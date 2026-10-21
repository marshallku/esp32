//! Text in any script the notifications bring, via GNU Unifont from u8g2
//! (`unifont_t_korean2`: ASCII, Latin and the common Hangul syllables).
//!
//! The embedded-graphics mono fonts used for the panel's own chrome are ASCII
//! only, so anything that came from outside — a notification, a Korean key
//! label — goes through here. Characters the subset lacks are skipped rather
//! than failing the whole string. Layout uses `desk_core::text::unifont_width`,
//! the same measure deskd validates labels with, so what deskd judged to fit
//! is what fits.

use desk_core::text::{UNIFONT_H, unifont_width, wrap_px};
use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use u8g2_fonts::{
    FontRenderer, fonts,
    types::{FontColor, VerticalPosition},
};

const FONT: FontRenderer = FontRenderer::new::<fonts::u8g2_font_unifont_t_korean2>().with_ignore_unknown_chars(true);

pub const LINE_H: i32 = UNIFONT_H as i32;

pub fn width(text: &str) -> i32 {
    text.chars().map(|c| i32::from(unifont_width(c))).sum()
}

/// `text` with its top-left corner at (x, y).
pub fn draw<D: DrawTarget<Color = Rgb565>>(target: &mut D, text: &str, x: i32, y: i32, color: Rgb565) {
    let _ = FONT.render(text, Point::new(x, y), VerticalPosition::Top, FontColor::Transparent(color), target);
}

/// `text` centred on `cx`, top at `y`.
pub fn draw_centered<D: DrawTarget<Color = Rgb565>>(target: &mut D, text: &str, cx: i32, y: i32, color: Rgb565) {
    draw(target, text, cx - width(text) / 2, y, color);
}

/// `text` wrapped to `max_w` pixels, at most `max_lines` lines; the last line
/// ends in "..." when text was cut (ASCII: the subset may lack "…").
/// Returns the lines drawn.
pub fn draw_wrapped<D: DrawTarget<Color = Rgb565>>(
    target: &mut D,
    text: &str,
    origin: Point,
    max_w: u16,
    max_lines: usize,
    color: Rgb565,
) -> usize {
    let lines = wrap_px(text, max_w, unifont_width);
    let shown = lines.len().min(max_lines);
    for (i, line) in lines.iter().take(shown).enumerate() {
        let y = origin.y + i as i32 * LINE_H;
        if i + 1 == shown && lines.len() > shown {
            // Make room for the ellipsis inside the same width.
            let mut cut = alloc::string::String::from(*line);
            while width(&cut) + 24 > i32::from(max_w) && cut.pop().is_some() {}
            cut.push_str("...");
            draw(target, &cut, origin.x, y, color);
        } else {
            draw(target, line, origin.x, y, color);
        }
    }
    shown
}
