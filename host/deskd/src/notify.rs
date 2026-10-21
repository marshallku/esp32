//! Desktop notifications, overheard on the session bus.
//!
//! deskd does not replace or reconfigure the notification daemon (dunst
//! here); it becomes a D-Bus *monitor* and watches `Notify` calls go past.
//! That needs no setup and sees every notification from every app.
//!
//! The one subtlety is the id. A `Notify` call carries `replaces_id` — usually
//! 0 — and the real id comes back in the daemon's *reply*. So calls are held
//! by (sender, serial) until the matching method return arrives, and only
//! then reported. Ids are namespaced by the daemon's unique bus name, so a
//! restarted daemon counting from 1 again cannot collide with old ones.

use std::{collections::HashMap, num::NonZeroU32, time::Duration};

use desk_core::protocol::{self, HostMsg, MAX_LINE, Note};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::{Connection, MatchRule, MessageStream, fdo::MonitoringProxy, message::Type, zvariant::OwnedValue};

use crate::session::Event;

const RETRY: Duration = Duration::from_secs(5);
/// Calls waiting for their reply. A daemon that never answers must not grow
/// this without bound.
const MAX_PENDING: usize = 64;

pub const APP_MAX: usize = 32;
pub const SUMMARY_MAX: usize = 100;
pub const BODY_MAX: usize = 300;

/// A notification as overheard, before deskd's store gives it an id.
#[derive(Clone, Debug, PartialEq)]
pub struct Incoming {
    /// The daemon's id, namespaced by its bus name.
    pub key: u64,
    pub app: String,
    pub summary: String,
    pub body: String,
    pub urgency: u8,
}

type NotifyArgs = (String, u32, String, String, String, Vec<String>, HashMap<String, OwnedValue>, i32);

pub async fn run(events: mpsc::Sender<Event>) {
    loop {
        if let Err(e) = watch(&events).await {
            eprintln!("notifications: {e:#}");
        }
        tokio::time::sleep(RETRY).await;
    }
}

async fn watch(events: &mpsc::Sender<Event>) -> zbus::Result<()> {
    let conn = Connection::session().await?;
    let rules = [
        MatchRule::builder()
            .msg_type(Type::MethodCall)
            .interface("org.freedesktop.Notifications")?
            .member("Notify")?
            .build(),
        // Replies cannot be filtered by interface; `Correlator` keeps only
        // the ones answering a Notify it saw.
        MatchRule::builder().msg_type(Type::MethodReturn).build(),
    ];
    MonitoringProxy::new(&conn).await?.become_monitor(&rules, 0).await?;
    eprintln!("notifications: watching the session bus");

    let mut stream = MessageStream::from(&conn);
    let mut correlator = Correlator::default();
    while let Some(msg) = stream.next().await {
        let msg = msg?;
        let header = msg.header();
        let event = match msg.message_type() {
            Type::MethodCall => {
                if let (Some(sender), Ok(args)) = (header.sender(), msg.body().deserialize::<NotifyArgs>()) {
                    let urgency = args.6.get("urgency").and_then(|v| u8::try_from(v).ok()).unwrap_or(1);
                    correlator.call(sender.to_string(), header.primary().serial_num(), Call {
                        app: args.0,
                        summary: args.3,
                        body: args.4,
                        urgency,
                    });
                }
                None
            }
            Type::MethodReturn => match (header.destination(), header.reply_serial(), header.sender()) {
                (Some(dest), Some(serial), Some(daemon)) => {
                    let id: u32 = msg.body().deserialize().unwrap_or(0);
                    correlator.reply(&dest.to_string(), serial, daemon, id).map(Event::Note)
                }
                _ => None,
            },
            _ => None,
        };
        if let Some(event) = event
            && events.send(event).await.is_err()
        {
            return Ok(());
        }
    }
    Ok(())
}

struct Call {
    app: String,
    summary: String,
    body: String,
    urgency: u8,
}

/// Holds Notify calls until the daemon's reply names their id.
#[derive(Default)]
struct Correlator {
    pending: Vec<((String, NonZeroU32), Call)>,
}

impl Correlator {
    fn call(&mut self, sender: String, serial: NonZeroU32, call: Call) {
        if self.pending.len() == MAX_PENDING {
            self.pending.remove(0);
        }
        self.pending.push(((sender, serial), call));
    }

    /// A method return from `daemon` to `dest` answering `serial`. Any other
    /// reply on the bus — the monitor sees them all — matches nothing.
    fn reply(&mut self, dest: &str, serial: NonZeroU32, daemon: impl ToString, id: u32) -> Option<Incoming> {
        let i = self.pending.iter().position(|((s, n), _)| s == dest && *n == serial)?;
        let (_, call) = self.pending.remove(i);
        let mut incoming = Incoming {
            key: namespaced(daemon, id),
            app: clip(&plain(&call.app), APP_MAX),
            summary: clip(&plain(&call.summary), SUMMARY_MAX),
            body: clip(&plain(&call.body), BODY_MAX),
            urgency: call.urgency.min(2),
        };
        fit_line(&mut incoming);
        Some(incoming)
    }
}

/// Shortens the body until the note, as the panel will receive it, fits one
/// protocol line. Character caps alone do not guarantee it: a 4-byte emoji
/// or an escaped quote costs several bytes per character, and a line the
/// panel cannot take is a line it never acks — every resync would resend it.
fn fit_line(incoming: &mut Incoming) {
    let encoded = |i: &Incoming| {
        let note = Note {
            id: u64::MAX,
            app: i.app.clone(),
            summary: i.summary.clone(),
            body: i.body.clone(),
            urgency: i.urgency,
            time: "00:00".into(),
            unread: true,
        };
        protocol::encode_host(&HostMsg::Note { note, live: true }).len()
    };
    while encoded(incoming) > MAX_LINE && !incoming.body.is_empty() {
        let keep = incoming.body.chars().count() / 2;
        incoming.body = clip(&incoming.body, keep.max(3));
        if keep < 3 {
            incoming.body.clear();
        }
    }
}

/// The daemon's id under a hash of its unique bus name.
fn namespaced(daemon: impl ToString, id: u32) -> u64 {
    // FNV-1a: stable across runs, unlike std's randomly keyed hasher.
    let hash = daemon.to_string().bytes().fold(0x811c_9dc5u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193));
    (u64::from(hash) << 32) | u64::from(id)
}

/// Notification text with its markup removed: the spec allows a small HTML
/// subset (`<b>`, `<i>`, `<a href>`, `<img>`) and the five XML entities.
/// Control characters other than newline become spaces — the panel cannot
/// draw them and JSON would spend six bytes escaping each.
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match (c, in_tag) {
            ('<', _) => in_tag = true,
            ('>', true) => in_tag = false,
            (c, false) if c.is_control() && c != '\n' => out.push(' '),
            (_, false) => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// At most `max` characters, ending in "..." when cut — ASCII, because the
/// panel's font subset may not have "…".
pub fn clip(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max - 3).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serial(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    fn call(summary: &str) -> Call {
        Call { app: "app".into(), summary: summary.into(), body: "<b>bold</b> &amp; more".into(), urgency: 1 }
    }

    #[test]
    fn reply_names_the_id_and_only_matching_replies_count() {
        let mut c = Correlator::default();
        c.call(":1.5".into(), serial(9), call("first"));
        assert_eq!(c.reply(":1.6", serial(9), ":1.61", 20), None, "reply to someone else");
        assert_eq!(c.reply(":1.5", serial(8), ":1.61", 20), None, "another call's reply");
        let got = c.reply(":1.5", serial(9), ":1.61", 20).unwrap();
        assert_eq!((got.summary.as_str(), got.body.as_str()), ("first", "bold & more"));
        assert_eq!(got.key & 0xFFFF_FFFF, 20);
        assert_eq!(c.reply(":1.5", serial(9), ":1.61", 20), None, "answered once");
    }

    #[test]
    fn ids_are_namespaced_by_daemon() {
        assert_ne!(namespaced(":1.61", 20), namespaced(":1.99", 20), "a restarted daemon reusing ids");
        assert_eq!(namespaced(":1.61", 20), namespaced(":1.61", 20));
    }

    #[test]
    fn pending_calls_are_bounded() {
        let mut c = Correlator::default();
        for n in 1..=(MAX_PENDING as u32 + 10) {
            c.call(":1.5".into(), serial(n), call("x"));
        }
        assert_eq!(c.pending.len(), MAX_PENDING);
        assert!(c.reply(":1.5", serial(1), ":1.61", 1).is_none(), "the oldest was dropped");
    }

    #[test]
    fn any_notification_fits_one_protocol_line() {
        let mut c = Correlator::default();
        let worst = Call {
            app: "😀".repeat(100),
            summary: "\"\u{1}".repeat(200),
            body: "😀\\".repeat(400),
            urgency: 2,
        };
        c.call(":1.5".into(), serial(1), worst);
        let got = c.reply(":1.5", serial(1), ":1.61", 1).unwrap();
        let note = Note {
            id: u64::MAX,
            app: got.app,
            summary: got.summary,
            body: got.body,
            urgency: got.urgency,
            time: "00:00".into(),
            unread: true,
        };
        let line = protocol::encode_host(&HostMsg::Note { note: note.clone(), live: true });
        assert!(line.len() <= MAX_LINE, "{} bytes", line.len());
        assert!(!note.summary.contains('\u{1}'), "control characters are gone");
    }

    #[test]
    fn markup_is_stripped_and_text_clipped() {
        assert_eq!(plain("<a href=\"x\">link</a> &lt;3"), "link <3");
        assert_eq!(plain("a\tb\nc\u{1b}"), "a b\nc ");
        assert_eq!(clip("가나다라마바", 5), "가나...");
        assert_eq!(clip("  short  ", 10), "short");
    }
}
