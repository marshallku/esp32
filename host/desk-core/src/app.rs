//! The panel's behaviour, minus pixels: which screen is up, what the host has
//! pushed, what a gesture means right now, and what to tell the host.
//!
//! The firmware feeds it host lines, gestures and clock ticks, sends whatever
//! messages come back, and redraws from `view` when `take_dirty` says so.
//! Keeping it free of hardware is what lets the rules that matter — a stale
//! layout cannot be pressed, a held finger cannot approve — be tested here.

use alloc::{string::String, vec::Vec};

use crate::{
    geometry,
    gesture::Event,
    protocol::{DeviceMsg, Dir, Gesture, HostMsg, KEYS_PER_PAGE, Key, MAX_BUTTONS, MAX_PAGES, PROTO},
};

/// A prompt ignores touches that began less than this long after it appeared.
/// The hand that was reaching for a key when the modal popped up must not land
/// on "Allow".
pub const ARM_MS: u64 = 300;
/// The host pings every 2 s; three missed pings and it is gone.
pub const LINK_TIMEOUT_MS: u64 = 6_000;
/// A button page left untouched this long falls back to the dashboard.
pub const IDLE_MS: u64 = 60_000;

pub struct Prompt {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub buttons: Vec<String>,
    shown_at: u64,
}

struct Layout {
    generation: u32,
    pages: Vec<Vec<Key>>,
}

struct Staging {
    generation: u32,
    pages: Vec<Option<Vec<Key>>>,
}

/// What to draw.
pub enum View<'a> {
    Dashboard,
    Deck { page: u8, keys: &'a [Key] },
    /// A button page was asked for but there is nothing to show on it.
    NoDeck { link_up: bool },
    Prompt(&'a Prompt),
}

pub struct App {
    fw: String,
    layout: Option<Layout>,
    staging: Option<Staging>,
    /// `None` is the dashboard.
    screen: Option<u8>,
    prompt: Option<Prompt>,
    time: String,
    last_host: Option<u64>,
    last_input: u64,
    link_up: bool,
    dirty: bool,
}

impl App {
    pub fn new(fw: &str) -> Self {
        Self {
            fw: String::from(fw),
            layout: None,
            staging: None,
            screen: None,
            prompt: None,
            time: String::new(),
            last_host: None,
            last_input: 0,
            link_up: false,
            dirty: true,
        }
    }

    pub fn view(&self) -> View<'_> {
        if let Some(prompt) = &self.prompt {
            return View::Prompt(prompt);
        }
        match (self.screen, &self.layout) {
            (None, _) => View::Dashboard,
            (Some(page), Some(layout)) if self.link_up => View::Deck { page, keys: &layout.pages[page as usize] },
            (Some(_), _) => View::NoDeck { link_up: self.link_up },
        }
    }

    pub fn screen(&self) -> Option<u8> {
        self.screen
    }

    /// Pages in the active layout; 0 before the host has pushed one.
    pub fn page_count(&self) -> usize {
        self.layout.as_ref().map_or(0, |l| l.pages.len())
    }

    pub fn link_up(&self) -> bool {
        self.link_up
    }

    pub fn time(&self) -> &str {
        &self.time
    }

    /// Whether anything visible changed since the last call.
    pub fn take_dirty(&mut self) -> bool {
        core::mem::take(&mut self.dirty)
    }

    /// One message from the host. The caller acks the line itself, whatever
    /// this returns — flow control is per line, not per meaningful message.
    pub fn on_host(&mut self, msg: HostMsg, now: u64) -> Vec<DeviceMsg> {
        self.last_host = Some(now);
        let mut out = Vec::new();
        if !self.link_up {
            self.link_up = true;
            self.dirty = true;
        }

        match msg {
            HostMsg::Hello { proto } => {
                out.push(DeviceMsg::Hello { proto: PROTO, fw: self.fw.clone() });
                if proto != PROTO {
                    out.push(err("protocol version mismatch"));
                }
                // A restarted host knows nothing about where the panel is;
                // tell it rather than make it guess.
                out.push(DeviceMsg::Screen { page: self.screen });
            }
            HostMsg::Ping => out.push(DeviceMsg::Pong),
            HostMsg::Begin { generation, pages } => {
                if pages == 0 || pages as usize > MAX_PAGES {
                    out.push(err("page count out of range"));
                } else {
                    self.staging = Some(Staging { generation, pages: (0..pages).map(|_| None).collect() });
                }
            }
            HostMsg::Page { generation, index, keys } => match &mut self.staging {
                Some(s) if s.generation == generation && (index as usize) < s.pages.len() => {
                    if keys.len() > KEYS_PER_PAGE {
                        out.push(err("too many keys on a page"));
                    } else {
                        s.pages[index as usize] = Some(keys);
                    }
                }
                _ => out.push(err("page for a layout not being staged")),
            },
            HostMsg::Commit { generation } => match self.staging.take() {
                Some(s) if s.generation == generation && s.pages.iter().all(Option::is_some) => {
                    let pages: Vec<Vec<Key>> = s.pages.into_iter().flatten().collect();
                    let count = pages.len() as u8;
                    self.layout = Some(Layout { generation, pages });
                    if self.screen.is_some_and(|p| p >= count) {
                        self.set_screen(Some(count - 1), &mut out);
                    }
                    self.dirty = true;
                }
                // An incomplete or mismatched commit leaves the old layout up:
                // a half-replaced deck is exactly the state to avoid.
                _ => out.push(err("commit of an incomplete layout")),
            },
            HostMsg::Key { generation, page, index, key } => match &mut self.layout {
                Some(l) if l.generation == generation => {
                    match l.pages.get_mut(page as usize).and_then(|p| p.get_mut(index as usize)) {
                        Some(slot) => {
                            *slot = key;
                            if self.screen == Some(page) {
                                self.dirty = true;
                            }
                        }
                        None => out.push(err("key update out of range")),
                    }
                }
                _ => out.push(err("key update for a stale layout")),
            },
            HostMsg::Goto { page } => {
                if page.is_none_or(|p| (p as usize) < self.page_count()) {
                    self.set_screen(page, &mut out);
                } else {
                    out.push(err("goto page out of range"));
                }
            }
            HostMsg::Time { text } => {
                if text != self.time {
                    self.time = text;
                    self.dirty = true;
                }
            }
            HostMsg::Prompt { id, title, body, buttons } => {
                if buttons.is_empty() || buttons.len() > MAX_BUTTONS {
                    out.push(err("prompt button count out of range"));
                } else {
                    self.prompt = Some(Prompt { id, title, body, buttons, shown_at: now });
                    self.dirty = true;
                }
            }
            HostMsg::Dismiss { id } => {
                if self.prompt.as_ref().is_some_and(|p| p.id == id) {
                    self.prompt = None;
                    self.dirty = true;
                }
            }
            HostMsg::Inject { gesture } => out.extend(self.on_gesture(Event { gesture, down_at: now }, now)),
        }
        out
    }

    pub fn on_gesture(&mut self, ev: Event, now: u64) -> Vec<DeviceMsg> {
        self.last_input = now;
        let mut out = Vec::new();

        if let Some(prompt) = &self.prompt {
            if let Gesture::Tap { x, y } = ev.gesture
                && ev.down_at >= prompt.shown_at + ARM_MS
                && let Some(button) = geometry::button_at(x, y, prompt.buttons.len())
            {
                out.push(DeviceMsg::Answer { id: prompt.id, button });
                self.prompt = None;
                self.dirty = true;
            }
            return out;
        }

        match ev.gesture {
            Gesture::Swipe { dir } => {
                let count = self.page_count() as i32;
                // Screens in order: dashboard (-1), then pages 0..count.
                let current = self.screen.map_or(-1, i32::from);
                let next = match dir {
                    Dir::Left => current + 1,
                    Dir::Right => current - 1,
                };
                if next >= -1 && next < count.max(1) {
                    self.set_screen(u8::try_from(next).ok(), &mut out);
                }
            }
            Gesture::Tap { x, y } | Gesture::Long { x, y } => {
                let long = matches!(ev.gesture, Gesture::Long { .. });
                if let (Some(page), Some(layout), true) = (self.screen, &self.layout, self.link_up)
                    && let Some(index) = geometry::key_at(x, y)
                    && (index as usize) < layout.pages[page as usize].len()
                {
                    out.push(DeviceMsg::Press { generation: layout.generation, page, index, long });
                }
            }
        }
        out
    }

    /// Time-driven transitions: the host going quiet, the panel going idle.
    pub fn tick(&mut self, now: u64) -> Vec<DeviceMsg> {
        let mut out = Vec::new();
        if self.link_up && self.last_host.is_none_or(|t| now - t > LINK_TIMEOUT_MS) {
            self.link_up = false;
            // An unanswerable prompt must not stay up looking answerable; the
            // host has timed it out on its side already.
            self.prompt = None;
            self.dirty = true;
        }
        if self.prompt.is_none() && self.screen.is_some() && now - self.last_input > IDLE_MS {
            self.set_screen(None, &mut out);
        }
        out
    }

    fn set_screen(&mut self, page: Option<u8>, out: &mut Vec<DeviceMsg>) {
        if self.screen != page {
            self.screen = page;
            self.dirty = true;
            out.push(DeviceMsg::Screen { page });
        }
    }
}

fn err(msg: &str) -> DeviceMsg {
    DeviceMsg::Err { msg: String::from(msg) }
}

#[cfg(test)]
mod tests {
    use alloc::{string::ToString, vec};

    use super::*;

    fn key(label: &str) -> Key {
        Key { label: label.to_string(), bg: 0, fg: 0xFFFFFF, ..Default::default() }
    }

    /// An app with a committed two-page layout at generation 1, on page 0.
    fn ready() -> App {
        let mut app = App::new("test");
        app.on_host(HostMsg::Hello { proto: PROTO }, 0);
        app.on_host(HostMsg::Begin { generation: 1, pages: 2 }, 0);
        app.on_host(HostMsg::Page { generation: 1, index: 0, keys: vec![key("A"), key("B")] }, 0);
        app.on_host(HostMsg::Page { generation: 1, index: 1, keys: vec![key("C")] }, 0);
        assert!(app.on_host(HostMsg::Commit { generation: 1 }, 0).is_empty());
        app.on_host(HostMsg::Goto { page: Some(0) }, 0);
        app
    }

    fn tap_key(i: usize) -> Gesture {
        let r = geometry::key_rect(i);
        Gesture::Tap { x: r.x + r.w / 2, y: r.y + r.h / 2 }
    }

    fn at(gesture: Gesture, down_at: u64) -> Event {
        Event { gesture, down_at }
    }

    #[test]
    fn hello_reports_version_and_flags_mismatch() {
        let mut app = App::new("fw1");
        assert_eq!(
            app.on_host(HostMsg::Hello { proto: PROTO }, 0),
            [DeviceMsg::Hello { proto: PROTO, fw: "fw1".into() }, DeviceMsg::Screen { page: None }]
        );
        assert!(matches!(app.on_host(HostMsg::Hello { proto: PROTO + 1 }, 0)[1], DeviceMsg::Err { .. }));
    }

    #[test]
    fn press_carries_the_generation_it_was_made_against() {
        let mut app = ready();
        assert_eq!(app.on_gesture(at(tap_key(1), 10), 10), [DeviceMsg::Press { generation: 1, page: 0, index: 1, long: false }]);
        assert!(app.on_gesture(at(tap_key(5), 10), 10).is_empty(), "no key there");
    }

    #[test]
    fn incomplete_layout_is_never_shown() {
        let mut app = ready();
        app.on_host(HostMsg::Begin { generation: 2, pages: 2 }, 0);
        app.on_host(HostMsg::Page { generation: 2, index: 0, keys: vec![key("X")] }, 0);
        assert!(matches!(app.on_host(HostMsg::Commit { generation: 2 }, 0)[..], [DeviceMsg::Err { .. }]));
        match app.view() {
            View::Deck { keys, .. } => assert_eq!(keys[0].label, "A"),
            _ => panic!("expected the old deck"),
        }
        // Presses still go out against the layout actually on screen.
        assert!(matches!(app.on_gesture(at(tap_key(0), 1), 1)[..], [DeviceMsg::Press { generation: 1, .. }]));
    }

    #[test]
    fn stale_key_update_is_rejected() {
        let mut app = ready();
        let out = app.on_host(HostMsg::Key { generation: 9, page: 0, index: 0, key: key("Z") }, 0);
        assert!(matches!(out[..], [DeviceMsg::Err { .. }]));
    }

    #[test]
    fn swipes_walk_dashboard_and_pages_and_stop_at_the_ends() {
        let mut app = ready();
        let swipe = |dir| at(Gesture::Swipe { dir }, 0);
        assert_eq!(app.on_gesture(swipe(Dir::Left), 0), [DeviceMsg::Screen { page: Some(1) }]);
        assert!(app.on_gesture(swipe(Dir::Left), 0).is_empty(), "already on the last page");
        app.on_gesture(swipe(Dir::Right), 0);
        assert_eq!(app.on_gesture(swipe(Dir::Right), 0), [DeviceMsg::Screen { page: None }]);
        assert!(app.on_gesture(swipe(Dir::Right), 0).is_empty(), "dashboard is the first screen");
    }

    #[test]
    fn prompt_ignores_touches_that_began_before_it_was_armed() {
        let mut app = ready();
        let buttons = vec!["Deny".to_string(), "Allow".to_string()];
        app.on_host(HostMsg::Prompt { id: 7, title: "Bash".into(), body: "ls".into(), buttons }, 1_000);
        let r = geometry::button_rect(1, 2);
        let allow = Gesture::Tap { x: r.x + 5, y: r.y + 5 };

        assert!(app.on_gesture(at(allow, 900), 1_100).is_empty(), "finger already down");
        assert!(app.on_gesture(at(allow, 1_200), 1_250).is_empty(), "inside the arming window");
        assert!(app.on_gesture(at(tap_key(0), 2_000), 2_000).is_empty(), "keys are covered by the prompt");
        assert_eq!(app.on_gesture(at(allow, 1_400), 1_450), [DeviceMsg::Answer { id: 7, button: 1 }]);
        assert!(matches!(app.view(), View::Deck { .. }));
    }

    #[test]
    fn dismiss_only_clears_the_matching_prompt() {
        let mut app = ready();
        app.on_host(HostMsg::Prompt { id: 1, title: "t".into(), body: "b".into(), buttons: vec!["OK".into()] }, 0);
        app.on_host(HostMsg::Dismiss { id: 2 }, 0);
        assert!(matches!(app.view(), View::Prompt(_)));
        app.on_host(HostMsg::Dismiss { id: 1 }, 0);
        assert!(!matches!(app.view(), View::Prompt(_)));
    }

    #[test]
    fn link_loss_drops_prompt_and_blocks_presses() {
        let mut app = ready();
        app.on_host(HostMsg::Prompt { id: 1, title: "t".into(), body: "b".into(), buttons: vec!["OK".into()] }, 0);
        app.tick(LINK_TIMEOUT_MS + 1);
        assert!(matches!(app.view(), View::NoDeck { link_up: false }));
        assert!(app.on_gesture(at(tap_key(0), LINK_TIMEOUT_MS + 2), LINK_TIMEOUT_MS + 2).is_empty());
    }

    #[test]
    fn idle_deck_returns_to_dashboard() {
        let mut app = ready();
        app.on_gesture(at(tap_key(0), 100), 100);
        app.on_host(HostMsg::Ping, IDLE_MS);
        assert!(app.tick(IDLE_MS).is_empty());
        app.on_host(HostMsg::Ping, IDLE_MS + 101);
        assert_eq!(app.tick(IDLE_MS + 101), [DeviceMsg::Screen { page: None }]);
    }

    #[test]
    fn commit_with_fewer_pages_moves_off_a_vanished_page() {
        let mut app = ready();
        app.on_host(HostMsg::Goto { page: Some(1) }, 0);
        app.on_host(HostMsg::Begin { generation: 2, pages: 1 }, 0);
        app.on_host(HostMsg::Page { generation: 2, index: 0, keys: vec![key("A")] }, 0);
        assert_eq!(app.on_host(HostMsg::Commit { generation: 2 }, 0), [DeviceMsg::Screen { page: Some(0) }]);
    }

    #[test]
    fn injected_gestures_go_through_the_same_rules() {
        let mut app = ready();
        let out = app.on_host(HostMsg::Inject { gesture: tap_key(0) }, 5);
        assert_eq!(out, [DeviceMsg::Press { generation: 1, page: 0, index: 0, long: false }]);
    }
}
