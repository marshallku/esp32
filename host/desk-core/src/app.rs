//! The panel's behaviour, minus pixels: which screen is up, what the host has
//! pushed, what a gesture means right now, and what to tell the host.
//!
//! The firmware feeds it host lines, gestures and clock ticks, sends whatever
//! messages come back, and redraws from `view` when `take_dirty` says so.
//! Keeping it free of hardware is what lets the rules that matter — a stale
//! layout cannot be pressed, a held finger cannot approve — be tested here.

use alloc::{collections::VecDeque, string::String, vec::Vec};

use crate::{
    geometry,
    gesture::Event,
    protocol::{
        DeviceMsg, Dir, Gesture, HostMsg, KEYS_PER_PAGE, Key, MAX_BUTTONS, MAX_NOTES, MAX_PAGES, Note, PROTO,
        ScreenId, Stats, URGENCY_CRITICAL,
    },
};

/// A prompt ignores touches that began less than this long after it appeared.
/// The hand that was reaching for a key when the modal popped up must not land
/// on "Allow".
pub const ARM_MS: u64 = 300;
/// The host pings every 2 s; three missed pings and it is gone.
pub const LINK_TIMEOUT_MS: u64 = 6_000;
/// A button page left untouched this long falls back to the dashboard.
pub const IDLE_MS: u64 = 60_000;
/// Stats arrive about once a second; older than this and the screen says so.
pub const STATS_STALE_MS: u64 = 5_000;
/// Samples kept for the stats screen's history graphs.
pub const HISTORY_LEN: usize = 120;
/// How long a new notification's banner covers the top of the screen.
pub const BANNER_MS: u64 = 4_000;
/// Half a blink of the border: on for this long, then off for as long.
pub const BLINK_MS: u64 = 300;
/// Blinks for an ordinary notification. A critical one blinks until seen.
pub const BLINKS: u64 = 3;

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

/// Recent (cpu %, memory %) samples, oldest first. `None` marks a gap — the
/// link was down or stats stopped — so a graph does not join two readings
/// minutes apart into one line.
#[derive(Default)]
pub struct History {
    samples: VecDeque<Option<(u8, u8)>>,
}

impl History {
    pub fn iter(&self) -> impl Iterator<Item = Option<(u8, u8)>> + '_ {
        self.samples.iter().copied()
    }

    fn push(&mut self, sample: Option<(u8, u8)>) {
        // Consecutive gaps say nothing more than one.
        if sample.is_none() && self.samples.back().is_some_and(Option::is_none) {
            return;
        }
        if self.samples.len() == HISTORY_LEN {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }
}

/// The notification feedback drawn on top of whatever screen is up.
#[derive(Default)]
pub struct Overlay<'a> {
    pub banner: Option<&'a Note>,
    /// Border lit this frame; `Some(true)` for a critical notification.
    pub border: Option<bool>,
}

struct Alert {
    id: u64,
    since: u64,
    critical: bool,
}

/// What to draw.
pub enum View<'a> {
    Dashboard,
    Notes { notes: &'a VecDeque<Note>, scroll: usize },
    Stats { stats: Option<&'a Stats>, stale: bool, history: &'a History },
    Deck { page: u8, keys: &'a [Key] },
    /// A button page was asked for but there is nothing to show on it.
    NoDeck { link_up: bool },
    Prompt(&'a Prompt),
}

pub struct App {
    fw: String,
    layout: Option<Layout>,
    staging: Option<Staging>,
    screen: ScreenId,
    prompt: Option<Prompt>,
    time: String,
    stats: Option<Stats>,
    stats_at: Option<u64>,
    /// Whether the stats screen was last drawn as stale.
    stats_stale: bool,
    history: History,
    /// Newest first.
    notes: VecDeque<Note>,
    notes_scroll: usize,
    alert: Option<Alert>,
    banner_on: bool,
    border_on: bool,
    /// When what lies under a finger last changed: the screen, the layout, a
    /// modal. A touch that began before this was aimed at something else and
    /// must not act on what is there now.
    surface_changed_at: u64,
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
            screen: ScreenId::Dash,
            prompt: None,
            time: String::new(),
            stats: None,
            stats_at: None,
            stats_stale: false,
            history: History::default(),
            notes: VecDeque::new(),
            notes_scroll: 0,
            alert: None,
            banner_on: false,
            border_on: false,
            surface_changed_at: 0,
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
            (ScreenId::Dash, _) => View::Dashboard,
            (ScreenId::Stats, _) => {
                View::Stats { stats: self.stats.as_ref(), stale: self.stats_stale, history: &self.history }
            }
            (ScreenId::Notes, _) => View::Notes { notes: &self.notes, scroll: self.notes_scroll },
            (ScreenId::Page(page), Some(layout)) if self.link_up => {
                View::Deck { page, keys: &layout.pages[page as usize] }
            }
            (ScreenId::Page(_), _) => View::NoDeck { link_up: self.link_up },
        }
    }

    pub fn screen(&self) -> ScreenId {
        self.screen
    }

    /// Notification feedback to draw over the screen. Nothing while a prompt
    /// is up: an approval must never be partly covered.
    pub fn overlay(&self) -> Overlay<'_> {
        if self.prompt.is_some() {
            return Overlay::default();
        }
        let alert = self.alert.as_ref();
        Overlay {
            banner: alert.filter(|_| self.banner_on).and_then(|a| self.notes.iter().find(|n| n.id == a.id)),
            border: alert.filter(|_| self.border_on).map(|a| a.critical),
        }
    }

    pub fn unread(&self) -> usize {
        self.notes.iter().filter(|n| n.unread).count()
    }

    /// Every screen in swipe order.
    pub fn screens(&self) -> Vec<ScreenId> {
        let pages = self.page_count().max(1) as u8;
        [ScreenId::Dash, ScreenId::Stats, ScreenId::Notes].into_iter().chain((0..pages).map(ScreenId::Page)).collect()
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
                out.push(DeviceMsg::Screen { screen: self.screen });
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
                    if let ScreenId::Page(p) = self.screen {
                        // New keys under the finger, even on the same page.
                        self.surface_changed_at = now;
                        if p >= count {
                            self.set_screen(ScreenId::Page(count - 1), now, &mut out);
                        }
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
                            if self.screen == ScreenId::Page(page) {
                                self.dirty = true;
                            }
                        }
                        None => out.push(err("key update out of range")),
                    }
                }
                _ => out.push(err("key update for a stale layout")),
            },
            HostMsg::Goto { screen } => {
                if self.screens().contains(&screen) {
                    self.set_screen(screen, now, &mut out);
                } else {
                    out.push(err("goto a screen that does not exist"));
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
                    self.surface_changed_at = now;
                    self.dirty = true;
                }
            }
            HostMsg::Dismiss { id } => {
                if self.prompt.as_ref().is_some_and(|p| p.id == id) {
                    self.prompt = None;
                    self.surface_changed_at = now;
                    self.dirty = true;
                }
            }
            HostMsg::Stats(stats) => {
                if self.stats_at.is_none_or(|t| now - t > STATS_STALE_MS) {
                    self.history.push(None);
                }
                let mem = match (stats.mem_used, stats.mem_total) {
                    (Some(used), Some(total)) if total > 0 => Some((u64::from(used) * 100 / u64::from(total)) as u8),
                    _ => None,
                };
                self.history.push(stats.cpu.zip(mem));
                self.stats = Some(stats);
                self.stats_at = Some(now);
                if self.screen == ScreenId::Stats {
                    self.stats_stale = false;
                    self.dirty = true;
                }
            }
            HostMsg::NotesReset => {
                self.notes.clear();
                self.notes_scroll = 0;
                self.alert = None;
                if self.screen == ScreenId::Notes {
                    self.surface_changed_at = now;
                }
                self.dirty = true;
            }
            HostMsg::Note { mut note, live } => {
                let seen_now = live && self.screen == ScreenId::Notes;
                if seen_now {
                    note.unread = false;
                }
                let id = note.id;
                let critical = note.urgency >= URGENCY_CRITICAL;
                // Same id: an update in place (a progress notification, a
                // resend after reconnect) keeps its position.
                match self.notes.iter_mut().find(|n| n.id == id) {
                    Some(slot) => *slot = note,
                    None => {
                        self.notes.push_front(note);
                        self.notes.truncate(MAX_NOTES);
                    }
                }
                if live && !seen_now {
                    // A critical alert still waiting to be seen stays
                    // critical: an ordinary one arriving after it must not
                    // turn its persistent blink into three blinks.
                    let critical = critical || self.alert.as_ref().is_some_and(|a| a.critical);
                    self.alert = Some(Alert { id, since: now, critical });
                    self.update_overlay(now);
                }
                if self.screen == ScreenId::Notes {
                    // Rows moved under any finger on the list.
                    self.surface_changed_at = now;
                }
                self.dirty = true;
            }
            HostMsg::Inject { gesture } => out.extend(self.on_gesture(Event { gesture, down_at: now }, now)),
        }
        out
    }

    pub fn on_gesture(&mut self, ev: Event, now: u64) -> Vec<DeviceMsg> {
        self.last_input = now;
        let mut out = Vec::new();

        // The finger went down on something that has since been replaced —
        // another screen, another profile's keys, a modal coming or going.
        // Acting on what is under it now would act on something unseen.
        if ev.down_at < self.surface_changed_at {
            return out;
        }

        if let Some(prompt) = &self.prompt {
            if let Gesture::Tap { x, y } = ev.gesture
                && ev.down_at >= prompt.shown_at + ARM_MS
                && let Some(button) = geometry::button_at(x, y, prompt.buttons.len())
            {
                out.push(DeviceMsg::Answer { id: prompt.id, button });
                self.prompt = None;
                self.surface_changed_at = now;
                self.dirty = true;
            }
            return out;
        }

        if self.banner_on
            && let Gesture::Tap { x, y } = ev.gesture
            && geometry::BANNER.contains(x, y)
        {
            self.set_screen(ScreenId::Notes, now, &mut out);
            return out;
        }

        if self.screen == ScreenId::Notes {
            match ev.gesture {
                Gesture::Swipe { dir: Dir::Up } => {
                    if self.notes_scroll + geometry::NOTE_ROWS < self.notes.len() {
                        self.notes_scroll += 1;
                        self.dirty = true;
                    }
                    return out;
                }
                Gesture::Swipe { dir: Dir::Down } => {
                    if self.notes_scroll > 0 {
                        self.notes_scroll -= 1;
                        self.dirty = true;
                    }
                    return out;
                }
                Gesture::Tap { x, y } if geometry::CLEAR_BUTTON.contains(x, y) => {
                    if !self.notes.is_empty() {
                        self.notes.clear();
                        self.notes_scroll = 0;
                        self.surface_changed_at = now;
                        self.dirty = true;
                        out.push(DeviceMsg::NotesClear);
                    }
                    return out;
                }
                Gesture::Long { y, .. } => {
                    if let Some(row) = geometry::note_row_at(y)
                        && let Some(note) = self.notes.remove(self.notes_scroll + row)
                    {
                        self.notes_scroll = self.notes_scroll.min(self.notes.len().saturating_sub(geometry::NOTE_ROWS));
                        self.surface_changed_at = now;
                        self.dirty = true;
                        out.push(DeviceMsg::NoteDismiss { id: note.id });
                    }
                    return out;
                }
                _ => {}
            }
        }

        match ev.gesture {
            Gesture::Swipe { dir } => {
                let screens = self.screens();
                let current = screens.iter().position(|s| *s == self.screen).unwrap_or(0);
                let next = match dir {
                    Dir::Left => current.checked_add(1),
                    Dir::Right => current.checked_sub(1),
                    Dir::Up | Dir::Down => None,
                };
                if let Some(&screen) = next.and_then(|i| screens.get(i)) {
                    self.set_screen(screen, now, &mut out);
                }
            }
            Gesture::Tap { x, y } | Gesture::Long { x, y } => {
                let long = matches!(ev.gesture, Gesture::Long { .. });
                if let (ScreenId::Page(page), Some(layout), true) = (self.screen, &self.layout, self.link_up)
                    && let Some(index) = geometry::key_at(x, y)
                    && (index as usize) < layout.pages[page as usize].len()
                {
                    out.push(DeviceMsg::Press { generation: layout.generation, page, index, long });
                }
            }
        }
        out
    }

    /// Time-driven transitions: the host going quiet, the panel going idle,
    /// stats going stale.
    pub fn tick(&mut self, now: u64) -> Vec<DeviceMsg> {
        let mut out = Vec::new();
        if self.link_up && self.last_host.is_none_or(|t| now - t > LINK_TIMEOUT_MS) {
            self.link_up = false;
            // An unanswerable prompt must not stay up looking answerable; the
            // host has timed it out on its side already.
            if self.prompt.take().is_some() {
                self.surface_changed_at = now;
            }
            self.dirty = true;
        }
        if self.prompt.is_none() && matches!(self.screen, ScreenId::Page(_)) && now - self.last_input > IDLE_MS {
            self.set_screen(ScreenId::Dash, now, &mut out);
        }
        let stale = self.stats_at.is_none_or(|t| now - t > STATS_STALE_MS);
        if self.screen == ScreenId::Stats && stale != self.stats_stale {
            self.stats_stale = stale;
            self.dirty = true;
        }
        self.update_overlay(now);
        out
    }

    /// Banner and border state for `now`. Redraws only when either flips.
    fn update_overlay(&mut self, now: u64) {
        let (banner, border) = match &self.alert {
            Some(a) => {
                let elapsed = now.saturating_sub(a.since);
                let phase = elapsed / BLINK_MS;
                let blinking = a.critical || phase < 2 * BLINKS;
                (elapsed < BANNER_MS, blinking && phase.is_multiple_of(2))
            }
            None => (false, false),
        };
        if banner != self.banner_on {
            self.banner_on = banner;
            // The banner covers part of the screen; a touch that began on
            // either side of it appearing or going belongs to neither.
            self.surface_changed_at = now;
            self.dirty = true;
        }
        if border != self.border_on {
            self.border_on = border;
            self.dirty = true;
        }
        // An ordinary alert is finished once it has stopped blinking and its
        // banner is gone; a critical one waits for the notes screen.
        if self.alert.as_ref().is_some_and(|a| !a.critical) && !banner && now.saturating_sub(self.alert.as_ref().map_or(0, |a| a.since)) >= 2 * BLINKS * BLINK_MS {
            self.alert = None;
        }
    }

    fn set_screen(&mut self, screen: ScreenId, now: u64, out: &mut Vec<DeviceMsg>) {
        if self.screen != screen {
            self.screen = screen;
            self.surface_changed_at = now;
            self.dirty = true;
            if screen == ScreenId::Stats {
                self.stats_stale = self.stats_at.is_none_or(|t| now - t > STATS_STALE_MS);
            }
            out.push(DeviceMsg::Screen { screen });
            if screen == ScreenId::Notes {
                // Opening the list is seeing it: unread marks go, and a
                // critical notification stops blinking.
                for n in &mut self.notes {
                    n.unread = false;
                }
                self.alert = None;
                self.update_overlay(now);
                out.push(DeviceMsg::NotesSeen);
            }
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
        app.on_host(HostMsg::Goto { screen: ScreenId::Page(0) }, 0);
        app
    }

    fn tap_key(i: usize) -> Gesture {
        let r = geometry::key_rect(i);
        Gesture::Tap { x: r.x + r.w / 2, y: r.y + r.h / 2 }
    }

    fn at(gesture: Gesture, down_at: u64) -> Event {
        Event { gesture, down_at }
    }

    fn screen(screen: ScreenId) -> DeviceMsg {
        DeviceMsg::Screen { screen }
    }

    #[test]
    fn hello_reports_version_and_flags_mismatch() {
        let mut app = App::new("fw1");
        assert_eq!(
            app.on_host(HostMsg::Hello { proto: PROTO }, 0),
            [DeviceMsg::Hello { proto: PROTO, fw: "fw1".into() }, screen(ScreenId::Dash)]
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
    fn swipes_walk_every_screen_in_order_and_stop_at_the_ends() {
        let mut app = ready();
        let swipe = |dir| at(Gesture::Swipe { dir }, 0);
        assert_eq!(app.on_gesture(swipe(Dir::Left), 0), [screen(ScreenId::Page(1))]);
        assert!(app.on_gesture(swipe(Dir::Left), 0).is_empty(), "already on the last page");
        app.on_gesture(swipe(Dir::Right), 0);
        assert_eq!(app.on_gesture(swipe(Dir::Right), 0), [screen(ScreenId::Notes), DeviceMsg::NotesSeen]);
        assert_eq!(app.on_gesture(swipe(Dir::Right), 0), [screen(ScreenId::Stats)]);
        assert_eq!(app.on_gesture(swipe(Dir::Right), 0), [screen(ScreenId::Dash)]);
        assert!(app.on_gesture(swipe(Dir::Right), 0).is_empty(), "dashboard is the first screen");
        assert!(app.on_gesture(swipe(Dir::Up), 0).is_empty(), "vertical swipes do not change screens");
    }

    #[test]
    fn a_touch_that_began_on_another_surface_does_nothing() {
        let mut app = ready();
        // Finger down at 100 on profile A's keys; profile B commits at 150.
        app.on_host(HostMsg::Begin { generation: 2, pages: 1 }, 150);
        app.on_host(HostMsg::Page { generation: 2, index: 0, keys: vec![key("Danger")] }, 150);
        app.on_host(HostMsg::Commit { generation: 2 }, 150);
        assert!(app.on_gesture(at(tap_key(0), 100), 200).is_empty(), "release lands on keys never seen");
        assert!(matches!(app.on_gesture(at(tap_key(0), 300), 350)[..], [DeviceMsg::Press { generation: 2, .. }]));

        // Same for a screen change under a held finger.
        app.on_host(HostMsg::Goto { screen: ScreenId::Dash }, 400);
        app.on_host(HostMsg::Goto { screen: ScreenId::Page(0) }, 450);
        assert!(app.on_gesture(at(tap_key(0), 420), 500).is_empty());
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
        assert!(app.on_gesture(at(tap_key(0), 1_420), 1_500).is_empty(), "the answering finger does not also press");
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
    fn idle_deck_returns_to_dashboard_but_stats_stays() {
        let mut app = ready();
        app.on_gesture(at(tap_key(0), 100), 100);
        app.on_host(HostMsg::Ping, IDLE_MS);
        assert!(app.tick(IDLE_MS).is_empty());
        app.on_host(HostMsg::Ping, IDLE_MS + 101);
        assert_eq!(app.tick(IDLE_MS + 101), [screen(ScreenId::Dash)]);

        app.on_host(HostMsg::Goto { screen: ScreenId::Stats }, IDLE_MS + 200);
        app.on_host(HostMsg::Ping, 3 * IDLE_MS);
        assert!(app.tick(3 * IDLE_MS).is_empty(), "a stats screen is left up on purpose");
    }

    #[test]
    fn commit_with_fewer_pages_moves_off_a_vanished_page() {
        let mut app = ready();
        app.on_host(HostMsg::Goto { screen: ScreenId::Page(1) }, 0);
        app.on_host(HostMsg::Begin { generation: 2, pages: 1 }, 0);
        app.on_host(HostMsg::Page { generation: 2, index: 0, keys: vec![key("A")] }, 0);
        assert_eq!(app.on_host(HostMsg::Commit { generation: 2 }, 0), [screen(ScreenId::Page(0))]);
    }

    #[test]
    fn goto_rejects_screens_that_do_not_exist() {
        let mut app = ready();
        assert!(matches!(app.on_host(HostMsg::Goto { screen: ScreenId::Page(5) }, 0)[..], [DeviceMsg::Err { .. }]));
    }

    #[test]
    fn stats_history_marks_gaps_and_staleness() {
        let mut app = ready();
        let stats = |cpu| HostMsg::Stats(Stats { cpu: Some(cpu), mem_used: Some(1), mem_total: Some(4), ..Default::default() });
        app.on_host(stats(10), 1_000);
        app.on_host(stats(20), 2_000);
        app.on_host(stats(30), 2_000 + STATS_STALE_MS + 1);
        let samples: Vec<_> = app.history.iter().collect();
        assert_eq!(samples, [None, Some((10, 25)), Some((20, 25)), None, Some((30, 25))]);

        app.on_host(HostMsg::Goto { screen: ScreenId::Stats }, 8_000);
        app.take_dirty();
        app.on_host(HostMsg::Ping, 8_000 + STATS_STALE_MS);
        app.tick(8_000 + STATS_STALE_MS);
        assert!(app.take_dirty(), "crossing into stale redraws");
        assert!(matches!(app.view(), View::Stats { stale: true, .. }));
    }

    fn note(id: u64, urgency: u8) -> Note {
        Note { id, app: "app".into(), summary: "s".into(), urgency, time: "10:00".into(), unread: true, ..Default::default() }
    }

    fn overlay(app: &App) -> (bool, Option<bool>) {
        let o = app.overlay();
        (o.banner.is_some(), o.border)
    }

    #[test]
    fn a_live_note_shows_a_banner_and_blinks_three_times() {
        let mut app = ready();
        app.on_host(HostMsg::Note { note: note(1, 1), live: true }, 1_000);
        assert_eq!(overlay(&app), (true, Some(false)));
        app.tick(1_000 + BLINK_MS);
        assert_eq!(overlay(&app).1, None, "off half of the first blink");
        app.tick(1_000 + 2 * BLINK_MS);
        assert_eq!(overlay(&app).1, Some(false));
        app.tick(1_000 + 2 * BLINKS * BLINK_MS);
        assert_eq!(overlay(&app), (true, None), "blinking over, banner still up");
        app.tick(1_000 + BANNER_MS);
        assert_eq!(overlay(&app), (false, None));
        app.tick(1_000 + BANNER_MS + 10 * BLINK_MS);
        assert_eq!(overlay(&app), (false, None), "an ordinary alert does not come back");
        assert_eq!(app.unread(), 1);
    }

    #[test]
    fn a_critical_note_blinks_until_the_notes_screen_is_opened() {
        let mut app = ready();
        app.on_host(HostMsg::Note { note: note(1, URGENCY_CRITICAL), live: true }, 0);
        // An even phase number is an "on" half of a blink.
        app.tick(120 * BLINK_MS);
        assert_eq!(overlay(&app), (false, Some(true)), "long after the banner, still blinking");
        app.on_host(HostMsg::Goto { screen: ScreenId::Notes }, 121 * BLINK_MS);
        app.tick(122 * BLINK_MS);
        assert_eq!(overlay(&app), (false, None));
        assert_eq!(app.unread(), 0);
    }

    #[test]
    fn tapping_the_banner_opens_the_notes_and_a_held_finger_does_not() {
        let mut app = ready();
        let r = geometry::BANNER;
        let on_banner = Gesture::Tap { x: r.x + 10, y: r.y + 10 };
        app.on_host(HostMsg::Note { note: note(1, 1), live: true }, 1_000);
        assert!(app.on_gesture(at(on_banner, 900), 1_100).is_empty(), "finger was down before the banner");
        assert_eq!(app.on_gesture(at(on_banner, 1_200), 1_250), [screen(ScreenId::Notes), DeviceMsg::NotesSeen]);

        // Once the banner is gone, the same spot is the status strip again.
        app.on_host(HostMsg::Goto { screen: ScreenId::Page(0) }, 2_000);
        app.on_host(HostMsg::Note { note: note(2, 1), live: true }, 3_000);
        app.tick(3_000 + BANNER_MS);
        assert!(app.on_gesture(at(on_banner, 3_000 + BANNER_MS - 50), 3_000 + BANNER_MS + 50).is_empty());
    }

    #[test]
    fn notes_update_in_place_resync_quietly_and_cap() {
        let mut app = ready();
        app.on_host(HostMsg::Note { note: note(1, 1), live: true }, 0);
        app.on_host(HostMsg::Note { note: note(2, 1), live: true }, 0);
        let mut updated = note(1, 1);
        updated.summary = "progress 50%".into();
        app.on_host(HostMsg::Note { note: updated, live: true }, 0);
        let ids: Vec<u64> = app.notes.iter().map(|n| n.id).collect();
        assert_eq!(ids, [2, 1], "an update keeps its place");

        app.on_host(HostMsg::NotesReset, 10_000);
        app.tick(10_000);
        for id in 0..(MAX_NOTES as u64 + 5) {
            let mut n = note(id, 1);
            n.unread = id % 2 == 0;
            app.on_host(HostMsg::Note { note: n, live: false }, 10_000);
        }
        assert_eq!(app.notes.len(), MAX_NOTES);
        assert_eq!(app.notes[0].id, MAX_NOTES as u64 + 4, "newest first");
        assert_eq!(overlay(&app), (false, None), "a resync is not news");
        assert!(app.unread() > 0, "unread state survives a resync");
    }

    #[test]
    fn notes_screen_scrolls_dismisses_and_clears() {
        let mut app = ready();
        for id in 0..8 {
            app.on_host(HostMsg::Note { note: note(id, 1), live: false }, 0);
        }
        app.on_host(HostMsg::Goto { screen: ScreenId::Notes }, 0);
        let swipe = |dir| at(Gesture::Swipe { dir }, 10);
        app.on_gesture(swipe(Dir::Up), 10);
        assert_eq!(app.notes_scroll, 1);
        for _ in 0..20 {
            app.on_gesture(swipe(Dir::Up), 10);
        }
        assert_eq!(app.notes_scroll, 8 - geometry::NOTE_ROWS, "stops with the last row at the bottom");

        let first_row_y = geometry::NOTES_TOP + 5;
        let shown = app.notes[app.notes_scroll].id;
        let out = app.on_gesture(at(Gesture::Long { x: 100, y: first_row_y }, 20), 20);
        assert_eq!(out, [DeviceMsg::NoteDismiss { id: shown }]);
        assert_eq!(app.notes.len(), 7);

        let c = geometry::CLEAR_BUTTON;
        let out = app.on_gesture(at(Gesture::Tap { x: c.x + 5, y: c.y + 5 }, 30), 30);
        assert_eq!(out, [DeviceMsg::NotesClear]);
        assert!(app.notes.is_empty());
    }

    #[test]
    fn an_ordinary_note_does_not_downgrade_a_pending_critical_one() {
        let mut app = ready();
        app.on_host(HostMsg::Note { note: note(1, URGENCY_CRITICAL), live: true }, 0);
        app.on_host(HostMsg::Note { note: note(2, 1), live: true }, 1_000);
        // An even phase long after three blinks would have ended.
        app.tick(1_000 + 100 * BLINK_MS);
        assert_eq!(overlay(&app), (false, Some(true)));
    }

    #[test]
    fn a_note_arriving_under_a_held_row_cancels_the_hold() {
        let mut app = ready();
        app.on_host(HostMsg::Note { note: note(1, 1), live: false }, 0);
        app.on_host(HostMsg::Goto { screen: ScreenId::Notes }, 0);
        // Finger down on row 0 (note 1) at 100; note 2 arrives and takes row 0.
        app.on_host(HostMsg::Note { note: note(2, 1), live: true }, 200);
        let row0 = geometry::NOTES_TOP + 5;
        assert!(app.on_gesture(at(Gesture::Long { x: 100, y: row0 }, 100), 700).is_empty());
        assert_eq!(app.notes.len(), 2);
    }

    #[test]
    fn a_prompt_hides_notification_feedback() {
        let mut app = ready();
        app.on_host(HostMsg::Note { note: note(1, URGENCY_CRITICAL), live: true }, 0);
        app.on_host(HostMsg::Prompt { id: 9, title: "t".into(), body: "b".into(), buttons: vec!["OK".into()] }, 10);
        assert_eq!(overlay(&app), (false, None));
    }

    #[test]
    fn injected_gestures_go_through_the_same_rules() {
        let mut app = ready();
        let out = app.on_host(HostMsg::Inject { gesture: tap_key(0) }, 5);
        assert_eq!(out, [DeviceMsg::Press { generation: 1, page: 0, index: 0, long: false }]);
    }
}
