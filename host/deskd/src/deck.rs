//! The live profile: config plus the current value of every state source,
//! rendered into what the panel draws, and the way back from a press to the
//! action behind it.

use std::collections::HashMap;

use desk_core::protocol::{HostMsg, Key};

use crate::{
    actions::Action,
    config::{Config, KeyConfig, parse_color},
};

pub struct Deck {
    config: Config,
    states: HashMap<String, bool>,
    /// Bumped on every full push. Presses echo it back, and one that does not
    /// match is from a layout the panel no longer shows.
    generation: u32,
    /// Whether any layout has gone out yet; until then there is nothing on
    /// the panel for a state change to update.
    pushed: bool,
}

impl Deck {
    pub fn new(config: Config) -> Self {
        // Random, not 0 or the clock: the panel keeps its layout across a
        // deskd restart, and a press it sent just before the new daemon's
        // first push must not match the new layout's generation. A clock
        // seed collides when a restart lands within a few pushes' worth of
        // seconds of the previous start.
        Self { config, states: HashMap::new(), generation: random_u32(), pushed: false }
    }

    /// The whole profile as the panel's `begin` / `page`… / `commit`.
    pub fn push(&mut self) -> Vec<HostMsg> {
        self.generation = self.generation.wrapping_add(1);
        self.pushed = true;
        let generation = self.generation;
        let mut msgs = vec![HostMsg::Begin { generation, pages: self.config.pages.len() as u8 }];
        for (index, page) in self.config.pages.iter().enumerate() {
            let keys = page.keys.iter().map(|k| self.render(k)).collect();
            msgs.push(HostMsg::Page { generation, index: index as u8, keys });
        }
        msgs.push(HostMsg::Commit { generation });
        msgs
    }

    pub fn states(&self) -> &HashMap<String, bool> {
        &self.states
    }

    /// Replaces the profile (config reload). The caller pushes afterwards.
    pub fn replace(&mut self, config: Config) {
        self.config = config;
    }

    /// The action behind a press, if it was made against the layout on the
    /// panel now. `None` for a stale generation or an empty slot.
    pub fn resolve(&self, generation: u32, page: u8, index: u8) -> Option<&Action> {
        if generation != self.generation {
            return None;
        }
        self.config.pages.get(page as usize)?.keys.get(index as usize).map(|k| &k.action)
    }

    /// Records a state value and returns updates for every key showing it.
    /// Unchanged values produce nothing, so pollers can report every tick.
    pub fn set_state(&mut self, name: &str, value: bool) -> Vec<HostMsg> {
        if self.states.get(name) == Some(&value) {
            return Vec::new();
        }
        self.states.insert(name.to_string(), value);
        let mut msgs = Vec::new();
        for (page, p) in self.config.pages.iter().enumerate() {
            for (index, k) in p.keys.iter().enumerate() {
                if k.state.as_deref() == Some(name) {
                    msgs.push(HostMsg::Key {
                        generation: self.generation,
                        page: page as u8,
                        index: index as u8,
                        key: self.render(k),
                    });
                }
            }
        }
        // Before the first push there is no layout on the panel to update;
        // the value is kept and goes out with the push.
        if self.pushed { msgs } else { Vec::new() }
    }

    pub fn render(&self, k: &KeyConfig) -> Key {
        let on = k.state.as_ref().and_then(|s| self.states.get(s)).copied().unwrap_or(false);
        let pick = |base: &String, alt: &Option<String>| if on { alt.clone().unwrap_or_else(|| base.clone()) } else { base.clone() };
        Key {
            label: pick(&k.label, &k.label_on),
            sub: pick(&k.sub, &k.sub_on),
            // Validated at load; unreachable defaults keep render infallible.
            bg: parse_color(&k.color).unwrap_or(0x303030),
            fg: parse_color(&k.text).unwrap_or(0xFFFFFF),
            on,
        }
    }
}

fn random_u32() -> u32 {
    let mut bytes = [0u8; 4];
    // /dev/urandom does not fail on Linux once booted; if it somehow did, a
    // clock-derived value is still far better than a constant.
    match std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes)) {
        Ok(()) => u32::from_ne_bytes(bytes),
        Err(_) => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.subsec_nanos()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse;

    fn deck() -> Deck {
        Deck::new(
            parse(
                r#"
[[page]]
name = "a"
[[page.key]]
label = "Mic"
state = "mic-muted"
label_on = "Muted"
action = { type = "mic-mute" }
[[page.key]]
label = "Dash"
action = { type = "dashboard" }
"#,
            )
            .unwrap(),
        )
    }

    #[test]
    fn push_is_a_complete_generation() {
        let mut d = deck();
        let msgs = d.push();
        let HostMsg::Begin { generation, pages: 1 } = msgs[0] else { panic!("expected begin") };
        assert!(matches!(msgs.last(), Some(HostMsg::Commit { generation: g }) if *g == generation));
        assert_eq!(d.push().len(), msgs.len());
        assert!(matches!(d.push()[0], HostMsg::Begin { generation: g, .. } if g == generation.wrapping_add(2)));
    }

    #[test]
    fn presses_resolve_only_against_the_current_generation() {
        let mut d = deck();
        let HostMsg::Begin { generation, .. } = d.push()[0] else { panic!("expected begin") };
        assert_eq!(d.resolve(generation, 0, 1), Some(&Action::Dashboard));
        assert_eq!(d.resolve(generation, 0, 5), None);
        d.push();
        assert_eq!(d.resolve(generation, 0, 1), None, "stale generation");
    }

    #[test]
    fn generations_differ_across_restarts() {
        // Two daemons must not hand out overlapping generation ranges; a
        // random seed makes that a 1-in-4-billion event per restart.
        let (a, b) = (deck().generation, deck().generation);
        assert!(a.abs_diff(b) > 1000, "{a} vs {b}");
    }

    #[test]
    fn state_changes_update_bound_keys_once() {
        let mut d = deck();
        assert!(d.set_state("mic-muted", true).is_empty(), "nothing on the panel yet");
        let push = d.push();
        let HostMsg::Page { keys, .. } = &push[1] else { panic!() };
        assert_eq!((keys[0].label.as_str(), keys[0].on), ("Muted", true));

        let msgs = d.set_state("mic-muted", false);
        assert_eq!(msgs.len(), 1);
        let HostMsg::Key { key, index: 0, .. } = &msgs[0] else { panic!() };
        assert_eq!((key.label.as_str(), key.on), ("Mic", false));
        assert!(d.set_state("mic-muted", false).is_empty(), "unchanged");
    }
}
