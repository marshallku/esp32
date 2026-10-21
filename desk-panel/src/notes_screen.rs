//! The notifications screen and the feedback a new one gets on every other
//! screen: a banner over the top and a blinking border.
//!
//! Rows are newest first. Swipe up and down to scroll, long-press a row to
//! remove it, tap Clear to remove all (`desk_core::app` holds those rules).

use alloc::collections::VecDeque;

use desk_core::{
    app::{App, Overlay},
    geometry::{BANNER, CLEAR_BUTTON, H, NOTE_ROW_H, NOTE_ROWS, NOTES_TOP, Rect, W},
    protocol::{Note, URGENCY_CRITICAL},
};
use embedded_graphics::{
    mono_font::{
        MonoTextStyle,
        ascii::{FONT_6X10, FONT_10X20},
    },
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle, RoundedRectangle},
    text::{Baseline, Text},
};

use crate::{ui, unifont};

const PAD: i32 = 10;
const DIM: Rgb565 = Rgb565::new(12, 24, 12);
const ROW_BG: Rgb565 = Rgb565::new(3, 6, 4);
/// Catppuccin mauve and red, the frame colours of the user's dunst config, so
/// the panel's alert reads as the same notification.
const NORMAL: Rgb565 = Rgb565::new(25, 40, 30);
const CRITICAL: Rgb565 = Rgb565::new(30, 34, 21);
const BANNER_BG: Rgb565 = Rgb565::new(4, 8, 7);
const BORDER: u32 = 6;

pub fn draw<D: DrawTarget<Color = Rgb565>>(target: &mut D, app: &App, notes: &VecDeque<Note>, scroll: usize) {
    ui::strip(target, app, "NOTES", false);
    if notes.is_empty() {
        let style = MonoTextStyle::new(&FONT_10X20, DIM);
        let _ = Text::with_baseline("no notifications", Point::new(150, 220), style, Baseline::Top).draw(target);
        return;
    }
    for (i, note) in notes.iter().skip(scroll).take(NOTE_ROWS).enumerate() {
        let y = i32::from(NOTES_TOP) + i as i32 * i32::from(NOTE_ROW_H);
        row(target, note, y);
    }

    // Where in the list the window is, when there is more than one screenful.
    if notes.len() > NOTE_ROWS {
        let track_h = i32::from(NOTE_ROW_H) * NOTE_ROWS as i32;
        let thumb_h = track_h * NOTE_ROWS as i32 / notes.len() as i32;
        let thumb_y = i32::from(NOTES_TOP) + track_h * scroll as i32 / notes.len() as i32;
        fill(target, W as i32 - 4, thumb_y, 3, thumb_h, DIM);
    }

    let c = CLEAR_BUTTON;
    let rect = Rectangle::new(Point::new(c.x.into(), c.y.into()), Size::new(c.w.into(), c.h.into()));
    let _ = RoundedRectangle::with_equal_corners(rect, Size::new(10, 10))
        .into_styled(PrimitiveStyle::with_fill(DIM))
        .draw(target);
    let style = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    let _ = Text::with_baseline("Clear", Point::new(i32::from(c.x) + 35, i32::from(c.y) + 12), style, Baseline::Top)
        .draw(target);
    let hint = MonoTextStyle::new(&FONT_6X10, DIM);
    let _ = Text::with_baseline("hold a row to remove it", Point::new(PAD, H as i32 - 34), hint, Baseline::Top)
        .draw(target);
}

fn row<D: DrawTarget<Color = Rgb565>>(target: &mut D, note: &Note, y: i32) {
    let h = i32::from(NOTE_ROW_H) - 4;
    fill(target, 4, y, W as i32 - 12, h, ROW_BG);
    fill(target, 4, y, 4, h, urgency_color(note));
    if note.unread {
        fill(target, W as i32 - 22, y + 8, 8, 8, NORMAL);
    }
    let x = PAD + 6;
    let time = MonoTextStyle::new(&FONT_6X10, DIM);
    let _ = Text::with_baseline(&note.time, Point::new(W as i32 - 70, y + 6), time, Baseline::Top).draw(target);
    unifont::draw(target, &note.app, x, y + 4, DIM);
    unifont::draw_wrapped(target, &note.summary, Point::new(x, y + 22), (W as i32 - x - 30) as u16, 1, Rgb565::WHITE);
    if !note.body.is_empty() {
        unifont::draw_wrapped(target, &note.body, Point::new(x, y + 40), (W as i32 - x - 30) as u16, 2, DIM);
    }
}

/// Banner and border over whatever was just drawn. Called last.
pub fn draw_overlay<D: DrawTarget<Color = Rgb565>>(target: &mut D, overlay: &Overlay<'_>) {
    if let Some(critical) = overlay.border {
        let color = if critical { CRITICAL } else { NORMAL };
        let _ = Rectangle::new(Point::zero(), Size::new(W.into(), H.into()))
            .into_styled(PrimitiveStyle::with_stroke(color, BORDER))
            .draw(target);
    }
    if let Some(note) = overlay.banner {
        banner(target, note, BANNER);
    }
}

fn banner<D: DrawTarget<Color = Rgb565>>(target: &mut D, note: &Note, r: Rect) {
    let rect = Rectangle::new(Point::new(r.x.into(), r.y.into()), Size::new(r.w.into(), r.h.into()));
    let accent = urgency_color(note);
    let _ = RoundedRectangle::with_equal_corners(rect, Size::new(12, 12))
        .into_styled(PrimitiveStyle::with_fill(BANNER_BG))
        .draw(target);
    let _ = RoundedRectangle::with_equal_corners(rect, Size::new(12, 12))
        .into_styled(PrimitiveStyle::with_stroke(accent, 3))
        .draw(target);
    let x = i32::from(r.x) + 14;
    let w = (r.w - 28) as u16;
    unifont::draw(target, &note.app, x, i32::from(r.y) + 8, accent);
    unifont::draw_wrapped(target, &note.summary, Point::new(x, i32::from(r.y) + 28), w, 1, Rgb565::WHITE);
    if !note.body.is_empty() {
        unifont::draw_wrapped(target, &note.body, Point::new(x, i32::from(r.y) + 48), w, 2, DIM);
    }
}

fn urgency_color(note: &Note) -> Rgb565 {
    if note.urgency >= URGENCY_CRITICAL { CRITICAL } else { NORMAL }
}

fn fill<D: DrawTarget<Color = Rgb565>>(target: &mut D, x: i32, y: i32, w: i32, h: i32, color: Rgb565) {
    if w > 0 && h > 0 {
        let _ = Rectangle::new(Point::new(x, y), Size::new(w as u32, h as u32))
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(target);
    }
}
