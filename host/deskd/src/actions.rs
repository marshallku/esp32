//! What a key does when pressed.
//!
//! Most actions are one external command. They are described as data — the
//! `argv` they would run — so tests can check exactly what a key does without
//! running it, and execution is a thin spawn on top.

use anyhow::{Result, bail};
use desk_core::protocol::ScreenId;
use serde::Deserialize;

use crate::obs;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    /// `sh -c cmd`, detached. Output goes to deskd's log.
    Shell { cmd: String },
    /// A key combination sent to the focused window through Hyprland.
    Keys {
        #[serde(default)]
        mods: String,
        key: String,
    },
    /// playerctl: play-pause, next, previous, stop.
    Media { cmd: String },
    /// Default output volume by `step` percent (negative lowers it).
    Volume { step: i8 },
    /// Toggle the default output's mute.
    Mute,
    /// Toggle the default input's mute.
    MicMute,
    /// Switch the panel to button page `index`.
    Page { index: u8 },
    /// Switch the panel to the dashboard.
    Dashboard,
    /// Switch the panel to the PC stats screen.
    Stats,
    /// Switch the panel to the notifications.
    Notes,
    /// OBS: switch the program scene.
    ObsScene { scene: String },
    /// OBS: start or stop recording.
    ObsRecord,
    /// OBS: start or stop streaming.
    ObsStream,
    /// OBS: toggle an audio input's mute (the source name as shown in OBS).
    ObsMute { input: String },
}

/// What deskd does about a press, beyond running a command.
#[derive(Debug, PartialEq)]
pub enum Effect {
    Run(Vec<String>),
    Goto(ScreenId),
    Obs(obs::Command),
}

const MEDIA_COMMANDS: &[&str] = &["play-pause", "next", "previous", "stop"];

impl Action {
    pub fn validate(&self, pages: usize) -> Result<()> {
        match self {
            Action::Keys { mods, key } => {
                // Both are spliced into a Lua dispatcher call, so they are held
                // to the characters Hyprland key names actually use.
                let ok = |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ' ');
                if key.is_empty() || !ok(key) || !ok(mods) {
                    bail!("keys: mods/key may only contain letters, digits, '_' and spaces");
                }
            }
            Action::Media { cmd } if !MEDIA_COMMANDS.contains(&cmd.as_str()) => {
                bail!("media: cmd must be one of {:?}", MEDIA_COMMANDS);
            }
            Action::Page { index } if *index as usize >= pages => bail!("page {index} does not exist"),
            _ => {}
        }
        Ok(())
    }

    pub fn uses_obs(&self) -> bool {
        matches!(self, Action::ObsScene { .. } | Action::ObsRecord | Action::ObsStream | Action::ObsMute { .. })
    }

    pub fn effect(&self) -> Effect {
        let run = |args: &[&str]| Effect::Run(args.iter().map(|s| s.to_string()).collect());
        match self {
            Action::Shell { cmd } => run(&["sh", "-c", cmd]),
            Action::Keys { mods, key } => {
                let lua = format!("hl.dsp.send_shortcut({{mods=\"{mods}\", key=\"{key}\"}})");
                Effect::Run(vec!["hyprctl".into(), "dispatch".into(), lua])
            }
            Action::Media { cmd } => run(&["playerctl", cmd]),
            Action::Volume { step } => {
                let arg = format!("{}%{}", step.unsigned_abs(), if *step < 0 { "-" } else { "+" });
                Effect::Run(vec!["wpctl".into(), "set-volume".into(), "-l".into(), "1.0".into(), SINK.into(), arg])
            }
            Action::Mute => run(&["wpctl", "set-mute", SINK, "toggle"]),
            Action::MicMute => run(&["wpctl", "set-mute", SOURCE, "toggle"]),
            Action::Page { index } => Effect::Goto(ScreenId::Page(*index)),
            Action::Dashboard => Effect::Goto(ScreenId::Dash),
            Action::Stats => Effect::Goto(ScreenId::Stats),
            Action::Notes => Effect::Goto(ScreenId::Notes),
            Action::ObsScene { scene } => Effect::Obs(obs::Command::Scene(scene.clone())),
            Action::ObsRecord => Effect::Obs(obs::Command::ToggleRecord),
            Action::ObsStream => Effect::Obs(obs::Command::ToggleStream),
            Action::ObsMute { input } => Effect::Obs(obs::Command::ToggleMute(input.clone())),
        }
    }
}

pub const SINK: &str = "@DEFAULT_AUDIO_SINK@";
pub const SOURCE: &str = "@DEFAULT_AUDIO_SOURCE@";

/// Runs `argv` in the background and logs a failure. A key press is fire and
/// forget — nothing waits on it — but a command that fails silently would
/// look like a dead key.
pub fn spawn(argv: Vec<String>) {
    tokio::spawn(async move {
        let result = tokio::process::Command::new(&argv[0]).args(&argv[1..]).kill_on_drop(false).status().await;
        match result {
            Ok(status) if status.success() => {}
            Ok(status) => eprintln!("action {:?} exited with {}", argv, status),
            Err(e) => eprintln!("action {:?} failed to start: {}", argv, e),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(effect: Effect) -> Vec<String> {
        match effect {
            Effect::Run(argv) => argv,
            other => panic!("expected a command, got {other:?}"),
        }
    }

    #[test]
    fn keys_become_a_hyprland_lua_dispatch() {
        let action = Action::Keys { mods: "CTRL SHIFT".into(), key: "T".into() };
        assert_eq!(argv(action.effect()), ["hyprctl", "dispatch", r#"hl.dsp.send_shortcut({mods="CTRL SHIFT", key="T"})"#]);
    }

    #[test]
    fn keys_reject_lua_injection() {
        let action = Action::Keys { mods: String::new(), key: "T\"}) os.execute(\"x".into() };
        assert!(action.validate(1).is_err());
    }

    #[test]
    fn volume_steps_both_ways_and_caps_at_full() {
        assert_eq!(argv(Action::Volume { step: 5 }.effect())[5], "5%+");
        assert_eq!(argv(Action::Volume { step: -5 }.effect())[5], "5%-");
        assert_eq!(argv(Action::Volume { step: 5 }.effect())[2..4], ["-l", "1.0"]);
    }

    #[test]
    fn navigation_is_not_a_command() {
        assert_eq!(Action::Page { index: 1 }.effect(), Effect::Goto(ScreenId::Page(1)));
        assert_eq!(Action::Dashboard.effect(), Effect::Goto(ScreenId::Dash));
        assert!(Action::Page { index: 2 }.validate(2).is_err());
    }
}
