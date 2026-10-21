//! What to send the panel next, by priority, with stale messages folded away.
//!
//! The link is stop-and-wait over a serial port, so everything queued waits
//! its turn, and a plain FIFO lets the cheap, frequent things — stats every
//! second, a profile switch on every focus change — bury the ones that matter.
//! Priorities, highest first:
//!
//! 1. urgent: handshake, ping, approval prompts, screen changes, test input
//! 2. a layout transfer already started (begin … commit), to its end
//! 3. a new layout: only a flag until its turn, then built from the deck's
//!    current state — so ten focus changes in a row cost one transfer, and
//!    key updates queued before it are dropped because it already holds them
//! 4. key updates, one per key, newest wins
//! 5. the clock, then stats: newest only

use std::collections::VecDeque;

use desk_core::protocol::HostMsg;

#[derive(Default)]
pub struct Outbox {
    urgent: VecDeque<HostMsg>,
    layout: VecDeque<HostMsg>,
    needs_push: bool,
    keys: Vec<HostMsg>,
    time: Option<HostMsg>,
    stats: Option<HostMsg>,
}

impl Outbox {
    pub fn urgent(&mut self, msg: HostMsg) {
        self.urgent.push_back(msg);
    }

    /// Asks for the whole layout to be (re)sent at the next opportunity.
    pub fn push_layout(&mut self) {
        self.needs_push = true;
        self.keys.clear();
    }

    pub fn key(&mut self, msg: HostMsg) {
        if self.needs_push {
            // The coming push renders the current state already.
            return;
        }
        let HostMsg::Key { page, index, .. } = &msg else { return };
        let (page, index) = (*page, *index);
        self.keys.retain(|k| !matches!(k, HostMsg::Key { page: p, index: i, .. } if (*p, *i) == (page, index)));
        self.keys.push(msg);
    }

    pub fn time(&mut self, msg: HostMsg) {
        self.time = Some(msg);
    }

    pub fn stats(&mut self, msg: HostMsg) {
        self.stats = Some(msg);
    }

    /// Forgets everything: the panel rebooted and holds none of it.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The next line to send. `render` builds a full layout push and is only
    /// called when one is due.
    pub fn next(&mut self, render: impl FnOnce() -> Vec<HostMsg>) -> Option<HostMsg> {
        if let Some(msg) = self.urgent.pop_front() {
            return Some(msg);
        }
        if self.layout.is_empty() && self.needs_push {
            self.needs_push = false;
            self.layout.extend(render());
        }
        if let Some(msg) = self.layout.pop_front() {
            return Some(msg);
        }
        if !self.keys.is_empty() {
            return Some(self.keys.remove(0));
        }
        self.time.take().or_else(|| self.stats.take())
    }
}

#[cfg(test)]
mod tests {
    use desk_core::protocol::{Key, ScreenId, Stats};

    use super::*;

    fn layout(generation: u32) -> Vec<HostMsg> {
        vec![
            HostMsg::Begin { generation, pages: 1 },
            HostMsg::Page { generation, index: 0, keys: vec![] },
            HostMsg::Commit { generation },
        ]
    }

    fn key(index: u8, label: &str) -> HostMsg {
        HostMsg::Key { generation: 1, page: 0, index, key: Key { label: label.into(), ..Default::default() } }
    }

    fn drain(o: &mut Outbox, mut generation: u32) -> Vec<HostMsg> {
        std::iter::from_fn(|| {
            o.next(|| {
                generation += 1;
                layout(generation)
            })
        })
        .collect()
    }

    #[test]
    fn many_push_requests_cost_one_transfer_built_at_send_time() {
        let mut o = Outbox::default();
        for _ in 0..10 {
            o.push_layout();
        }
        let sent = drain(&mut o, 0);
        assert_eq!(sent, layout(1));
    }

    #[test]
    fn urgent_messages_cut_into_a_layout_transfer() {
        let mut o = Outbox::default();
        o.push_layout();
        let first = o.next(|| layout(1)).unwrap();
        assert!(matches!(first, HostMsg::Begin { .. }));
        o.urgent(HostMsg::Goto { screen: ScreenId::Stats });
        o.push_layout();
        let rest = drain(&mut o, 1);
        // The prompt-class message goes next; the started transfer finishes
        // before the newer layout starts.
        assert!(matches!(rest[0], HostMsg::Goto { .. }));
        assert!(matches!(rest[1], HostMsg::Page { generation: 1, .. }));
        assert!(matches!(rest[2], HostMsg::Commit { generation: 1 }));
        assert!(matches!(rest[3], HostMsg::Begin { generation: 2, .. }));
    }

    #[test]
    fn key_updates_coalesce_per_key_and_vanish_into_a_pending_push() {
        let mut o = Outbox::default();
        o.key(key(0, "a"));
        o.key(key(1, "b"));
        o.key(key(0, "c"));
        assert_eq!(drain(&mut o, 0), [key(1, "b"), key(0, "c")]);

        o.key(key(0, "a"));
        o.push_layout();
        o.key(key(0, "b"));
        assert_eq!(drain(&mut o, 0), layout(1));
    }

    #[test]
    fn clock_and_stats_keep_only_the_latest_and_come_last() {
        let mut o = Outbox::default();
        o.stats(HostMsg::Stats(Stats { cpu: Some(1), ..Default::default() }));
        o.stats(HostMsg::Stats(Stats { cpu: Some(2), ..Default::default() }));
        o.time(HostMsg::Time { text: "10:00".into() });
        o.key(key(0, "k"));
        let sent = drain(&mut o, 0);
        assert_eq!(sent.len(), 3);
        assert!(matches!(sent[0], HostMsg::Key { .. }));
        assert!(matches!(sent[1], HostMsg::Time { .. }));
        assert_eq!(sent[2], HostMsg::Stats(Stats { cpu: Some(2), ..Default::default() }));
    }
}
