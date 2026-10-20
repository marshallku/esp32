//! OBS control over obs-websocket v5, with live state for the keys.
//!
//! `Machine` is the protocol, with no I/O: it takes each server message and
//! returns what to send back and which key states changed. `run` is the
//! connection loop around it. Keeping them apart is what lets the handshake,
//! auth and state mapping be tested without an OBS running.
//!
//! States published (see `states::is_known`):
//!   obs-connected, obs-recording, obs-streaming,
//!   obs-scene:<scene name> (true for the program scene),
//!   obs-muted:<input name>

use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::session::Event;

const RETRY: Duration = Duration::from_secs(3);

/// Scenes (1<<2) | Inputs (1<<3) | Outputs (1<<6): exactly the categories the
/// keys show. The high-volume meters stay off.
const SUBSCRIPTIONS: u32 = (1 << 2) | (1 << 3) | (1 << 6);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Scene(String),
    ToggleRecord,
    ToggleStream,
    ToggleMute(String),
}

/// How the rest of deskd reaches the OBS task.
#[derive(Clone)]
pub struct Handle {
    tx: mpsc::Sender<(u64, Command)>,
    /// The identified connection's number, or 0 while there is none.
    connection: Arc<AtomicU64>,
}

impl Handle {
    /// Queues a press for OBS, stamped with the connection it was made
    /// against, or drops it if OBS is not connected right now. The OBS task
    /// drops any press stamped with an earlier connection, so a press made
    /// just as OBS went away cannot fire once it comes back — least of all a
    /// recording or streaming toggle nobody remembers making.
    pub fn send(&self, cmd: Command) {
        let connection = self.connection.load(Ordering::SeqCst);
        if connection == 0 {
            eprintln!("OBS not connected; dropped {cmd:?}");
        } else if self.tx.try_send((connection, cmd)).is_err() {
            // try_send: a press must never stall the panel link on OBS.
            eprintln!("OBS command queue full; press dropped");
        }
    }
}

/// Starts the OBS connection task and returns its handle.
pub fn spawn(url: String, machine: Machine, events: mpsc::Sender<Event>) -> Handle {
    let (tx, rx) = mpsc::channel(8);
    let connection = Arc::new(AtomicU64::new(0));
    tokio::spawn(run(url, machine, events, rx, connection.clone()));
    Handle { tx, connection }
}

/// obs-websocket's authentication string:
/// base64(sha256(base64(sha256(password + salt)) + challenge)).
pub fn auth(password: &str, salt: &str, challenge: &str) -> String {
    let secret = B64.encode(Sha256::digest(format!("{password}{salt}")));
    B64.encode(Sha256::digest(format!("{secret}{challenge}")))
}

/// What one server message produced.
#[derive(Debug, Default, PartialEq)]
pub struct Output {
    pub send: Vec<Value>,
    pub states: Vec<(String, bool)>,
}

pub struct Machine {
    password: Option<String>,
    /// Inputs whose mute state some key shows; queried after every identify.
    inputs: BTreeSet<String>,
    scene: Option<String>,
    identified: bool,
    next_id: u64,
    /// `GetInputMute` answers with only `inputMuted`, not the input's name,
    /// so the name is remembered against the request id.
    mute_queries: HashMap<String, String>,
}

impl Machine {
    pub fn new(password: Option<String>, inputs: BTreeSet<String>) -> Self {
        Self { password, inputs, scene: None, identified: false, next_id: 0, mute_queries: HashMap::new() }
    }

    pub fn on_message(&mut self, msg: &Value) -> Result<Output> {
        let mut out = Output::default();
        let d = &msg["d"];
        match msg["op"].as_u64() {
            // Hello
            Some(0) => {
                let mut identify = json!({ "rpcVersion": 1, "eventSubscriptions": SUBSCRIPTIONS });
                if let Some(a) = d.get("authentication") {
                    let Some(password) = &self.password else {
                        bail!("OBS requires a password and none is configured");
                    };
                    let (salt, challenge) = (a["salt"].as_str().unwrap_or(""), a["challenge"].as_str().unwrap_or(""));
                    identify["authentication"] = json!(auth(password, salt, challenge));
                }
                out.send.push(json!({ "op": 1, "d": identify }));
            }
            // Identified: events only report changes, so ask for the
            // current state of everything a key shows — otherwise keys stay
            // wrong until something happens to change.
            Some(2) => {
                eprintln!("OBS: connected");
                self.identified = true;
                out.states.push(("obs-connected".into(), true));
                out.send.push(self.request("GetCurrentProgramScene", json!({})));
                out.send.push(self.request("GetRecordStatus", json!({})));
                out.send.push(self.request("GetStreamStatus", json!({})));
                self.mute_queries.clear();
                for input in self.inputs.clone() {
                    let req = self.request("GetInputMute", json!({ "inputName": input }));
                    self.mute_queries.insert(req["d"]["requestId"].as_str().unwrap_or_default().to_string(), input);
                    out.send.push(req);
                }
            }
            // Event
            Some(5) => {
                let data = &d["eventData"];
                match d["eventType"].as_str() {
                    Some("CurrentProgramSceneChanged") => self.set_scene(data["sceneName"].as_str(), &mut out),
                    Some("RecordStateChanged") => out.states.push(("obs-recording".into(), active(data))),
                    Some("StreamStateChanged") => out.states.push(("obs-streaming".into(), active(data))),
                    Some("InputMuteStateChanged") => {
                        // Only inputs a key shows: those are exactly the ones
                        // `disconnected` knows to switch off again.
                        if let (Some(input), Some(muted)) = (data["inputName"].as_str(), data["inputMuted"].as_bool())
                            && self.inputs.contains(input)
                        {
                            out.states.push((format!("obs-muted:{input}"), muted));
                        }
                    }
                    _ => {}
                }
            }
            // RequestResponse
            Some(7) => {
                if d["requestStatus"]["result"] != true {
                    eprintln!(
                        "OBS {} failed: {}",
                        d["requestType"].as_str().unwrap_or("?"),
                        d["requestStatus"]["comment"].as_str().unwrap_or("no reason given")
                    );
                    return Ok(out);
                }
                let data = &d["responseData"];
                match d["requestType"].as_str() {
                    Some("GetCurrentProgramScene") => {
                        let name = data["sceneName"].as_str().or(data["currentProgramSceneName"].as_str());
                        self.set_scene(name, &mut out);
                    }
                    Some("GetRecordStatus") => out.states.push(("obs-recording".into(), active(data))),
                    Some("GetStreamStatus") => out.states.push(("obs-streaming".into(), active(data))),
                    Some("GetInputMute") => {
                        let input = d["requestId"].as_str().and_then(|id| self.mute_queries.remove(id));
                        if let (Some(input), Some(muted)) = (input, data["inputMuted"].as_bool()) {
                            out.states.push((format!("obs-muted:{input}"), muted));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(out)
    }

    /// The request for a key press, or `None` before the session is
    /// identified — a toggle must not be queued and replayed later, when
    /// nobody remembers asking for it.
    pub fn command(&mut self, cmd: &Command) -> Option<Value> {
        if !self.identified {
            return None;
        }
        Some(match cmd {
            Command::Scene(scene) => self.request("SetCurrentProgramScene", json!({ "sceneName": scene })),
            Command::ToggleRecord => self.request("ToggleRecord", json!({})),
            Command::ToggleStream => self.request("ToggleStream", json!({})),
            Command::ToggleMute(input) => self.request("ToggleInputMute", json!({ "inputName": input })),
        })
    }

    /// Everything off, for when the connection drops: a key must not keep
    /// claiming "recording" about an OBS deskd can no longer see.
    pub fn disconnected(&mut self) -> Vec<(String, bool)> {
        self.identified = false;
        let mut states = vec![
            ("obs-connected".to_string(), false),
            ("obs-recording".to_string(), false),
            ("obs-streaming".to_string(), false),
        ];
        if let Some(scene) = self.scene.take() {
            states.push((format!("obs-scene:{scene}"), false));
        }
        states.extend(self.inputs.iter().map(|i| (format!("obs-muted:{i}"), false)));
        states
    }

    fn request(&mut self, kind: &str, data: Value) -> Value {
        self.next_id += 1;
        json!({ "op": 6, "d": { "requestType": kind, "requestId": self.next_id.to_string(), "requestData": data } })
    }

    fn set_scene(&mut self, name: Option<&str>, out: &mut Output) {
        let Some(name) = name else { return };
        if self.scene.as_deref() == Some(name) {
            return;
        }
        if let Some(old) = self.scene.replace(name.to_string()) {
            out.states.push((format!("obs-scene:{old}"), false));
        }
        out.states.push((format!("obs-scene:{name}"), true));
    }

}

/// Whether recording/streaming should show as on. `outputActive` is the
/// truth, with three overrides. STARTING already counts as on and STOPPING
/// already as off, so the key reacts the moment it is pressed rather than when
/// OBS finishes spinning up or down. PAUSED counts as on although OBS reports
/// `outputActive: false` for it: the recording still exists, and a key showing
/// "start" there would stop it when pressed.
fn active(data: &Value) -> bool {
    match data["outputState"].as_str() {
        Some("OBS_WEBSOCKET_OUTPUT_STARTING" | "OBS_WEBSOCKET_OUTPUT_PAUSED") => true,
        Some("OBS_WEBSOCKET_OUTPUT_STOPPING") => false,
        _ => data["outputActive"].as_bool().unwrap_or(false),
    }
}

/// The password, from `password_env` if set, else from OBS's own
/// obs-websocket config — deskd runs as the same user, so it can read the
/// file OBS itself reads, and nobody has to copy the password anywhere.
pub fn password(password_env: &str) -> Option<String> {
    if let Ok(p) = std::env::var(password_env) {
        return Some(p);
    }
    let path = PathBuf::from(std::env::var_os("HOME")?).join(".config/obs-studio/plugin_config/obs-websocket/config.json");
    let config: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    if config["auth_required"] == false {
        return None;
    }
    config["server_password"].as_str().map(str::to_string)
}

/// Keeps a connection to OBS for as long as deskd runs, reconnecting every
/// few seconds while OBS is closed.
async fn run(
    url: String,
    mut machine: Machine,
    events: mpsc::Sender<Event>,
    mut commands: mpsc::Receiver<(u64, Command)>,
    connection: Arc<AtomicU64>,
) {
    let mut announced_down = false;
    let mut number = 0;
    loop {
        number += 1;
        let result = session(&url, &mut machine, &events, &mut commands, &connection, number).await;
        connection.store(0, Ordering::SeqCst);
        // A session that got as far as identifying ends the previous outage,
        // so the next failure is news again.
        if machine.identified {
            eprintln!("OBS: disconnected");
            announced_down = false;
        }
        if let Err(e) = result
            && !announced_down
        {
            // Logged once per outage, not every retry: OBS being closed is
            // the normal state for most of the day.
            eprintln!("OBS: {e:#}");
            announced_down = true;
        }
        for (name, value) in machine.disconnected() {
            let _ = events.send(Event::State(name, value)).await;
        }
        tokio::time::sleep(RETRY).await;
    }
}

async fn session(
    url: &str,
    machine: &mut Machine,
    events: &mpsc::Sender<Event>,
    commands: &mut mpsc::Receiver<(u64, Command)>,
    connection: &AtomicU64,
    number: u64,
) -> Result<()> {
    let (ws, _) = tokio_tungstenite::connect_async(url).await.context("connecting")?;
    let (mut tx, mut rx) = ws.split();
    loop {
        tokio::select! {
            msg = rx.next() => {
                let text = match msg.context("connection closed")?? {
                    Message::Text(t) => t,
                    Message::Close(_) => bail!("OBS closed the connection"),
                    _ => continue,
                };
                let out = machine.on_message(&serde_json::from_str(&text)?)?;
                connection.store(if machine.identified { number } else { 0 }, Ordering::SeqCst);
                for v in out.send {
                    tx.send(Message::Text(v.to_string())).await?;
                }
                for (name, value) in out.states {
                    let _ = events.send(Event::State(name, value)).await;
                }
            }
            Some((made_on, cmd)) = commands.recv() => match machine.command(&cmd) {
                Some(req) if made_on == number => tx.send(Message::Text(req.to_string())).await?,
                _ => eprintln!("OBS press from an earlier connection; dropped {cmd:?}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identified(inputs: &[&str]) -> Machine {
        let mut m = Machine::new(None, inputs.iter().map(|s| s.to_string()).collect());
        m.on_message(&json!({ "op": 0, "d": { "rpcVersion": 1 } })).unwrap();
        m.on_message(&json!({ "op": 2, "d": { "negotiatedRpcVersion": 1 } })).unwrap();
        m
    }

    #[test]
    fn auth_matches_an_independent_implementation() {
        // The protocol doc's example inputs; the expected value was computed
        // separately with Python's hashlib, not by this code.
        let salt = "lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI=";
        let challenge = "+IxH4CnCiqpX1rM9scsNynZzbOe4KhDeYcTNS3PDaeY=";
        assert_eq!(auth("supersecretpassword", salt, challenge), "1Ct943GAT+6YQUUX47Ia/ncufilbe6+oD6lY+5kaCu4=");
    }

    #[test]
    fn hello_with_auth_identifies_with_subscriptions() {
        let mut m = Machine::new(Some("pw".into()), BTreeSet::new());
        let hello = json!({ "op": 0, "d": { "rpcVersion": 1, "authentication": { "salt": "s", "challenge": "c" } } });
        let out = m.on_message(&hello).unwrap();
        assert_eq!(out.send[0]["op"], 1);
        assert_eq!(out.send[0]["d"]["authentication"], auth("pw", "s", "c"));
        assert_eq!(out.send[0]["d"]["eventSubscriptions"], 76);
        assert!(Machine::new(None, BTreeSet::new()).on_message(&hello).is_err(), "auth required, no password");
    }

    #[test]
    fn identified_queries_current_state_of_everything_shown() {
        let mut m = Machine::new(None, ["Mic/Aux".to_string()].into());
        let out = m.on_message(&json!({ "op": 2, "d": {} })).unwrap();
        let kinds: Vec<_> = out.send.iter().map(|v| v["d"]["requestType"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["GetCurrentProgramScene", "GetRecordStatus", "GetStreamStatus", "GetInputMute"]);
        assert_eq!(out.states, [("obs-connected".to_string(), true)]);
    }

    #[test]
    fn mute_query_answers_are_matched_by_request_id() {
        // The real response carries only `inputMuted` — found against OBS 32,
        // where reading `inputName` from it silently produced nothing.
        let mut m = Machine::new(None, ["Desktop Audio".to_string(), "Mic/Aux".to_string()].into());
        let sent = m.on_message(&json!({ "op": 2, "d": {} })).unwrap().send;
        let id_of = |name: &str| {
            sent.iter().find(|r| r["d"]["requestData"]["inputName"] == name).unwrap()["d"]["requestId"].clone()
        };
        let resp = json!({ "op": 7, "d": { "requestType": "GetInputMute", "requestId": id_of("Mic/Aux"),
            "requestStatus": { "result": true, "code": 100 }, "responseData": { "inputMuted": true } } });
        assert_eq!(m.on_message(&resp).unwrap().states, [("obs-muted:Mic/Aux".to_string(), true)]);
        assert!(m.on_message(&resp).unwrap().states.is_empty(), "an id is answered once");
    }

    #[test]
    fn scene_changes_move_the_on_state() {
        let mut m = identified(&[]);
        let resp = json!({ "op": 7, "d": { "requestType": "GetCurrentProgramScene", "requestStatus": { "result": true },
            "responseData": { "sceneName": "Desk" } } });
        assert_eq!(m.on_message(&resp).unwrap().states, [("obs-scene:Desk".to_string(), true)]);
        let ev = json!({ "op": 5, "d": { "eventType": "CurrentProgramSceneChanged", "eventData": { "sceneName": "BRB" } } });
        assert_eq!(
            m.on_message(&ev).unwrap().states,
            [("obs-scene:Desk".to_string(), false), ("obs-scene:BRB".to_string(), true)]
        );
    }

    #[test]
    fn output_states_count_starting_as_on() {
        let mut m = identified(&[]);
        let ev = |active: bool, state: &str| json!({ "op": 5, "d": { "eventType": "RecordStateChanged",
            "eventData": { "outputActive": active, "outputState": state } } });
        let on = |m: &mut Machine, active, state| m.on_message(&ev(active, state)).unwrap().states[0].1;
        assert!(on(&mut m, false, "OBS_WEBSOCKET_OUTPUT_STARTING"));
        assert!(on(&mut m, true, "OBS_WEBSOCKET_OUTPUT_STARTED"));
        // OBS sends outputActive: false for PAUSED, yet the recording is still
        // in progress: a key showing "start" here would stop it when pressed.
        assert!(on(&mut m, false, "OBS_WEBSOCKET_OUTPUT_PAUSED"));
        assert!(on(&mut m, true, "OBS_WEBSOCKET_OUTPUT_RESUMED"));
        assert!(!on(&mut m, true, "OBS_WEBSOCKET_OUTPUT_STOPPING"));
        assert!(!on(&mut m, false, "OBS_WEBSOCKET_OUTPUT_STOPPED"));
    }

    #[test]
    fn commands_are_dropped_until_identified_and_cleared_on_disconnect() {
        let mut m = Machine::new(None, ["Mic".to_string()].into());
        assert_eq!(m.command(&Command::ToggleRecord), None);
        let mut m = identified(&["Mic"]);
        let req = m.command(&Command::ToggleMute("Mic".into())).unwrap();
        assert_eq!((req["op"].as_u64(), req["d"]["requestData"]["inputName"].as_str()), (Some(6), Some("Mic")));
        let off = m.disconnected();
        assert!(off.iter().all(|(_, v)| !v));
        assert!(off.contains(&("obs-muted:Mic".to_string(), false)));
        assert_eq!(m.command(&Command::ToggleRecord), None);
    }

    #[test]
    fn mute_events_for_untracked_inputs_are_ignored() {
        let mut m = identified(&["Mic/Aux"]);
        let ev = |name: &str| json!({ "op": 5, "d": { "eventType": "InputMuteStateChanged",
            "eventData": { "inputName": name, "inputMuted": true } } });
        assert!(m.on_message(&ev("Desktop Audio")).unwrap().states.is_empty());
        assert_eq!(m.on_message(&ev("Mic/Aux")).unwrap().states, [("obs-muted:Mic/Aux".to_string(), true)]);
    }

    #[tokio::test]
    async fn presses_carry_the_connection_they_were_made_on() {
        let (tx, mut rx) = mpsc::channel(8);
        let handle = Handle { tx, connection: Arc::new(AtomicU64::new(0)) };
        handle.send(Command::ToggleRecord);
        assert!(rx.try_recv().is_err(), "refused at press time, not queued for later");
        handle.connection.store(3, Ordering::SeqCst);
        handle.send(Command::ToggleRecord);
        // The task compares this stamp with its current connection and drops
        // a mismatch, which closes the gap between this check and the send.
        assert_eq!(rx.try_recv().unwrap(), (3, Command::ToggleRecord));
    }

    #[test]
    fn failed_requests_change_nothing() {
        let mut m = identified(&[]);
        let resp = json!({ "op": 7, "d": { "requestType": "GetRecordStatus",
            "requestStatus": { "result": false, "code": 600, "comment": "nope" } } });
        assert_eq!(m.on_message(&resp).unwrap(), Output::default());
    }
}
