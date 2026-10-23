//! Live state sources that keys can show: polled, reported on every poll,
//! deduplicated by `Deck::set_state`.

use std::{collections::BTreeMap, process::Stdio, time::Duration};

use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::Instant,
};

use crate::{
    actions::{SINK, SOURCE},
    config::CommandState,
    session::Event,
};

const POLL: Duration = Duration::from_millis(700);

/// Longest a state command may run before that poll is given up.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// For `BURST` after a key press, command states are polled every
/// `BURST_EVERY`: a key that toggles one (a VPN, a service) then shows the
/// result within a second instead of at its next regular poll.
const BURST: Duration = Duration::from_secs(6);
const BURST_EVERY: Duration = Duration::from_secs(1);

/// Fixed names a key's `state` can refer to. OBS adds per-scene and
/// per-input names on top; see `is_known`.
pub const NAMES: &[&str] = &["mic-muted", "muted", "obs-connected", "obs-recording", "obs-streaming"];

pub const OBS_SCENE: &str = "obs-scene:";
pub const OBS_MUTED: &str = "obs-muted:";

pub fn is_known(name: &str) -> bool {
    NAMES.contains(&name)
        || [OBS_SCENE, OBS_MUTED].iter().any(|p| name.strip_prefix(p).is_some_and(|rest| !rest.is_empty()))
}

pub async fn poll_audio(events: mpsc::Sender<Event>) {
    let mut tick = tokio::time::interval(POLL);
    loop {
        tick.tick().await;
        for (name, node) in [("mic-muted", SOURCE), ("muted", SINK)] {
            if let Some(muted) = wpctl_muted(node).await
                && events.send(Event::State(name.into(), muted)).await.is_err()
            {
                return;
            }
        }
    }
}

/// `wpctl get-volume` prints `Volume: 0.40 [MUTED]` for a muted node. `None`
/// when wpctl is missing or the node does not exist — a key bound to it then
/// just stays in its last state rather than flipping to a guess.
async fn wpctl_muted(node: &str) -> Option<bool> {
    let out = tokio::process::Command::new("wpctl").args(["get-volume", node]).output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_muted(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_muted(text: &str) -> bool {
    text.contains("[MUTED]")
}

/// The `[state.<name>]` commands, polled for as long as deskd runs. A reload
/// replaces the whole table.
pub struct Commands {
    table: watch::Sender<BTreeMap<String, CommandState>>,
    pressed: watch::Sender<Instant>,
}

impl Commands {
    pub fn spawn(table: BTreeMap<String, CommandState>, events: mpsc::Sender<Event>) -> Self {
        let (table, table_rx) = watch::channel(table);
        let (pressed, pressed_rx) = watch::channel(Instant::now());
        tokio::spawn(supervise(table_rx, pressed_rx, events));
        Self { table, pressed }
    }

    pub fn replace(&self, table: BTreeMap<String, CommandState>) {
        self.table.send_if_modified(|current| {
            let changed = *current != table;
            *current = table;
            changed
        });
    }

    /// A key ran something; it may have changed what a command reports.
    pub fn pressed(&self) {
        self.pressed.send_replace(Instant::now());
    }
}

/// One poller per command, restarted whenever the table changes.
async fn supervise(
    mut table: watch::Receiver<BTreeMap<String, CommandState>>,
    pressed: watch::Receiver<Instant>,
    events: mpsc::Sender<Event>,
) {
    loop {
        let mut pollers = JoinSet::new();
        for (name, state) in table.borrow_and_update().clone() {
            pollers.spawn(poll_command(name, state, pressed.clone(), events.clone()));
        }
        if table.changed().await.is_err() {
            return;
        }
        // Dropping the set aborts the pollers of the old table.
        drop(pollers);
    }
}

async fn poll_command(
    name: String,
    state: CommandState,
    mut pressed: watch::Receiver<Instant>,
    events: mpsc::Sender<Event>,
) {
    let every = Duration::from_secs(state.every);
    let mut burst_until = None;
    // Logged when a command starts failing, not on every poll after that.
    let mut failing = false;
    loop {
        match check(&state.cmd, COMMAND_TIMEOUT).await {
            Ok(value) => {
                failing = false;
                if events.send(Event::State(name.clone(), value)).await.is_err() {
                    return;
                }
            }
            Err(e) => {
                if !failing {
                    eprintln!("state '{name}': {e}");
                }
                failing = true;
            }
        }
        let wait = match burst_until {
            Some(until) if Instant::now() < until => BURST_EVERY.min(every),
            _ => every,
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            changed = pressed.changed() => {
                if changed.is_err() {
                    return;
                }
                burst_until = Some(Instant::now() + BURST);
            }
        }
    }
}

/// Whether `cmd` exits 0. A command that cannot start or does not finish
/// within `limit` reports nothing, so its keys keep their last state rather
/// than a guess.
async fn check(cmd: &str, limit: Duration) -> Result<bool, String> {
    let mut child = tokio::process::Command::new("sh")
        .args(["-c", cmd])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not run: {e}"))?;
    // Killing `sh` alone would leave a pipeline or `sleep` it started
    // running after a timeout or a reload, one more on every poll.
    let group = child.id().map(Group);
    let result = tokio::time::timeout(limit, child.wait()).await;
    if let Ok(Ok(status)) = result {
        // Reaped: the group's id may already belong to someone else.
        std::mem::forget(group);
        return Ok(status.success());
    }
    match result {
        Ok(Err(e)) => Err(format!("could not wait: {e}")),
        _ => Err(format!("no answer within {}s", limit.as_secs_f32())),
    }
}

/// A command's process group, killed when its poll is given up — including
/// by the poller being aborted mid-poll. Dropped before the `Child`, while
/// the unreaped leader still holds the id.
struct Group(u32);

impl Drop for Group {
    fn drop(&mut self) {
        let pgid = nix::unistd::Pid::from_raw(self.0 as i32);
        let _ = nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obs_state_names_need_a_subject() {
        assert!(is_known("obs-scene:Desk"));
        assert!(is_known("obs-recording"));
        assert!(!is_known("obs-scene:"));
        assert!(!is_known("cpu-hot"));
    }

    #[test]
    fn reads_wpctl_mute_flag() {
        assert!(parse_muted("Volume: 0.40 [MUTED]\n"));
        assert!(!parse_muted("Volume: 1.00\n"));
    }

    fn table(entries: &[(&str, &str)]) -> BTreeMap<String, CommandState> {
        entries.iter().map(|(n, c)| (n.to_string(), CommandState { cmd: c.to_string(), every: 3600 })).collect()
    }

    async fn next_state(rx: &mut mpsc::Receiver<Event>) -> (String, bool) {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(Event::State(name, value))) => (name, value),
            _ => panic!("no state reported"),
        }
    }

    #[tokio::test]
    async fn command_states_follow_the_exit_status_and_reloads() {
        let (tx, mut rx) = mpsc::channel(8);
        let commands = Commands::spawn(table(&[("on", "true"), ("off", "exit 3")]), tx);
        let mut seen = BTreeMap::new();
        for _ in 0..2 {
            let (name, value) = next_state(&mut rx).await;
            seen.insert(name, value);
        }
        assert_eq!(seen, BTreeMap::from([("off".to_string(), false), ("on".to_string(), true)]));

        // Polled hourly, so this answer comes from the reload itself.
        commands.replace(table(&[("new", "true")]));
        assert_eq!(next_state(&mut rx).await, ("new".to_string(), true));

        // And this one from the press, well inside the hour.
        commands.pressed();
        assert_eq!(next_state(&mut rx).await, ("new".to_string(), true));
    }

    #[tokio::test]
    async fn a_hung_command_reports_nothing() {
        let limit = Duration::from_millis(300);
        assert!(check("exit 0", limit).await.unwrap());
        assert!(!check("false", limit).await.unwrap());
        let started = std::time::Instant::now();
        assert!(check("sleep 5", limit).await.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn a_given_up_command_takes_its_children_with_it() {
        let marker = format!("deskd-orphan-{}", std::process::id());
        // `exec -a` names the sleep so it can be found; `; true` keeps sh
        // from exec'ing it, so it is sh's child, not sh itself.
        let cmd = format!("bash -c 'exec -a {marker} sleep 30'; true");
        assert!(check(&cmd, Duration::from_millis(300)).await.is_err());
        tokio::time::sleep(Duration::from_millis(100)).await;
        let left = std::process::Command::new("pgrep").args(["-f", &marker]).output().unwrap();
        assert!(left.stdout.is_empty(), "left running: {}", String::from_utf8_lossy(&left.stdout));
    }
}
