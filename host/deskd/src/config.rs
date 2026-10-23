//! `~/.config/deskd/config.toml`: the pages of keys and what each one does.
//!
//! This is the Stream Deck "profile". The panel never sees it — deskd turns it
//! into the `Key`s the panel draws and keeps the actions to itself.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use desk_core::{
    protocol::{self, HostMsg, KEYS_PER_PAGE, Key, MAX_LINE, MAX_PAGES},
    text,
};
use serde::Deserialize;

use crate::{actions::Action, states};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Optional; OBS keys work with the defaults.
    #[serde(default)]
    pub obs: ObsConfig,
    /// The default profile: shown whenever the focused app has none.
    #[serde(rename = "page")]
    pub pages: Vec<Page>,
    /// Per-app profiles, like the Stream Deck's: while a window of one of
    /// `class` has focus, its pages replace the default ones.
    #[serde(rename = "app", default)]
    pub apps: Vec<AppProfile>,
    /// States read from a command, `[state.<name>]`, for keys of any
    /// profile to show by name.
    #[serde(rename = "state", default)]
    pub states: BTreeMap<String, CommandState>,
}

/// A live state deskd polls by running a command: true while it exits 0.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CommandState {
    /// Run with `sh -c`.
    pub cmd: String,
    /// Seconds between polls.
    #[serde(default = "default_every")]
    pub every: u64,
}

fn default_every() -> u64 {
    5
}

/// Slowest poll a command state may ask for.
const EVERY_MAX: u64 = 3600;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppProfile {
    /// Shown in the panel's status strip.
    pub name: String,
    /// Window classes (Hyprland `class`), matched case-insensitively.
    pub class: Vec<String>,
    #[serde(rename = "page")]
    pub pages: Vec<Page>,
}

/// Longest profile name the status strip has room for.
pub const PROFILE_NAME_MAX: usize = 12;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ObsConfig {
    pub url: String,
    /// Environment variable holding the password. When unset, deskd reads it
    /// from OBS's own obs-websocket config.
    pub password_env: String,
}

impl Default for ObsConfig {
    fn default() -> Self {
        Self { url: "ws://127.0.0.1:4455".into(), password_env: "OBS_WEBSOCKET_PASSWORD".into() }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub name: String,
    #[serde(rename = "key", default)]
    pub keys: Vec<KeyConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyConfig {
    pub label: String,
    #[serde(default)]
    pub sub: String,
    /// "#rrggbb"
    #[serde(default = "default_bg")]
    pub color: String,
    #[serde(default = "default_fg")]
    pub text: String,
    /// Name of a live state (see `states`); while it is true the key is drawn
    /// "on" and shows `label_on`/`sub_on` when given.
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub label_on: Option<String>,
    #[serde(default)]
    pub sub_on: Option<String>,
    pub action: Action,
}

fn default_bg() -> String {
    "#303030".into()
}

fn default_fg() -> String {
    "#ffffff".into()
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("deskd/config.toml")
}

pub fn load(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&text).with_context(|| format!("in {}", path.display()))
}

/// Parses and validates. Everything the panel would reject or misdraw is
/// caught here, at startup, rather than showing up as an `err` line later.
pub fn parse(text: &str) -> Result<Config> {
    let config: Config = toml::from_str(text)?;
    for (name, state) in &config.states {
        let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !ok {
            bail!("state name '{name}' may only contain a-z, 0-9 and '-'");
        }
        if states::is_known(name) || name.starts_with("obs-") {
            bail!("state name '{name}' is taken by a built-in state");
        }
        if state.cmd.trim().is_empty() {
            bail!("state '{name}' needs a cmd");
        }
        if !(1..=EVERY_MAX).contains(&state.every) {
            bail!("state '{name}': every must be 1..={EVERY_MAX} seconds");
        }
    }
    validate_pages(&config.pages, &config.states).context("default profile")?;
    for app in &config.apps {
        let name = &app.name;
        if name.is_empty() || !name.is_ascii() || name.len() > PROFILE_NAME_MAX {
            bail!("app profile name '{name}' must be 1..={PROFILE_NAME_MAX} ASCII characters for the status strip");
        }
        if app.class.is_empty() || app.class.iter().any(String::is_empty) {
            bail!("app profile '{name}' needs non-empty window classes");
        }
        validate_pages(&app.pages, &config.states).with_context(|| format!("app profile '{name}'"))?;
    }
    Ok(config)
}

/// One profile's pages. A `page` action's index refers to this profile's own
/// pages, so it is checked against them.
fn validate_pages(pages: &[Page], commands: &BTreeMap<String, CommandState>) -> Result<()> {
    if pages.is_empty() || pages.len() > MAX_PAGES {
        bail!("need 1..={} pages, got {}", MAX_PAGES, pages.len());
    }
    for page in pages {
        if page.keys.len() > KEYS_PER_PAGE {
            bail!("page '{}' has {} keys, the panel shows {}", page.name, page.keys.len(), KEYS_PER_PAGE);
        }
        for key in &page.keys {
            for color in [&key.color, &key.text] {
                parse_color(color).with_context(|| format!("key '{}'", key.label))?;
            }
            let texts = [Some(&key.label), key.label_on.as_ref(), Some(&key.sub), key.sub_on.as_ref()];
            if let Some(text) = texts.into_iter().flatten().find(|t| t.chars().any(char::is_control)) {
                bail!("key text {:?} contains control characters", text);
            }
            if let Some(state) = &key.state
                && !states::is_known(state)
                && !commands.contains_key(state)
            {
                bail!(
                    "key '{}': unknown state '{}', expected one of {:?}, {}<scene> / {}<input> or a [state.<name>]",
                    key.label,
                    state,
                    states::NAMES,
                    states::OBS_SCENE,
                    states::OBS_MUTED
                );
            }
            key.action.validate(pages.len()).with_context(|| format!("key '{}'", key.label))?;
            for label in [Some(&key.label), key.label_on.as_ref()].into_iter().flatten() {
                if !text::label_fits(label) {
                    bail!("label '{}' does not fit the two lines a key shows", label);
                }
            }
            for sub in [Some(&key.sub), key.sub_on.as_ref()].into_iter().flatten() {
                if !text::sub_fits(sub) {
                    bail!("sub '{}' does not fit the one line a key shows", sub);
                }
            }
        }
        let worst = worst_case_page(page);
        let size = protocol::encode_host(&worst).len();
        if size > MAX_LINE {
            bail!("page '{}' encodes to {} bytes, over the {}-byte line limit", page.name, size, MAX_LINE);
        }
    }
    Ok(())
}

/// The page as its longest possible wire line: every key showing whichever of
/// its two texts is longer, at the largest generation. The panel drops an
/// overlong line on every retry, so a profile that could ever produce one is
/// rejected at load rather than failing to appear later.
fn worst_case_page(page: &Page) -> HostMsg {
    let longer = |a: &String, b: &Option<String>| match b {
        Some(b) if b.len() > a.len() => b.clone(),
        _ => a.clone(),
    };
    let keys = page
        .keys
        .iter()
        .map(|k| Key { label: longer(&k.label, &k.label_on), sub: longer(&k.sub, &k.sub_on), bg: 0xFFFFFF, fg: 0xFFFFFF, on: true })
        .collect();
    HostMsg::Page { generation: u32::MAX, index: u8::MAX, keys }
}

impl Config {
    /// Every key of every profile: OBS support is started once, at launch,
    /// so it has to cover profiles that are not active yet.
    fn keys(&self) -> impl Iterator<Item = &KeyConfig> {
        self.pages.iter().chain(self.apps.iter().flat_map(|a| &a.pages)).flat_map(|p| &p.keys)
    }

    /// The profile for a focused window class: the first `[[app]]` listing
    /// it, or `None` for the default.
    pub fn profile_for(&self, class: &str) -> Option<usize> {
        // No window focused (an empty workspace) is always the default.
        if class.is_empty() {
            return None;
        }
        self.apps.iter().position(|a| a.class.iter().any(|c| c.eq_ignore_ascii_case(class)))
    }

    pub fn pages(&self, profile: Option<usize>) -> &[Page] {
        profile.and_then(|i| self.apps.get(i)).map_or(&self.pages, |a| &a.pages)
    }

    /// The status strip's title for a profile; empty for the default.
    pub fn title(&self, profile: Option<usize>) -> &str {
        profile.and_then(|i| self.apps.get(i)).map_or("", |a| &a.name)
    }

    /// Whether any key needs OBS, so deskd only connects when it is used.
    pub fn uses_obs(&self) -> bool {
        self.keys().any(|k| k.action.uses_obs() || k.state.as_deref().is_some_and(|s| s.starts_with("obs-")))
    }

    /// OBS inputs whose mute state a key shows or toggles.
    pub fn obs_inputs(&self) -> std::collections::BTreeSet<String> {
        self.keys()
            .filter_map(|k| match (&k.action, k.state.as_deref().and_then(|s| s.strip_prefix(states::OBS_MUTED))) {
                (_, Some(input)) => Some(input.to_string()),
                (Action::ObsMute { input }, None) => Some(input.clone()),
                _ => None,
            })
            .collect()
    }
}

pub fn parse_color(s: &str) -> Result<u32> {
    let hex = s.strip_prefix('#').filter(|h| h.len() == 6).with_context(|| format!("'{s}' is not #rrggbb"))?;
    u32::from_str_radix(hex, 16).with_context(|| format!("'{s}' is not #rrggbb"))
}

#[cfg(test)]
mod tests {
    use desk_core::text::{LABEL_MAX, SUB_MAX};

    use super::*;

    const SAMPLE: &str = r##"
[[page]]
name = "main"

[[page.key]]
label = "Mic"
color = "#803030"
state = "mic-muted"
label_on = "Mic OFF"
action = { type = "mic-mute" }

[[page.key]]
label = "Term"
action = { type = "shell", cmd = "kitty" }

[[page.key]]
label = "Copy"
action = { type = "keys", mods = "CTRL", key = "C" }

[[page]]
name = "two"
[[page.key]]
label = "Back"
action = { type = "page", index = 0 }
"##;

    #[test]
    fn parses_a_profile() {
        let config = parse(SAMPLE).unwrap();
        assert_eq!(config.pages.len(), 2);
        assert_eq!(config.pages[0].keys[0].state.as_deref(), Some("mic-muted"));
        assert_eq!(parse_color(&config.pages[0].keys[1].color).unwrap(), 0x303030);
    }

    fn full_page(label: &str) -> String {
        let mut text = String::from("[[page]]\nname = \"x\"\n");
        for _ in 0..KEYS_PER_PAGE {
            text.push_str(&format!(
                "[[page.key]]\nlabel = \"{label}\"\nsub = \"{}\"\naction = {{ type = \"dashboard\" }}\n",
                "y".repeat(SUB_MAX)
            ));
        }
        text
    }

    #[test]
    fn labels_at_the_caps_always_fit_on_the_wire() {
        // Quotes double on the wire; the caps are set so even that fits.
        parse(&full_page(&"x".repeat(LABEL_MAX))).unwrap();
        parse(&full_page(&"\\\"".repeat(LABEL_MAX))).unwrap();
    }

    const APPS: &str = r##"
[[page]]
name = "default"
[[page.key]]
label = "Default"
action = { type = "dashboard" }

[[app]]
name = "Firefox"
class = ["firefox", "librewolf"]
[[app.page]]
name = "browse"
[[app.page.key]]
label = "Back"
action = { type = "keys", mods = "ALT", key = "Left" }
[[app.page.key]]
label = "Scene"
state = "obs-scene:Desk"
action = { type = "obs-scene", scene = "Desk" }
"##;

    #[test]
    fn app_profiles_match_by_class_and_count_for_obs() {
        let config = parse(APPS).unwrap();
        assert_eq!(config.profile_for("firefox"), Some(0));
        assert_eq!(config.profile_for("LibreWolf"), Some(0), "case-insensitive");
        assert_eq!(config.profile_for("kitty"), None);
        assert_eq!(config.profile_for(""), None, "an empty workspace is the default");
        assert_eq!(config.pages(Some(0))[0].keys[0].label, "Back");
        assert_eq!(config.pages(None)[0].keys[0].label, "Default");
        assert_eq!(config.title(Some(0)), "Firefox");
        assert!(config.uses_obs(), "OBS keys in an inactive profile still start OBS support");
    }

    #[test]
    fn app_profiles_are_validated_like_the_default() {
        assert!(parse(&APPS.replace("\"Firefox\"", "\"A very long profile name\"")).is_err());
        assert!(parse(&APPS.replace("class = [\"firefox\", \"librewolf\"]", "class = []")).is_err());
        assert!(parse(&APPS.replace("class = [\"firefox\", \"librewolf\"]", "class = [\"\"]")).is_err());
        let bad_page = APPS.replace("action = { type = \"keys\", mods = \"ALT\", key = \"Left\" }", "action = { type = \"page\", index = 1 }");
        assert!(parse(&bad_page).is_err(), "page index beyond the app profile's own pages");
    }

    #[test]
    fn command_states_are_named_by_the_config() {
        let vpn = format!("{SAMPLE}\n[state.vpn]\ncmd = \"true\"\n").replace("\"mic-muted\"", "\"vpn\"");
        let config = parse(&vpn).unwrap();
        assert_eq!(config.states["vpn"], CommandState { cmd: "true".into(), every: 5 });
        let builtin = format!("{SAMPLE}\n[state.muted]\ncmd = \"true\"\n");
        assert!(parse(&builtin).is_err(), "shadows a built-in state");
        let obs = format!("{SAMPLE}\n[state.obs-x]\ncmd = \"true\"\n");
        assert!(parse(&obs).is_err(), "takes the OBS prefix");
        let never = format!("{SAMPLE}\n[state.vpn]\ncmd = \"true\"\nevery = 0\n");
        assert!(parse(&never).is_err(), "never polled");
        let empty = format!("{SAMPLE}\n[state.vpn]\ncmd = \" \"\n");
        assert!(parse(&empty).is_err(), "no command");
        let upper = format!("{SAMPLE}\n[state.VPN]\ncmd = \"true\"\n");
        assert!(parse(&upper).is_err(), "name outside a-z0-9-");
    }

    #[test]
    fn shipped_example_is_valid() {
        parse(include_str!("../config.example.toml")).unwrap();
    }

    #[test]
    fn rejects_what_the_panel_cannot_show() {
        let bad_page = SAMPLE.replace("index = 0", "index = 5");
        assert!(parse(&bad_page).is_err(), "page action out of range");
        let korean = SAMPLE.replace("\"Term\"", "\"터미널\"");
        assert!(parse(&korean).is_ok(), "Korean labels are drawn with the Unifont");
        let long_korean = SAMPLE.replace("\"Term\"", &format!("\"{}\"", "가".repeat(17)));
        assert!(parse(&long_korean).is_err(), "more Hangul than two lines hold");
        let long = SAMPLE.replace("\"Term\"", &format!("\"{}\"", "x".repeat(LABEL_MAX + 1)));
        assert!(parse(&long).is_err(), "label longer than a key shows");
        let state = SAMPLE.replace("\"mic-muted\"", "\"cpu-hot\"");
        assert!(parse(&state).is_err(), "unknown state");
        let color = SAMPLE.replace("#803030", "red");
        assert!(parse(&color).is_err(), "bad colour");
        let mut ten = String::from("[[page]]\nname = \"x\"\n");
        for _ in 0..10 {
            ten.push_str("[[page.key]]\nlabel = \"k\"\naction = { type = \"dashboard\" }\n");
        }
        assert!(parse(&ten).is_err(), "more keys than the grid");
    }
}
