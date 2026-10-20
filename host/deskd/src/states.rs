//! Live state sources that keys can show: polled, reported on every poll,
//! deduplicated by `Deck::set_state`.

use std::time::Duration;

use tokio::sync::mpsc;

use crate::{
    actions::{SINK, SOURCE},
    session::Event,
};

const POLL: Duration = Duration::from_millis(700);

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
}
