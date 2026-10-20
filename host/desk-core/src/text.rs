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
    fn fit_rejects_overflow_and_non_ascii() {
        assert!(fits_body("rm -rf target"));
        assert!(!fits_body(&"x".repeat(BODY_COLS * BODY_ROWS + 1)));
        assert!(fits_body(&"x".repeat(BODY_COLS * BODY_ROWS)));
        assert!(!fits_body("echo hi\u{202e}"), "bidi override");
        assert!(!fits_body("rm\tx"), "tab renders as nothing");
    }
}
