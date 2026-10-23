//! Line-oriented JSON over the board's one serial port.
//!
//! The UART also carries esp-println logs and the ROM's boot chatter, so the
//! device prefixes every protocol line with `@` and the host ignores anything
//! else. The host sends bare JSON: nothing else talks to the device's RX.
//!
//! Flow control is stop-and-wait: the device answers every host line with
//! `ack`, and the host sends nothing more until it arrives. A line is at most
//! `MAX_LINE` bytes, which the device's RX ring always has room for, so a burst
//! can never outrun it while it is busy drawing.
//!
//! Layouts are versioned by `gen`. A replacement arrives as `begin`, one
//! `page` per page, then `commit`; the device swaps only on a complete commit,
//! and every `press` echoes the gen it was made against, so a press on the old
//! layout cannot run whatever now sits at the same position.

use alloc::{string::String, vec::Vec};

use serde::{Deserialize, Serialize};

/// Bumped on any incompatible change. `hello` carries it both ways and the
/// host refuses a device that disagrees.
pub const PROTO: u16 = 3;

/// Longest line either side may send, newline included. Sized so a full page
/// of keys at the label caps in `text` fits even when every character needs a
/// JSON escape (see the test below).
pub const MAX_LINE: usize = 2048;

pub const MAX_PAGES: usize = 8;
pub const KEYS_PER_PAGE: usize = 9;
pub const MAX_BUTTONS: usize = 3;

/// Device-to-host lines start with this; anything else on the wire is a log.
pub const DEVICE_PREFIX: char = '@';

/// One key as drawn. Colours are 0xRRGGBB.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sub: String,
    /// A name from `icons::NAMES`, drawn above the label. The panel draws
    /// none for a name it does not have.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    pub bg: u32,
    pub fg: u32,
    /// A toggle in its active state. Drawn inverted with an accent bar, so it
    /// reads as "on" without relying on the colour the user picked.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub on: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// The panel's screens, in swipe order: dashboard, PC stats, notifications,
/// then the button pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenId {
    Dash,
    Stats,
    Notes,
    Page(u8),
}

/// One sample of the PC's vitals. Every reading is optional: a missing sensor
/// is drawn as a dash, not a confident zero.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    /// Whole-CPU busy %.
    pub cpu: Option<u8>,
    /// Per-core busy %, at most `MAX_CORES` (larger CPUs are averaged down).
    #[serde(default)]
    pub cores: Vec<u8>,
    /// °C.
    pub cpu_temp: Option<i16>,
    /// MiB.
    pub mem_used: Option<u32>,
    pub mem_total: Option<u32>,
    pub gpu: Option<Gpu>,
    /// KiB/s summed over every non-loopback interface.
    pub net_rx: Option<u32>,
    pub net_tx: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gpu {
    pub util: Option<u8>,
    pub temp: Option<i16>,
    /// MiB.
    pub vram_used: Option<u32>,
    pub vram_total: Option<u32>,
}

pub const MAX_CORES: usize = 16;

/// Notifications the panel keeps; older ones fall off the end.
pub const MAX_NOTES: usize = 20;
/// freedesktop urgency: 0 low, 1 normal, 2 critical.
pub const URGENCY_CRITICAL: u8 = 2;

/// A desktop notification as the panel shows it. Text is plain (deskd strips
/// markup) and may be any script; the panel draws it with a Korean-capable
/// font.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// deskd's id, stable across panel reconnects.
    pub id: u64,
    pub app: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    pub urgency: u8,
    /// Local "HH:MM" it arrived.
    pub time: String,
    #[serde(default)]
    pub unread: bool,
}

/// A finished touch, as the gesture recogniser reports it and as the host can
/// inject it for testing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "g", rename_all = "snake_case")]
pub enum Gesture {
    Tap { x: u16, y: u16 },
    Long { x: u16, y: u16 },
    Swipe { dir: Dir },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HostMsg {
    Hello { proto: u16 },
    Ping,
    /// `title` names the profile (the default one, or the focused app's) for
    /// the panel's status strip.
    Begin {
        generation: u32,
        pages: u8,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        title: String,
    },
    Page { generation: u32, index: u8, keys: Vec<Key> },
    Commit { generation: u32 },
    /// Live update of one key in the active layout.
    Key { generation: u32, page: u8, index: u8, key: Key },
    Goto { screen: ScreenId },
    /// Wall-clock text for the status strip; the board has no clock of its own.
    Time { text: String },
    /// A modal that takes over the screen until answered or dismissed.
    Prompt { id: u64, title: String, body: String, buttons: Vec<String> },
    Dismiss { id: u64 },
    /// The PC's vitals, about once a second.
    Stats(Stats),
    /// Empties the panel's notification list; a resync follows as `note`s.
    NotesReset,
    /// A notification. `live` ones just arrived and get the banner and the
    /// blinking border; the rest are a resync of history.
    Note { note: Note, live: bool },
    /// Test hook: behave as if this gesture had just been made on the glass.
    Inject { gesture: Gesture },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum DeviceMsg {
    Hello { proto: u16, fw: String },
    Ack,
    Pong,
    Press { generation: u32, page: u8, index: u8, long: bool },
    Answer { id: u64, button: u8 },
    /// The screen now showing.
    Screen { screen: ScreenId },
    /// The notifications screen was opened: everything on it has been seen.
    NotesSeen,
    /// The user removed one notification from the panel.
    NoteDismiss { id: u64 },
    /// The user cleared the panel's notification list.
    NotesClear,
    Err { msg: String },
}

/// `msg` as one device line, prefix and newline included.
pub fn encode_device(msg: &DeviceMsg) -> String {
    let mut line = String::from(DEVICE_PREFIX);
    line.push_str(&serde_json::to_string(msg).expect("protocol types always serialise"));
    line.push('\n');
    line
}

/// `msg` as one host line, newline included.
pub fn encode_host(msg: &HostMsg) -> String {
    let mut line = serde_json::to_string(msg).expect("protocol types always serialise");
    line.push('\n');
    line
}

/// One line from the device. `None` for log lines and boot noise; the host
/// should pass those through to its own log rather than treat them as errors.
///
/// A protocol line can arrive glued to the tail of a truncated boot log line
/// — seen on hardware right after a reset — so the message is taken from the
/// first `@{` anywhere in the line, not only at its start.
pub fn decode_device(line: &str) -> Option<Result<DeviceMsg, serde_json::Error>> {
    let line = line.trim_end_matches(['\r', '\n']);
    let start = line.find("@{")?;
    Some(serde_json::from_str(&line[start + DEVICE_PREFIX.len_utf8()..]))
}

pub fn decode_host(line: &str) -> Result<HostMsg, serde_json::Error> {
    serde_json::from_str(line.trim_end_matches(['\r', '\n']))
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn worst_case_page_fits_one_line() {
        use crate::text::{LABEL_MAX, SUB_MAX};
        // Every character a quote: each costs two bytes once escaped.
        let longest_icon = crate::icons::NAMES.iter().max_by_key(|n| n.len()).unwrap();
        let full_page = HostMsg::Page {
            generation: u32::MAX,
            index: u8::MAX,
            keys: vec![
                Key {
                    label: "\"".repeat(LABEL_MAX),
                    sub: "\\".repeat(SUB_MAX),
                    icon: (*longest_icon).into(),
                    bg: 0xFFFFFF,
                    fg: 0xFFFFFF,
                    on: true,
                };
                KEYS_PER_PAGE
            ],
        };
        let line = encode_host(&full_page);
        // A worst-case page has to fit one line, or the stop-and-wait framing
        // breaks for exactly the messages that matter most.
        assert!(line.len() <= MAX_LINE, "full page is {} bytes", line.len());
        assert_eq!(decode_host(&line).unwrap(), full_page);
    }

    #[test]
    fn device_lines_are_prefixed_and_logs_are_skipped() {
        let msg = DeviceMsg::Press { generation: 3, page: 1, index: 4, long: false };
        let line = encode_device(&msg);
        assert!(line.starts_with('@') && line.ends_with('\n'));
        assert_eq!(decode_device(&line).unwrap().unwrap(), msg);
        assert!(decode_device("I (157) boot: Loaded app\r\n").is_none());
        assert!(decode_device("user@host: ok").is_none(), "a bare @ is not a message");
        let glued = alloc::format!("rst:0x1 (POWE{line}");
        assert_eq!(decode_device(&glued).unwrap().unwrap(), msg);
    }

    #[test]
    fn screens_and_stats_round_trip() {
        let msg = HostMsg::Goto { screen: ScreenId::Page(3) };
        assert_eq!(encode_host(&msg), "{\"t\":\"goto\",\"screen\":{\"page\":3}}\n");
        assert_eq!(decode_host(&encode_host(&msg)).unwrap(), msg);
        let stats = HostMsg::Stats(Stats {
            cpu: Some(12),
            cores: vec![1; MAX_CORES],
            gpu: Some(Gpu { util: Some(3), ..Default::default() }),
            ..Default::default()
        });
        let line = encode_host(&stats);
        assert!(line.len() < 512, "a stats line is small: {}", line.len());
        assert_eq!(decode_host(&line).unwrap(), stats);
    }

    #[test]
    fn a_long_korean_note_fits_one_line() {
        // deskd caps text by characters; Hangul is three bytes in UTF-8.
        let note = HostMsg::Note {
            note: Note {
                id: u64::MAX,
                app: "가".repeat(32),
                summary: "나".repeat(100),
                body: "다".repeat(300),
                urgency: URGENCY_CRITICAL,
                time: "23:59".into(),
                unread: true,
            },
            live: true,
        };
        let line = encode_host(&note);
        assert!(line.len() <= MAX_LINE, "{} bytes", line.len());
        assert_eq!(decode_host(&line).unwrap(), note);
    }

    #[test]
    fn large_ids_survive_json() {
        let msg = HostMsg::Dismiss { id: u64::MAX };
        assert_eq!(decode_host(&encode_host(&msg)).unwrap(), msg);
    }

    #[test]
    fn optional_key_fields_default() {
        let key: Key = serde_json::from_str(r#"{"label":"A","bg":0,"fg":16777215}"#).unwrap();
        assert_eq!(key.sub, "");
        assert!(!key.on);
    }
}
