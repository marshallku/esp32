//! A Unix socket for talking to the running daemon: `deskd ctl` uses it, and
//! so do the e2e tests and (later) the Claude Code hook.
//!
//! One JSON request per connection, one JSON reply. The socket lives in
//! `$XDG_RUNTIME_DIR`, which is private to the user, so anything that can
//! reach it could already run commands as them.

use std::path::PathBuf;

use anyhow::{Context, Result};
use desk_core::protocol::Gesture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot},
};

use crate::{
    approval::{Ask, Decision},
    session::Event,
};

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    /// Every live state value deskd knows, as keys would show them.
    States,
    Inject { gesture: Gesture },
    Reload,
    /// From `deskd hook`: put a Claude Code permission request on the panel
    /// and answer with the button pressed.
    Approve { ask: Ask },
    /// Test hook: behave as if a window of `class` had taken focus.
    Focus { class: String },
}

pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join("deskd.sock")
}

pub async fn serve(events: mpsc::Sender<Event>) -> Result<()> {
    let path = socket_path();
    // A socket file left by a previous run that died; nothing is listening.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    loop {
        let (stream, _) = listener.accept().await?;
        let events = events.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, events).await {
                eprintln!("control connection: {e:#}");
            }
        });
    }
}

async fn handle(stream: UnixStream, events: mpsc::Sender<Event>) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let mut line = String::new();
    read.read_line(&mut line).await?;
    let reply = match serde_json::from_str::<Request>(&line) {
        Ok(Request::Approve { ask }) => {
            let (tx, rx) = oneshot::channel();
            events.send(Event::Approve(ask, tx)).await.context("daemon shutting down")?;
            // Wait for the panel, but notice the hook going away: Claude Code
            // kills it on timeout, and dropping `rx` is how the session learns
            // to take the prompt down.
            let mut probe = [0u8; 1];
            let decision = tokio::select! {
                decision = rx => decision.unwrap_or(Decision::Terminal),
                _ = read.read(&mut probe) => return Ok(()),
            };
            serde_json::json!({ "ok": true, "decision": decision })
        }
        Ok(Request::Focus { class }) => {
            events.send(Event::Focus(class)).await.context("daemon shutting down")?;
            serde_json::json!({ "ok": true })
        }
        Ok(req) => {
            let (tx, rx) = oneshot::channel();
            events.send(Event::Control(req, tx)).await.context("daemon shutting down")?;
            rx.await.context("request dropped")?
        }
        Err(e) => serde_json::json!({ "ok": false, "error": format!("bad request: {e}") }),
    };
    write.write_all(format!("{reply}\n").as_bytes()).await?;
    Ok(())
}

/// Client side: send one request to the running daemon and return its reply.
pub async fn request(req: &Value) -> Result<Value> {
    let path = socket_path();
    let stream = UnixStream::connect(&path).await.with_context(|| format!("is deskd running? ({})", path.display()))?;
    let (read, mut write) = stream.into_split();
    write.write_all(format!("{req}\n").as_bytes()).await?;
    let mut line = String::new();
    BufReader::new(read).read_line(&mut line).await?;
    Ok(serde_json::from_str(&line)?)
}
