//! Which app has focus, for per-app profiles — from Hyprland's event socket.
//!
//! The socket only reports changes, so each connection starts by asking
//! `hyprctl activewindow` for the current window. Focus can flap through
//! several windows in a moment (a launcher opening and closing, a dialog), so
//! a class is reported only once it has held for `SETTLE`.
//!
//! Without Hyprland this does nothing and the default profile stays up;
//! macOS and Windows would need their own source here.

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::UnixStream,
    sync::mpsc,
    time::Instant,
};

use crate::session::Event;

const SETTLE: Duration = Duration::from_millis(150);
const RETRY: Duration = Duration::from_secs(3);

pub async fn run(events: mpsc::Sender<Event>) {
    let Some(path) = socket_path() else {
        eprintln!("focus: not running under Hyprland; app profiles stay on the default");
        return;
    };
    let mut announced = false;
    loop {
        if let Err(e) = watch(&path, &events).await
            && !announced
        {
            eprintln!("focus: {e:#}");
            announced = true;
        }
        if events.is_closed() {
            return;
        }
        tokio::time::sleep(RETRY).await;
    }
}

fn socket_path() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let instance = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    Some(PathBuf::from(runtime).join("hypr").join(instance).join(".socket2.sock"))
}

async fn watch(path: &PathBuf, events: &mpsc::Sender<Event>) -> Result<()> {
    let stream = UnixStream::connect(path).await.with_context(|| format!("connecting {}", path.display()))?;
    if let Some(class) = active_class().await {
        events.send(Event::Focus(class)).await?;
    }
    let mut lines = BufReader::new(stream).lines();
    let mut pending: Option<String> = None;
    let settle = tokio::time::sleep(SETTLE);
    tokio::pin!(settle);
    loop {
        tokio::select! {
            line = lines.next_line() => match line? {
                Some(line) => if let Some(class) = parse_event(&line) {
                    pending = Some(class);
                    settle.as_mut().reset(Instant::now() + SETTLE);
                },
                None => bail!("Hyprland closed the event socket"),
            },
            _ = &mut settle, if pending.is_some() => {
                if let Some(class) = pending.take() {
                    events.send(Event::Focus(class)).await?;
                }
            }
        }
    }
}

/// `activewindow>>CLASS,TITLE` → CLASS. An empty class means no window has
/// focus (an empty workspace), which selects the default profile.
fn parse_event(line: &str) -> Option<String> {
    let rest = line.strip_prefix("activewindow>>")?;
    Some(rest.split_once(',').map_or(rest, |(class, _)| class).to_string())
}

async fn active_class() -> Option<String> {
    let out = tokio::process::Command::new("hyprctl").args(["activewindow", "-j"]).output().await.ok()?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    // `{}` when nothing has focus.
    Some(json["class"].as_str().unwrap_or("").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_class_from_activewindow_events() {
        assert_eq!(parse_event("activewindow>>firefox,Some page, with a comma"), Some("firefox".into()));
        assert_eq!(parse_event("activewindow>>,"), Some(String::new()), "no window focused");
        assert_eq!(parse_event("activewindowv2>>557a141d4d60"), None);
        assert_eq!(parse_event("workspace>>2"), None);
    }
}
