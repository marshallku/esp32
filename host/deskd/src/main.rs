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
//!   deskd hook            Claude Code PermissionRequest hook: reads the
//!                         request on stdin, asks the panel, prints the decision

mod actions;
mod approval;
mod config;
mod control;
mod deck;
mod obs;
mod serial;
mod session;
mod states;

use std::{path::PathBuf, time::Duration};

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
        Some("hook") => {
            hook().await;
            Ok(())
        }
        Some(other) => bail!("unknown command '{other}'"),
    }
}

/// Longest the hook waits for the panel. Kept under the hook's own timeout
/// in Claude Code's settings (60 s) so the hook answers "terminal" itself
/// rather than being killed.
const HOOK_WAIT: Duration = Duration::from_secs(55);

/// The PermissionRequest hook. It can only ever print an allow or deny the
/// panel produced; every failure — no daemon, no panel, timeout, unreadable
/// input — prints nothing and exits 0, which hands the request back to
/// Claude Code's own prompt.
async fn hook() {
    let wait = std::env::var("DESKD_HOOK_WAIT_SECS").ok().and_then(|s| s.parse().ok()).map_or(HOOK_WAIT, Duration::from_secs);
    let mut input = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
        return;
    }
    let Ok(ask) = serde_json::from_str::<approval::Ask>(&input) else { return };
    let req = serde_json::json!({ "cmd": "approve", "ask": ask });
    let Ok(Ok(reply)) = tokio::time::timeout(wait, control::request(&req)).await else { return };
    let decision = serde_json::from_value(reply["decision"].clone()).unwrap_or(approval::Decision::Terminal);
    if let Some(out) = approval::hook_output(decision) {
        println!("{out}");
    }
}

async fn run() -> Result<()> {
    let config_path = config::default_path();
    let config = config::load(&config_path)?;
    let (events_tx, events_rx) = mpsc::channel(64);

    // Started only if some key uses OBS. Inputs are taken from the config at
    // startup; a reload that adds new OBS inputs needs a restart to track them.
    let obs = config.uses_obs().then(|| {
        let machine = obs::Machine::new(obs::password(&config.obs.password_env), config.obs_inputs());
        obs::spawn(config.obs.url.clone(), machine, events_tx.clone())
    });

    let deck = deck::Deck::new(config);
    tokio::spawn(states::poll_audio(events_tx.clone()));
    let control = tokio::spawn(control::serve(events_tx));
    let session = tokio::spawn(session::run(deck, config_path, events_rx, obs));

    tokio::select! {
        result = control => result?.context("control socket"),
        _ = session => unreachable!("the session loop never returns"),
        _ = tokio::signal::ctrl_c() => {
            let _ = std::fs::remove_file(control::socket_path());
            Ok(())
        }
    }
}
