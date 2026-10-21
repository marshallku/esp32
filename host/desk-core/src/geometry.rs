//! Where things are on the 480×480 screen, and what a point lands on.
//!
//! Layout is fixed so the hand learns it: a status strip on top, a 3×3 key
//! grid below. Gaps between keys are dead zones on purpose — a touch on the
//! seam between two keys should press neither.

use crate::protocol::{KEYS_PER_PAGE, MAX_BUTTONS};

pub const W: u16 = 480;
pub const H: u16 = 480;

/// Chosen so the three key rows divide the remaining height exactly.
pub const STRIP_H: u16 = 42;
const GAP: u16 = 9;
pub const KEY_W: u16 = (W - 4 * GAP) / 3;
pub const KEY_H: u16 = (H - STRIP_H - 4 * GAP) / 3;

/// Prompt buttons share the bottom row, full width.
pub const BUTTON_H: u16 = 84;
const BUTTON_Y: u16 = H - GAP - BUTTON_H;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Key `i` of a page, row-major from the top left.
pub fn key_rect(i: usize) -> Rect {
    assert!(i < KEYS_PER_PAGE);
    let (col, row) = ((i % 3) as u16, (i / 3) as u16);
    Rect { x: GAP + col * (KEY_W + GAP), y: STRIP_H + GAP + row * (KEY_H + GAP), w: KEY_W, h: KEY_H }
}

pub fn key_at(x: u16, y: u16) -> Option<u8> {
    (0..KEYS_PER_PAGE).find(|&i| key_rect(i).contains(x, y)).map(|i| i as u8)
}

/// Button `i` of `count` prompt buttons, laid side by side.
pub fn button_rect(i: usize, count: usize) -> Rect {
    assert!((1..=MAX_BUTTONS).contains(&count) && i < count);
    let n = count as u16;
    let w = (W - (n + 1) * GAP) / n;
    Rect { x: GAP + i as u16 * (w + GAP), y: BUTTON_Y, w, h: BUTTON_H }
}

pub fn button_at(x: u16, y: u16, count: usize) -> Option<u8> {
    (0..count).find(|&i| button_rect(i, count).contains(x, y)).map(|i| i as u8)
}

/// A notification banner slides over the top of whatever screen is up.
pub const BANNER: Rect = Rect { x: 6, y: 6, w: W - 12, h: 92 };

/// The notifications screen: rows of `NOTE_ROW_H` under the status strip,
/// and a Clear button bottom right.
pub const NOTES_TOP: u16 = STRIP_H + 4;
pub const NOTE_ROW_H: u16 = 76;
pub const CLEAR_BUTTON: Rect = Rect { x: W - 126, y: H - 50, w: 120, h: 44 };
pub const NOTE_ROWS: usize = ((CLEAR_BUTTON.y - NOTES_TOP) / NOTE_ROW_H) as usize;

/// The visible notification row under `y`, counted from the top.
pub fn note_row_at(y: u16) -> Option<usize> {
    (y >= NOTES_TOP && y < NOTES_TOP + NOTE_ROW_H * NOTE_ROWS as u16).then(|| ((y - NOTES_TOP) / NOTE_ROW_H) as usize)
}

/// The prompt body's text box, between the title and the buttons.
pub const BODY_TOP: u16 = 64;
pub const BODY_BOTTOM: u16 = BUTTON_Y - GAP;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_fills_the_screen_without_overlap() {
        let last = key_rect(8);
        assert_eq!(last.x + last.w + GAP, W);
        assert_eq!(last.y + last.h + GAP, H);
        for i in 0..KEYS_PER_PAGE {
            for j in 0..KEYS_PER_PAGE {
                if i != j {
                    let (a, b) = (key_rect(i), key_rect(j));
                    let overlap = a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
                    assert!(!overlap, "{i} overlaps {j}");
                }
            }
        }
    }

    #[test]
    fn hits_centres_and_misses_gaps_and_strip() {
        for i in 0..KEYS_PER_PAGE {
            let r = key_rect(i);
            assert_eq!(key_at(r.x + r.w / 2, r.y + r.h / 2), Some(i as u8));
        }
        assert_eq!(key_at(240, 10), None, "status strip");
        let r = key_rect(0);
        assert_eq!(key_at(r.x + r.w + 1, r.y + 10), None, "gap between keys");
    }

    #[test]
    fn buttons_split_the_row() {
        for count in 1..=MAX_BUTTONS {
            let last = button_rect(count - 1, count);
            assert!(last.x + last.w <= W);
            for i in 0..count {
                let r = button_rect(i, count);
                assert_eq!(button_at(r.x + r.w / 2, r.y + r.h / 2, count), Some(i as u8));
            }
        }
    }
}
