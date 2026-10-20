//! One connection to the panel, from open to unplug, and the loop that waits
//! for the next one.
//!
//! Everything that touches the panel goes through `Session`, which enforces
//! the protocol's stop-and-wait framing: one host line in flight, the next
//! only after its `ack`. Every handler only ever queues messages, so ordering
//! is decided in one place.

use std::{
    collections::VecDeque,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use desk_core::protocol::{self, DeviceMsg, HostMsg, PROTO};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::{
    actions::{self, Effect},
    config,
    control::Request,
    deck::Deck,
    obs, serial,
};

const ACK_TIMEOUT: Duration = Duration::from_secs(1);
/// Resends of one line before the link is declared dead.
const RETRIES: u8 = 2;
const PING_EVERY: Duration = Duration::from_secs(2);
/// Matches the panel's own `LINK_TIMEOUT_MS`: both ends give up together.
const SILENCE: Duration = Duration::from_secs(6);

pub enum Event {
    State(String, bool),
    Control(Request, oneshot::Sender<Value>),
}

/// Waits for the panel, runs a session, and starts over when it ends. Never
/// returns: deskd's job is to be there whenever the board is.
pub async fn run(
    mut deck: Deck,
    config_path: PathBuf,
    mut events: mpsc::Receiver<Event>,
    obs: Option<obs::Handle>,
) -> ! {
    loop {
        let Some(path) = serial::find() else {
            idle(&mut deck, &mut events, "panel not plugged in").await;
            continue;
        };
        let (port, lines) = match serial::open(&path) {
            Ok(opened) => opened,
            Err(e) => {
                eprintln!("{e:#}");
                idle(&mut deck, &mut events, "panel port could not be opened").await;
                continue;
            }
        };
        eprintln!("panel port {} open", path.display());
        let mut session = Session::new(port, obs.clone());
        let reason = session.run(&mut deck, &config_path, &mut events, lines).await;
        eprintln!("session ended: {reason:#}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// One second without a panel: state values are still recorded — OBS answers
/// its initial queries whether or not the panel is plugged in, and they go
/// out with the next push — and control requests get a reply instead of
/// hanging.
async fn idle(deck: &mut Deck, events: &mut mpsc::Receiver<Event>, why: &str) {
    let deadline = tokio::time::sleep(Duration::from_secs(1));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return,
            Some(ev) = events.recv() => match ev {
                Event::State(name, value) => {
                    deck.set_state(&name, value);
                }
                Event::Control(_, reply) => {
                    let _ = reply.send(json!({ "ok": false, "connected": false, "error": why }));
                }
            },
        }
    }
}

struct Session {
    port: File,
    outbox: VecDeque<HostMsg>,
    in_flight: Option<InFlight>,
    last_rx: Instant,
    last_ping: Instant,
    clock: String,
    /// Set once the panel has answered `hello` with our protocol version.
    fw: Option<String>,
    screen: Option<u8>,
    obs: Option<obs::Handle>,
}

struct InFlight {
    msg: HostMsg,
    sent: Instant,
    tries: u8,
}

impl Session {
    fn new(port: File, obs: Option<obs::Handle>) -> Self {
        let now = Instant::now();
        Self {
            port,
            outbox: VecDeque::from([HostMsg::Hello { proto: PROTO }]),
            in_flight: None,
            last_rx: now,
            last_ping: now,
            clock: String::new(),
            fw: None,
            screen: None,
            obs,
        }
    }

    async fn run(
        &mut self,
        deck: &mut Deck,
        config_path: &Path,
        events: &mut mpsc::Receiver<Event>,
        mut lines: mpsc::Receiver<String>,
    ) -> anyhow::Error {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            let step = tokio::select! {
                line = lines.recv() => match line {
                    Some(line) => self.on_line(&line, deck),
                    None => Err(anyhow::anyhow!("port closed")),
                },
                Some(ev) = events.recv() => {
                    self.on_event(ev, deck, config_path);
                    Ok(())
                },
                _ = tick.tick() => self.on_tick(),
            };
            if let Err(e) = step.and_then(|()| self.pump()) {
                return e;
            }
        }
    }

    fn on_line(&mut self, line: &str, deck: &mut Deck) -> Result<()> {
        let msg = match protocol::decode_device(line) {
            None => {
                // The panel's own log, interleaved on the same wire.
                if !line.trim().is_empty() {
                    eprintln!("panel: {line}");
                }
                return Ok(());
            }
            Some(Err(e)) => {
                eprintln!("unparseable panel line ({e}): {line}");
                return Ok(());
            }
            Some(Ok(msg)) => msg,
        };
        self.last_rx = Instant::now();

        match msg {
            DeviceMsg::Hello { proto, fw } => {
                if proto != PROTO {
                    bail!("panel speaks protocol {proto}, deskd speaks {PROTO}; reflash one of them");
                }
                eprintln!("panel ready: {fw}");
                self.fw = Some(fw);
                // Either the answer to our hello or the panel announcing a
                // reboot. In both cases it holds nothing deskd sent before, so
                // start clean: whatever was queued or in flight was for a
                // panel that no longer exists.
                self.outbox.clear();
                // Except our own hello: if that is what is in flight, its ack
                // is still coming, and dropping it here would let that ack be
                // taken for the next line's.
                if !self.in_flight.as_ref().is_some_and(|f| matches!(f.msg, HostMsg::Hello { .. })) {
                    self.in_flight = None;
                }
                self.clock.clear();
                self.queue_clock();
                self.outbox.extend(deck.push());
            }
            DeviceMsg::Ack => self.in_flight = None,
            DeviceMsg::Pong => {}
            DeviceMsg::Press { generation, page, index, long } => match deck.resolve(generation, page, index) {
                Some(action) => {
                    eprintln!("press {page}/{index}{}: {action:?}", if long { " (long)" } else { "" });
                    match action.effect() {
                        Effect::Run(argv) => actions::spawn(argv),
                        Effect::Goto(page) => self.outbox.push_back(HostMsg::Goto { page }),
                        Effect::Obs(cmd) => match &self.obs {
                            Some(obs) => obs.send(cmd),
                            None => eprintln!("OBS key pressed but OBS support is not running"),
                        },
                    }
                }
                None => eprintln!("ignored press {page}/{index} against stale generation {generation}"),
            },
            DeviceMsg::Answer { id, button } => eprintln!("answer {button} to prompt {id} with nothing waiting"),
            DeviceMsg::Screen { page } => self.screen = page,
            DeviceMsg::Err { msg } => eprintln!("panel rejected a message: {msg}"),
        }
        Ok(())
    }

    fn on_event(&mut self, ev: Event, deck: &mut Deck, config_path: &Path) {
        match ev {
            Event::State(name, value) => {
                let updates = deck.set_state(&name, value);
                if self.fw.is_some() {
                    self.outbox.extend(updates);
                }
            }
            Event::Control(req, reply) => {
                let answer = match req {
                    Request::Status => json!({
                        "ok": true,
                        "connected": true,
                        "ready": self.fw.is_some(),
                        "fw": self.fw,
                        "screen": self.screen,
                    }),
                    Request::States => json!({ "ok": true, "states": deck.states() }),
                    Request::Inject { gesture } if self.fw.is_some() => {
                        self.outbox.push_back(HostMsg::Inject { gesture });
                        json!({ "ok": true })
                    }
                    Request::Inject { .. } => json!({ "ok": false, "error": "panel not ready" }),
                    Request::Reload => match config::load(config_path) {
                        Ok(config) => {
                            deck.replace(config);
                            if self.fw.is_some() {
                                self.outbox.extend(deck.push());
                            }
                            json!({ "ok": true })
                        }
                        Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
                    },
                };
                let _ = reply.send(answer);
            }
        }
    }

    fn on_tick(&mut self) -> Result<()> {
        let now = Instant::now();
        if now - self.last_rx > SILENCE {
            bail!("panel silent for {}s", SILENCE.as_secs());
        }
        if let Some(f) = &mut self.in_flight
            && now - f.sent > ACK_TIMEOUT
        {
            if f.tries > RETRIES {
                bail!("no ack after {} tries", f.tries);
            }
            f.tries += 1;
            f.sent = now;
            let line = protocol::encode_host(&f.msg);
            self.port.write_all(line.as_bytes())?;
        }
        if now - self.last_ping > PING_EVERY {
            self.last_ping = now;
            // Before the handshake completes the ping is a hello, so a panel
            // that missed the first one (still booting) gets asked again.
            self.outbox.push_back(if self.fw.is_some() { HostMsg::Ping } else { HostMsg::Hello { proto: PROTO } });
        }
        if self.fw.is_some() {
            self.queue_clock();
        }
        Ok(())
    }

    fn queue_clock(&mut self) {
        let now = local_hm();
        if now != self.clock {
            self.clock = now.clone();
            self.outbox.push_back(HostMsg::Time { text: now });
        }
    }

    /// Sends the next queued line if nothing is awaiting an ack.
    fn pump(&mut self) -> Result<()> {
        if self.in_flight.is_some() {
            return Ok(());
        }
        let Some(msg) = self.outbox.pop_front() else { return Ok(()) };
        let line = protocol::encode_host(&msg);
        self.port.write_all(line.as_bytes())?;
        self.in_flight = Some(InFlight { msg, sent: Instant::now(), tries: 1 });
        Ok(())
    }
}

/// Local wall-clock "HH:MM".
fn local_hm() -> String {
    // SAFETY: localtime_r only writes the struct it is given.
    unsafe {
        let t = nix::libc::time(std::ptr::null_mut());
        let mut tm: nix::libc::tm = std::mem::zeroed();
        nix::libc::localtime_r(&t, &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}
