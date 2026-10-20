//! `~/.config/deskd/config.toml`: the pages of keys and what each one does.
//!
//! This is the Stream Deck "profile". The panel never sees it — deskd turns it
//! into the `Key`s the panel draws and keeps the actions to itself.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use desk_core::{
    protocol::{self, HostMsg, KEYS_PER_PAGE, Key, MAX_LINE, MAX_PAGES},
    text::{LABEL_MAX, SUB_MAX},
};
use serde::Deserialize;

use crate::{actions::Action, states};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(rename = "page")]
    pub pages: Vec<Page>,
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
    if config.pages.is_empty() || config.pages.len() > MAX_PAGES {
        bail!("need 1..={} pages, got {}", MAX_PAGES, config.pages.len());
    }
    for page in &config.pages {
        if page.keys.len() > KEYS_PER_PAGE {
            bail!("page '{}' has {} keys, the panel shows {}", page.name, page.keys.len(), KEYS_PER_PAGE);
        }
        for key in &page.keys {
            for color in [&key.color, &key.text] {
                parse_color(color).with_context(|| format!("key '{}'", key.label))?;
            }
            for label in [Some(&key.label), key.label_on.as_ref(), Some(&key.sub), key.sub_on.as_ref()].into_iter().flatten() {
                if !label.chars().all(|c| (' '..='~').contains(&c)) {
                    bail!("key label '{}' has characters the panel's ASCII font cannot draw", label);
                }
            }
            if let Some(state) = &key.state
                && !states::NAMES.contains(&state.as_str())
            {
                bail!("key '{}': unknown state '{}', expected one of {:?}", key.label, state, states::NAMES);
            }
            key.action.validate(config.pages.len()).with_context(|| format!("key '{}'", key.label))?;
            for label in [Some(&key.label), key.label_on.as_ref()].into_iter().flatten() {
                if label.len() > LABEL_MAX {
                    bail!("label '{}' is longer than the {} characters a key shows", label, LABEL_MAX);
                }
            }
            for sub in [Some(&key.sub), key.sub_on.as_ref()].into_iter().flatten() {
                if sub.len() > SUB_MAX {
                    bail!("sub '{}' is longer than the {} characters a key shows", sub, SUB_MAX);
                }
            }
        }
        let worst = worst_case_page(page);
        let size = protocol::encode_host(&worst).len();
        if size > MAX_LINE {
            bail!("page '{}' encodes to {} bytes, over the {}-byte line limit", page.name, size, MAX_LINE);
        }
    }
    Ok(config)
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

pub fn parse_color(s: &str) -> Result<u32> {
    let hex = s.strip_prefix('#').filter(|h| h.len() == 6).with_context(|| format!("'{s}' is not #rrggbb"))?;
    u32::from_str_radix(hex, 16).with_context(|| format!("'{s}' is not #rrggbb"))
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn shipped_example_is_valid() {
        parse(include_str!("../config.example.toml")).unwrap();
    }

    #[test]
    fn rejects_what_the_panel_cannot_show() {
        let bad_page = SAMPLE.replace("index = 0", "index = 5");
        assert!(parse(&bad_page).is_err(), "page action out of range");
        let korean = SAMPLE.replace("\"Term\"", "\"터미널\"");
        assert!(parse(&korean).is_err(), "non-ASCII label");
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
