//! Text wrapping for the panel's fixed-width font.
//!
//! The panel draws with 10×20 ASCII glyphs, so a line holds a fixed number of
//! characters. The daemon uses the same functions to decide, before it ever
//! sends a prompt, whether the whole of it will be visible — an approval the
//! user cannot read in full must not offer "Allow".

use alloc::vec::Vec;

use crate::geometry::{BODY_BOTTOM, BODY_TOP, KEY_W, W};

pub const GLYPH_W: u16 = 10;
pub const GLYPH_H: u16 = 20;
const MARGIN: u16 = 10;

pub const BODY_COLS: usize = ((W - 2 * MARGIN) / GLYPH_W) as usize;

/// Characters per line of a key label in the 10×20 font, with a 4 px margin.
pub const LABEL_COLS: usize = (KEY_W as usize - 8) / GLYPH_W as usize;
/// A label shows at most two lines; anything longer would be cut off.
pub const LABEL_MAX: usize = 2 * LABEL_COLS;
/// The subtitle is one line of the 6×10 font.
pub const SUB_MAX: usize = (KEY_W as usize - 8) / 6;
pub const BODY_ROWS: usize = ((BODY_BOTTOM - BODY_TOP) / GLYPH_H) as usize;

/// Line height of the Korean-capable font (GNU Unifont via u8g2).
pub const UNIFONT_H: u16 = 16;

/// Width of `c` in GNU Unifont: 8 px for half-width scripts (Latin, Greek,
/// Cyrillic…), 16 px from Hangul Jamo up — Hangul, CJK and most symbols.
pub fn unifont_width(c: char) -> u16 {
    if (c as u32) < 0x1100 { 8 } else { 16 }
}

/// Wraps `text` to lines at most `max_px` wide, measuring each character with
/// `width`; also breaks at newlines. By character, not word, like `wrap`.
pub fn wrap_px(text: &str, max_px: u16, width: fn(char) -> u16) -> Vec<&str> {
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let (mut start, mut used) = (0, 0);
        for (i, c) in raw.char_indices() {
            let w = width(c);
            if used + w > max_px && i > start {
                lines.push(&raw[start..i]);
                (start, used) = (i, 0);
            }
            used += w;
        }
        lines.push(&raw[start..]);
    }
    lines
}

/// Whether a key label shows in full: two lines of the 10×20 font if it is
/// plain ASCII, otherwise two lines of Unifont.
pub fn label_fits(label: &str) -> bool {
    if label.is_ascii() {
        return label.len() <= LABEL_MAX;
    }
    wrap_px(label, KEY_W - 8, unifont_width).len() <= 2
}

/// Whether a key's subtitle shows in full: one line of the 6×10 font if it
/// is plain ASCII, otherwise one line of Unifont.
pub fn sub_fits(sub: &str) -> bool {
    if sub.is_ascii() {
        return sub.len() <= SUB_MAX;
    }
    wrap_px(sub, KEY_W - 8, unifont_width).len() <= 1
}

/// Hard-wraps `text` at `cols`, also breaking at newlines. Breaks are by
/// character count, not word, because commands and paths are what gets shown
/// and those must appear byte for byte rather than reflowed.
pub fn wrap(text: &str, cols: usize) -> Vec<&str> {
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let mut rest = raw;
        if rest.is_empty() {
            lines.push(rest);
        }
        while !rest.is_empty() {
            let cut = rest.char_indices().nth(cols).map_or(rest.len(), |(i, _)| i);
            lines.push(&rest[..cut]);
            rest = &rest[cut..];
        }
    }
    lines
}

/// Whether `body` shows completely, character for character, in a prompt.
/// Anything outside printable ASCII fails: the font cannot draw it, and a
/// glyph that silently renders as something else is how a command hides.
pub fn fits_body(body: &str) -> bool {
    body.chars().all(|c| c == '\n' || (' '..='~').contains(&c)) && wrap(body, BODY_COLS).len() <= BODY_ROWS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_by_character_and_newline() {
        assert_eq!(wrap("abcdef", 4), ["abcd", "ef"]);
        assert_eq!(wrap("ab\ncd", 4), ["ab", "cd"]);
        assert_eq!(wrap("ab\n\ncd", 4), ["ab", "", "cd"]);
        assert_eq!(wrap("abcd", 4), ["abcd"]);
    }

    #[test]
    fn pixel_wrap_counts_hangul_double() {
        // 140 px: 17 half-width or 8 wide characters per line.
        assert_eq!(wrap_px("가나다라마바사아자", 140, unifont_width), ["가나다라마바사아", "자"]);
        assert_eq!(wrap_px("abc가", 40, unifont_width), ["abc가"], "exactly full");
        assert_eq!(wrap_px("abc가", 39, unifont_width), ["abc", "가"]);
        assert_eq!(wrap_px("a\nb", 40, unifont_width), ["a", "b"]);
        assert_eq!(wrap_px("", 40, unifont_width), [""]);
    }

    #[test]
    fn labels_fit_by_their_font() {
        assert!(label_fits(&"x".repeat(LABEL_MAX)));
        assert!(!label_fits(&"x".repeat(LABEL_MAX + 1)));
        assert!(label_fits("터미널"));
        assert!(label_fits(&"가".repeat(16)), "two lines of eight");
        assert!(!label_fits(&"가".repeat(17)));
    }

    #[test]
    fn fit_rejects_overflow_and_non_ascii() {
        assert!(fits_body("rm -rf target"));
        assert!(!fits_body(&"x".repeat(BODY_COLS * BODY_ROWS + 1)));
        assert!(fits_body(&"x".repeat(BODY_COLS * BODY_ROWS)));
        assert!(!fits_body("echo hi\u{202e}"), "bidi override");
        assert!(!fits_body("rm\tx"), "tab renders as nothing");
    }
}
