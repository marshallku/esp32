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

const BG: Rgb565 = Rgb565::BLACK;
const DIM: Rgb565 = Rgb565::new(12, 24, 12);
const ACCENT: Rgb565 = Rgb565::new(31, 45, 0);
const WARN: Rgb565 = Rgb565::new(31, 20, 0);

pub fn draw<D: DrawTarget<Color = Rgb565>>(target: &mut D, app: &App) {
    let _ = target.clear(BG);
    match app.view() {
        View::Prompt(prompt) => draw_prompt(target, prompt),
        View::Dashboard => {
            strip(target, app, "HOMELAB");
            center(target, "dashboard", 240, Rgb565::WHITE);
            center(target, "(arrives in the next firmware)", 270, DIM);
        }
        View::Deck { keys, .. } => {
            strip(target, app, "DECK");
            for (i, key) in keys.iter().enumerate() {
                draw_key(target, geometry::key_rect(i), key);
            }
        }
        View::NoDeck { link_up } => {
            strip(target, app, "DECK");
            let (line, color) =
                if link_up { ("waiting for a layout from deskd", DIM) } else { ("PC not connected", WARN) };
            center(target, line, 250, color);
        }
    }
}

/// Title on the left, page dots in the middle, link state and clock on the
/// right. The dots count the dashboard as the first screen, matching the
/// swipe order.
fn strip<D: DrawTarget<Color = Rgb565>>(target: &mut D, app: &App, title: &str) {
    let style = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    let _ = Text::with_baseline(title, Point::new(12, 11), style, Baseline::Top).draw(target);

    let screens = app.page_count() + 1;
    let current = app.screen().map_or(0, |p| p as usize + 1);
    let x0 = W as i32 / 2 - (screens as i32 * 16) / 2;
    for i in 0..screens {
        let style = if i == current { PrimitiveStyle::with_fill(Rgb565::WHITE) } else { PrimitiveStyle::with_stroke(DIM, 2) };
        let _ = Circle::new(Point::new(x0 + i as i32 * 16, STRIP_H as i32 / 2 - 4), 9).into_styled(style).draw(target);
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

    let cx = r.x as i32 + r.w as i32 / 2;
    let lines = text::wrap(&key.label, LABEL_COLS);
    let lines = &lines[..lines.len().min(2)];
    let block = lines.len() as i32 * GLYPH_H as i32 + if key.sub.is_empty() { 0 } else { 14 };
    let mut y = r.y as i32 + (r.h as i32 - block) / 2;
    let centered = TextStyleBuilder::new().alignment(Alignment::Center).baseline(Baseline::Top).build();
    for line in lines {
        let _ = Text::with_text_style(line, Point::new(cx, y), MonoTextStyle::new(&FONT_10X20, fg), centered).draw(target);
        y += GLYPH_H as i32;
    }
    if !key.sub.is_empty() {
        let sub = text::wrap(&key.sub, SUB_MAX);
        let _ = Text::with_text_style(sub[0], Point::new(cx, y + 4), MonoTextStyle::new(&FONT_6X10, fg), centered)
            .draw(target);
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
