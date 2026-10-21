//! deskd's copy of the notification history, outliving any one connection to
//! the panel: after a reconnect it is replayed whole (reset, then oldest to
//! newest), so the panel ends up holding exactly this — evictions, dismissals
//! and read state included — rather than whatever it had plus a replay.

use std::collections::VecDeque;

use desk_core::protocol::{HostMsg, MAX_NOTES, Note};

use crate::notify::Incoming;

#[derive(Default)]
pub struct Notes {
    /// Newest first, like the panel.
    notes: VecDeque<(u64, Note)>,
    next_id: u64,
}

impl Notes {
    /// Records a notification and returns it as the panel should get it.
    /// The daemon replacing one of its own (same key) updates it in place.
    pub fn add(&mut self, incoming: Incoming, time: String) -> Note {
        if let Some((_, note)) = self.notes.iter_mut().find(|(key, _)| *key == incoming.key) {
            note.app = incoming.app;
            note.summary = incoming.summary;
            note.body = incoming.body;
            note.urgency = incoming.urgency;
            note.time = time;
            note.unread = true;
            return note.clone();
        }
        self.next_id += 1;
        let note = Note {
            id: self.next_id,
            app: incoming.app,
            summary: incoming.summary,
            body: incoming.body,
            urgency: incoming.urgency,
            time,
            unread: true,
        };
        self.notes.push_front((incoming.key, note.clone()));
        self.notes.truncate(MAX_NOTES);
        note
    }

    pub fn seen(&mut self) {
        for (_, note) in &mut self.notes {
            note.unread = false;
        }
    }

    pub fn seen_one(&mut self, id: u64) {
        if let Some((_, note)) = self.notes.iter_mut().find(|(_, n)| n.id == id) {
            note.unread = false;
        }
    }

    pub fn dismiss(&mut self, id: u64) {
        self.notes.retain(|(_, n)| n.id != id);
    }

    pub fn clear(&mut self) {
        self.notes.clear();
    }

    pub fn unread(&self) -> usize {
        self.notes.iter().filter(|(_, n)| n.unread).count()
    }

    pub fn len(&self) -> usize {
        self.notes.len()
    }

    /// Everything, as a resync: reset first, then oldest to newest so each
    /// insert at the front leaves the panel in the same order.
    pub fn resync(&self) -> Vec<HostMsg> {
        std::iter::once(HostMsg::NotesReset)
            .chain(self.notes.iter().rev().map(|(_, note)| HostMsg::Note { note: note.clone(), live: false }))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn incoming(key: u64, summary: &str) -> Incoming {
        Incoming { key, app: "a".into(), summary: summary.into(), body: String::new(), urgency: 1 }
    }

    #[test]
    fn replacement_keeps_the_id_and_resync_preserves_order_and_state() {
        let mut n = Notes::default();
        let first = n.add(incoming(100, "one"), "10:00".into());
        n.add(incoming(200, "two"), "10:01".into());
        n.seen();
        let updated = n.add(incoming(100, "one, updated"), "10:02".into());
        assert_eq!(updated.id, first.id, "same daemon id → same note");

        let msgs = n.resync();
        assert!(matches!(msgs[0], HostMsg::NotesReset));
        let order: Vec<(&str, bool)> = msgs[1..]
            .iter()
            .map(|m| match m {
                HostMsg::Note { note, live: false } => (note.summary.as_str(), note.unread),
                other => panic!("{other:?}"),
            })
            .collect();
        // Oldest first on the wire — the panel inserts each at the front and
        // ends up newest first. The update kept its original (older) place.
        assert_eq!(order, [("one, updated", true), ("two", false)]);
    }

    #[test]
    fn history_is_capped_and_dismissals_stick() {
        let mut n = Notes::default();
        for k in 0..(MAX_NOTES as u64 + 3) {
            n.add(incoming(k, "x"), "t".into());
        }
        assert_eq!(n.len(), MAX_NOTES);
        let newest = n.notes[0].1.id;
        n.dismiss(newest);
        assert_eq!(n.len(), MAX_NOTES - 1);
        assert!(n.resync().iter().all(|m| !matches!(m, HostMsg::Note { note, .. } if note.id == newest)));
    }
}
