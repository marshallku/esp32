//! Draws `desk_core::app::View` onto a frame.
//!
//! Every screen is redrawn whole: the app marks itself dirty on any visible
//! change, and at ~10 full presents a second a whole-screen redraw is cheaper
//! to reason about than tracking damage.

use desk_core::{
    app::{App, Prompt, View},
    geometry::{self, Rect, STRIP_H, W},
    protocol::Key,
    text::{self, BODY_COLS, BODY_ROWS, GLYPH_H, LABEL_COLS, SUB_MAX},
};
use embedded_graphics::{
    mono_font::{
        MonoTextStyle,
        ascii::{FONT_6X10, FONT_10X20},
    },
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, PrimitiveStyle, Rectangle, RoundedRectangle},
    text::{Alignment, Baseline, Text, TextStyleBuilder},
};

use crate::{
    dashboard::{self, Dash},
    notes_screen, pcstats, unifont,
};

const BG: Rgb565 = Rgb565::BLACK;
const DIM: Rgb565 = Rgb565::new(12, 24, 12);
const ACCENT: Rgb565 = Rgb565::new(31, 45, 0);
const WARN: Rgb565 = Rgb565::new(31, 20, 0);
const ALARM: Rgb565 = Rgb565::new(24, 4, 4);
/// dunst's mauve, as on the notifications screen.
const UNREAD: Rgb565 = Rgb565::new(25, 40, 30);

pub fn draw<D: DrawTarget<Color = Rgb565>>(target: &mut D, app: &App, dash: &Dash) {
    let _ = target.clear(BG);
    match app.view() {
        View::Prompt(prompt) => draw_prompt(target, prompt),
        View::Dashboard => dashboard::draw(target, app, dash),
        View::Stats { stats, stale, history } => pcstats::draw(target, app, stats, stale, history),
        View::Deck { keys, .. } => {
            strip(target, app, "DECK", false);
            for (i, key) in keys.iter().enumerate() {
                draw_key(target, geometry::key_rect(i), key);
            }
        }
        View::NoDeck { link_up } => {
            strip(target, app, "DECK", false);
            let (line, color) =
                if link_up { ("waiting for a layout from deskd", DIM) } else { ("PC not connected", WARN) };
            center(target, line, 250, color);
        }
        View::Notes { notes, scroll } => notes_screen::draw(target, app, notes, scroll),
    }
    // Last, over everything; `overlay` is empty while a prompt is up.
    notes_screen::draw_overlay(target, &app.overlay());
}

/// Title on the left, page dots in the middle, link state and clock on the
/// right. One dot per screen, in swipe order. `alarm` paints the whole strip as a warning.
pub fn strip<D: DrawTarget<Color = Rgb565>>(target: &mut D, app: &App, title: &str, alarm: bool) {
    if alarm {
        let _ = Rectangle::new(Point::zero(), Size::new(W as u32, STRIP_H as u32))
            .into_styled(PrimitiveStyle::with_fill(ALARM))
            .draw(target);
    }
    let style = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    let _ = Text::with_baseline(title, Point::new(12, 11), style, Baseline::Top).draw(target);

    let all = app.screens();
    let screens = all.len();
    let current = all.iter().position(|s| *s == app.screen()).unwrap_or(0);
    let x0 = W as i32 / 2 - (screens as i32 * 16) / 2;
    for i in 0..screens {
        let style = if i == current { PrimitiveStyle::with_fill(Rgb565::WHITE) } else { PrimitiveStyle::with_stroke(DIM, 2) };
        let _ = Circle::new(Point::new(x0 + i as i32 * 16, STRIP_H as i32 / 2 - 4), 9).into_styled(style).draw(target);
    }

    let unread = app.unread();
    if unread > 0 {
        let _ = Circle::new(Point::new(W as i32 - 118, 10), 22).into_styled(PrimitiveStyle::with_fill(UNREAD)).draw(target);
        let mut count: heapless::String<4> = heapless::String::new();
        let _ = core::fmt::Write::write_fmt(&mut count, format_args!("{}", unread.min(99)));
        let x = W as i32 - 107 - count.len() as i32 * 3;
        let _ = Text::with_baseline(&count, Point::new(x, 16), MonoTextStyle::new(&FONT_6X10, Rgb565::BLACK), Baseline::Top)
            .draw(target);
    }

    let link = if app.link_up() { ACCENT } else { WARN };
    let _ = Circle::new(Point::new(W as i32 - 82, 16), 10).into_styled(PrimitiveStyle::with_fill(link)).draw(target);
    let _ = Text::with_baseline(app.time(), Point::new(W as i32 - 62, 11), style, Baseline::Top).draw(target);
}

fn draw_key<D: DrawTarget<Color = Rgb565>>(target: &mut D, r: Rect, key: &Key) {
    let (bg, fg) = if key.on { (rgb(key.fg), rgb(key.bg)) } else { (rgb(key.bg), rgb(key.fg)) };
    let rect = Rectangle::new(Point::new(r.x as i32, r.y as i32), Size::new(r.w as u32, r.h as u32));
    let _ = RoundedRectangle::with_equal_corners(rect, Size::new(12, 12))
        .into_styled(PrimitiveStyle::with_fill(bg))
        .draw(target);
    if key.on {
        let bar = Rectangle::new(Point::new(r.x as i32 + 10, r.y as i32 + r.h as i32 - 12), Size::new(r.w as u32 - 20, 5));
        let _ = bar.into_styled(PrimitiveStyle::with_fill(ACCENT)).draw(target);
    }

    // Each field picks its own font — the 10×20 / 6×10 mono fonts for ASCII,
    // Unifont otherwise — exactly as `text::label_fits` and `sub_fits`
    // validated it on the host.
    let cx = r.x as i32 + r.w as i32 / 2;
    let max_w = r.w - 8;
    let (lines, line_h) = if key.label.is_ascii() {
        (text::wrap(&key.label, LABEL_COLS), GLYPH_H as i32)
    } else {
        (text::wrap_px(&key.label, max_w, text::unifont_width), unifont::LINE_H + 2)
    };
    let lines = &lines[..lines.len().min(2)];
    let sub_h = match (key.sub.is_empty(), key.sub.is_ascii()) {
        (true, _) => 0,
        (false, true) => 14,
        (false, false) => unifont::LINE_H + 4,
    };
    let mut y = r.y as i32 + (r.h as i32 - lines.len() as i32 * line_h - sub_h) / 2;
    let centered = TextStyleBuilder::new().alignment(Alignment::Center).baseline(Baseline::Top).build();
    for line in lines {
        if key.label.is_ascii() {
            let _ = Text::with_text_style(line, Point::new(cx, y), MonoTextStyle::new(&FONT_10X20, fg), centered)
                .draw(target);
        } else {
            unifont::draw_centered(target, line, cx, y, fg);
        }
        y += line_h;
    }
    if key.sub.is_empty() {
        return;
    }
    if key.sub.is_ascii() {
        let sub = text::wrap(&key.sub, SUB_MAX);
        let _ = Text::with_text_style(sub[0], Point::new(cx, y + 4), MonoTextStyle::new(&FONT_6X10, fg), centered)
            .draw(target);
    } else {
        let sub = text::wrap_px(&key.sub, max_w, text::unifont_width);
        unifont::draw_centered(target, sub[0], cx, y + 4, fg);
    }
}

fn draw_prompt<D: DrawTarget<Color = Rgb565>>(target: &mut D, prompt: &Prompt) {
    let _ = Rectangle::new(Point::zero(), Size::new(W as u32, 52))
        .into_styled(PrimitiveStyle::with_fill(WARN))
        .draw(target);
    let title = MonoTextStyle::new(&FONT_10X20, Rgb565::BLACK);
    let _ = Text::with_baseline(&prompt.title, Point::new(10, 16), title, Baseline::Top).draw(target);

    // Shown exactly as `text::fits_body` measured it on the host, so what the
    // daemon judged fully visible is what appears.
    let body = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    for (i, line) in text::wrap(&prompt.body, BODY_COLS).iter().take(BODY_ROWS).enumerate() {
        let y = geometry::BODY_TOP as i32 + i as i32 * GLYPH_H as i32;
        let _ = Text::with_baseline(line, Point::new(10, y), body, Baseline::Top).draw(target);
    }

    let count = prompt.buttons.len();
    for (i, label) in prompt.buttons.iter().enumerate() {
        let r = geometry::button_rect(i, count);
        // The last button is the affirmative one by convention (Deny, Allow).
        let bg = if i + 1 == count && count > 1 { ACCENT } else { DIM };
        let key = Key { label: label.clone(), bg: from565(bg), fg: 0, ..Default::default() };
        draw_key(target, r, &key);
    }
}

fn center<D: DrawTarget<Color = Rgb565>>(target: &mut D, s: &str, y: i32, color: Rgb565) {
    let style = TextStyleBuilder::new().alignment(Alignment::Center).baseline(Baseline::Top).build();
    let _ = Text::with_text_style(s, Point::new(W as i32 / 2, y), MonoTextStyle::new(&FONT_10X20, color), style)
        .draw(target);
}

fn rgb(c: u32) -> Rgb565 {
    Rgb565::new(((c >> 19) & 0x1F) as u8, ((c >> 10) & 0x3F) as u8, ((c >> 3) & 0x1F) as u8)
}

fn from565(c: Rgb565) -> u32 {
    ((c.r() as u32) << 19) | ((c.g() as u32) << 10) | ((c.b() as u32) << 3)
}
