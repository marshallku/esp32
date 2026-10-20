//! deskd — the PC side of the desk panel.
//!
//! Like the Stream Deck app, it owns everything the panel shows and does: the
//! profile from `~/.config/deskd/config.toml`, live key states, and the
//! actions behind each key. The panel only draws and reports touches.
//!
//!   deskd                 run the daemon
//!   deskd check [PATH]    validate a config file and exit
//!   deskd ctl JSON        send one control request to the running daemon,
//!                         e.g. '{"cmd":"status"}' or '{"cmd":"reload"}'

mod actions;
mod config;
mod control;
mod deck;
mod serial;
mod session;
mod states;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => run().await,
        Some("check") => {
            let path = args.get(1).map(PathBuf::from).unwrap_or_else(config::default_path);
            let config = config::load(&path)?;
            println!("{}: {} page(s) OK", path.display(), config.pages.len());
            Ok(())
        }
        Some("ctl") => {
            let req = args.get(1).context("usage: deskd ctl JSON")?;
            let reply = control::request(&serde_json::from_str(req).context("request is not JSON")?).await?;
            println!("{reply}");
            if reply["ok"] != true {
                std::process::exit(1);
            }
            Ok(())
        }
        Some(other) => bail!("unknown command '{other}'"),
    }
}

async fn run() -> Result<()> {
    let config_path = config::default_path();
    let deck = deck::Deck::new(config::load(&config_path)?);
    let (events_tx, events_rx) = mpsc::channel(64);

    tokio::spawn(states::poll_audio(events_tx.clone()));
    let control = tokio::spawn(control::serve(events_tx));
    let session = tokio::spawn(session::run(deck, config_path, events_rx));

    tokio::select! {
        result = control => result?.context("control socket"),
        _ = session => unreachable!("the session loop never returns"),
        _ = tokio::signal::ctrl_c() => {
            let _ = std::fs::remove_file(control::socket_path());
            Ok(())
        }
    }
}
