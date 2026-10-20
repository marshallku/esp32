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
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot},
};

use crate::session::Event;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    Inject { gesture: Gesture },
    Reload,
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
    let mut line = String::new();
    BufReader::new(read).read_line(&mut line).await?;
    let reply = match serde_json::from_str::<Request>(&line) {
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
