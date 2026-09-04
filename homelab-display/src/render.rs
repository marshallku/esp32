//! Screen layout for the 400x300 monochrome panel.
//!
//! The panel is 1 bit deep, so nothing can be distinguished by colour: state
//! is carried by inversion (a filled bar, a filled header) and by position.
//! Rows are fixed-height and the section boundaries are constants, so a
//! healthy screen and a broken one put the same information in the same place
//! — the whole point of a display you glance at rather than read.
//!
//! Everything is laid out against these constants rather than measured, since
//! `embedded-graphics`' built-in fonts are fixed-width.

use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{
        MonoFont, MonoTextStyle,
        ascii::{FONT_6X10, FONT_6X13_BOLD, FONT_8X13_BOLD, FONT_10X20},
    },
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use heapless::String;

use crate::model::Status;

const W: i32 = crate::st7305::WIDTH as i32;
const H: i32 = crate::st7305::HEIGHT as i32;
const PAD: i32 = 4;

// Section boundaries, top-down. Fixed so the eye learns where to look.
const HEADER_H: i32 = 30;
const GROUPS_Y: i32 = HEADER_H + 6;
const GROUP_ROW_H: i32 = 18;
const GROUP_ROWS: i32 = 5;
const GROUPS_END: i32 = GROUPS_Y + GROUP_ROW_H * GROUP_ROWS;
const ISSUES_Y: i32 = GROUPS_END + 6;
const ISSUE_ROW_H: i32 = 13;
const ISSUES_END: i32 = ISSUES_Y + 14 + ISSUE_ROW_H * 6;
const HOSTS_Y: i32 = ISSUES_END + 5;
const HOST_ROW_H: i32 = 12;
const FOOTER_Y: i32 = H - 12;

/// Environment reading from the on-board SHTC3, when it answered.
#[derive(Clone, Copy)]
pub struct Room {
    pub temp_c: f32,
    pub hum_pct: f32,
}

type Style = MonoTextStyle<'static, BinaryColor>;

fn style(font: &'static MonoFont<'static>, ink: bool) -> Style {
    MonoTextStyle::new(
        font,
        if ink {
            BinaryColor::On
        } else {
            BinaryColor::Off
        },
    )
}

fn text<D>(target: &mut D, s: &str, x: i32, y: i32, style: Style)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let _ = Text::with_baseline(s, Point::new(x, y), style, Baseline::Top).draw(target);
}

/// Right-align `s` so that it ends at `right`.
fn text_right<D>(target: &mut D, s: &str, right: i32, y: i32, style: Style)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let width = s.len() as i32 * style.font.character_size.width as i32;
    text(target, s, right - width, y, style);
}

fn hline<D>(target: &mut D, y: i32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let _ = Line::new(Point::new(PAD, y), Point::new(W - PAD, y))
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(target);
}

fn fill<D>(target: &mut D, x: i32, y: i32, w: i32, h: i32, ink: bool)
where
    D: DrawTarget<Color = BinaryColor>,
{
    if w <= 0 || h <= 0 {
        return;
    }
    let colour = if ink {
        BinaryColor::On
    } else {
        BinaryColor::Off
    };
    let _ = Rectangle::new(Point::new(x, y), Size::new(w as u32, h as u32))
        .into_styled(PrimitiveStyle::with_fill(colour))
        .draw(target);
}

fn outline<D>(target: &mut D, x: i32, y: i32, w: i32, h: i32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let _ = Rectangle::new(Point::new(x, y), Size::new(w as u32, h as u32))
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(target);
}

/// Compact duration: `19d`, `6h02m`, `41m`, `58s`.
fn duration(secs: u32) -> String<10> {
    let mut out = String::new();
    if secs >= 86_400 {
        let _ = write!(out, "{}d{:02}h", secs / 86_400, (secs % 86_400) / 3_600);
    } else if secs >= 3_600 {
        let _ = write!(out, "{}h{:02}m", secs / 3_600, (secs % 3_600) / 60);
    } else if secs >= 60 {
        let _ = write!(out, "{}m", secs / 60);
    } else {
        let _ = write!(out, "{}s", secs);
    }
    out
}

/// Draw the whole screen. Callers flush afterwards.
pub fn draw<D>(target: &mut D, status: &Status, room: Option<Room>)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let _ = target.clear(BinaryColor::Off);

    header(target, status, room);
    groups(target, status);
    issues(target, status);
    hosts(target, status);
    footer(target, status);
}

fn header<D>(target: &mut D, status: &Status, room: Option<Room>)
where
    D: DrawTarget<Color = BinaryColor>,
{
    // A stale document inverts the header outright. It is the one failure the
    // screen cannot afford to render quietly: every number below it is a lie
    // of unknown age, and an inverted bar is visible from across the room.
    let alarm = status.stale || !status.ready;
    if alarm {
        fill(target, 0, 0, W, HEADER_H, true);
    }
    let big = style(&FONT_10X20, !alarm);
    let small = style(&FONT_6X10, !alarm);

    text(target, if alarm { "STALE" } else { "HOMELAB" }, PAD, 5, big);

    let mut summary: String<24> = String::new();
    if status.kuma.ok {
        let _ = write!(summary, "{}/{} UP", status.kuma.up, status.kuma.total);
    } else {
        let _ = write!(summary, "KUMA ERR");
    }
    // Centred-ish: the summary is the number the screen exists to show.
    text(target, &summary, W / 2 - 40, 5, big);

    if let Some(room) = room {
        let mut env: String<20> = String::new();
        let _ = write!(env, "{:.1}C {:.0}%", room.temp_c, room.hum_pct);
        text_right(target, &env, W - PAD, 9, small);
    }

    if !alarm {
        hline(target, HEADER_H);
    }
}

fn groups<D>(target: &mut D, status: &Status)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let ink = style(&FONT_6X10, true);
    let col_w = (W - PAD * 2) / 2;

    for (i, group) in status.kuma.groups.iter().enumerate() {
        let column = i as i32 / GROUP_ROWS;
        let row = i as i32 % GROUP_ROWS;
        let x = PAD + column * col_w;
        let y = GROUPS_Y + row * GROUP_ROW_H;

        text(target, &group.label, x, y, ink);

        let mut count: String<12> = String::new();
        let _ = write!(count, "{}/{}", group.up, group.total);
        text_right(target, &count, x + col_w - PAD * 2, y, ink);

        // Bar under the label. A full bar is solid; a degraded one is
        // partially filled, so "not full" reads at a glance without counting.
        let bar_w = col_w - PAD * 3 - 34;
        let bar_y = y + 11;
        outline(target, x, bar_y, bar_w, 4);
        if group.total > 0 {
            let filled = bar_w * group.up as i32 / group.total as i32;
            fill(target, x, bar_y, filled, 4, true);
        }
    }

    hline(target, GROUPS_END + 1);
}

fn issues<D>(target: &mut D, status: &Status)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let ink = style(&FONT_6X10, true);
    let bold = style(&FONT_6X13_BOLD, true);

    if !status.kuma.ok {
        text(target, "uptime kuma unreachable", PAD, ISSUES_Y, bold);
        hline(target, ISSUES_END);
        return;
    }

    if status.kuma.down.is_empty() {
        text(target, "ALL SERVICES UP", PAD, ISSUES_Y, style(&FONT_8X13_BOLD, true));
        hline(target, ISSUES_END);
        return;
    }

    let mut title: String<24> = String::new();
    let _ = write!(title, "DOWN ({})", status.kuma.down.len() as u16 + status.kuma.down_more);
    text(target, &title, PAD, ISSUES_Y, bold);

    for (i, down) in status.kuma.down.iter().enumerate() {
        let y = ISSUES_Y + 14 + i as i32 * ISSUE_ROW_H;
        text(target, &down.name, PAD + 6, y, ink);
        let elapsed = match down.secs {
            Some(secs) => duration(secs),
            None => {
                let mut unknown: String<10> = String::new();
                let _ = write!(unknown, "?");
                unknown
            }
        };
        text_right(target, &elapsed, W - PAD, y, ink);
    }

    if status.kuma.down_more > 0 {
        let mut more: String<20> = String::new();
        let _ = write!(more, "+{} more", status.kuma.down_more);
        text_right(target, &more, W - PAD, ISSUES_Y, ink);
    }

    hline(target, ISSUES_END);
}

fn hosts<D>(target: &mut D, status: &Status)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let ink = style(&FONT_6X10, true);

    if !status.hosts.ok {
        // Prometheus lives in the cluster, so this is the expected reading
        // during the outage the rest of the screen is reporting. Say which
        // half is missing instead of drawing zeros.
        text(target, "host metrics unavailable (prometheus)", PAD, HOSTS_Y, ink);
        return;
    }

    text(target, "HOST", PAD, HOSTS_Y, ink);
    text(target, "CPU", 120, HOSTS_Y, ink);
    text(target, "MEM", 180, HOSTS_Y, ink);
    text(target, "DISK", 240, HOSTS_Y, ink);
    text(target, "LOAD", 300, HOSTS_Y, ink);
    text(target, "UP", 356, HOSTS_Y, ink);

    for (i, node) in status.hosts.nodes.iter().enumerate() {
        let y = HOSTS_Y + (i as i32 + 1) * HOST_ROW_H;
        if y + HOST_ROW_H > FOOTER_Y {
            break;
        }
        text(target, &node.name, PAD, y, ink);
        percent(target, node.cpu, 120, y, ink);
        percent(target, node.mem, 180, y, ink);
        percent(target, node.disk, 240, y, ink);

        let mut load: String<10> = String::new();
        match node.load {
            Some(value) => {
                let _ = write!(load, "{:.1}", value);
            }
            None => {
                let _ = write!(load, "-");
            }
        }
        text(target, &load, 300, y, ink);

        let mut uptime: String<10> = String::new();
        match node.up_d {
            Some(days) => {
                let _ = write!(uptime, "{:.0}d", days);
            }
            None => {
                let _ = write!(uptime, "-");
            }
        }
        text(target, &uptime, 356, y, ink);
    }
}

fn percent<D>(target: &mut D, value: Option<f32>, x: i32, y: i32, style: Style)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let mut out: String<10> = String::new();
    match value {
        Some(value) => {
            let _ = write!(out, "{:.0}%", value);
        }
        None => {
            let _ = write!(out, "-");
        }
    }
    text(target, &out, x, y, style);
}

fn footer<D>(target: &mut D, status: &Status)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let ink = style(&FONT_6X10, true);
    hline(target, FOOTER_Y - 3);

    // Monitors excluded from the headline ratio are reported here rather than
    // dropped, so "54/56" is never quietly hiding two more.
    if status.kuma.unsettled > 0 {
        let mut pending: String<24> = String::new();
        let _ = write!(pending, "{} pending/maint", status.kuma.unsettled);
        text(target, &pending, PAD, FOOTER_Y, ink);
    }

    // A wall-clock stamp rather than "21s ago": the age is something this
    // board asserts about itself, so firmware that wedges after a good draw
    // would keep claiming the data is fresh. The stamp is a value the board
    // only echoes, so a frozen screen simply stops agreeing with the clock on
    // the wall.
    let mut updated: String<32> = String::new();
    let _ = write!(updated, "updated {}", status.generated_at);
    text_right(target, &updated, W - PAD, FOOTER_Y, ink);
}

/// A standalone message, used before the first successful fetch and after a
/// fetch failure. Callers flush afterwards.
pub fn message<D>(target: &mut D, title: &str, detail: &str)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let _ = target.clear(BinaryColor::Off);
    text(target, title, PAD, H / 2 - 30, style(&FONT_10X20, true));
    text(target, detail, PAD, H / 2, style(&FONT_6X10, true));
}
