//! The homelab status page: the same document homelab-display shows on its
//! monochrome shelf panel, laid out for 480×480 colour.
//!
//! The semantics are carried over unchanged — they are the point of that
//! display and are documented there: a stale or not-ready document turns the
//! header into an alarm, the headline counts only settled monitors and the
//! unsettled ones are printed rather than dropped, and the footer echoes the
//! server's wall-clock stamp instead of claiming an age of its own.
//! Colour only adds a second channel on top: red for down, amber for degraded.

use core::fmt::Write as _;

use desk_core::app::App;
use embedded_graphics::{
    mono_font::{
        MonoTextStyle,
        ascii::{FONT_6X10, FONT_8X13, FONT_8X13_BOLD, FONT_10X20},
    },
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Alignment, Baseline, Text, TextStyleBuilder},
};
use heapless::String;
use homelab_status::{model::Status, net::FetchError};

use crate::ui;

const W: i32 = 480;
const PAD: i32 = 10;

const OK: Rgb565 = Rgb565::new(6, 44, 12);
const WARN: Rgb565 = Rgb565::new(31, 40, 0);
const BAD: Rgb565 = Rgb565::new(28, 8, 6);
const DIM: Rgb565 = Rgb565::new(12, 24, 12);
const TRACK: Rgb565 = Rgb565::new(5, 10, 5);

// Section tops, fixed so a healthy screen and a broken one put the same
// information in the same place.
const HEADLINE_Y: i32 = 52;
const GROUPS_Y: i32 = 86;
const GROUP_ROW_H: i32 = 26;
const GROUP_ROWS: i32 = 5;
const ISSUES_Y: i32 = GROUPS_Y + GROUP_ROW_H * GROUP_ROWS + 8;
const ISSUE_ROW_H: i32 = 16;
const HOSTS_Y: i32 = ISSUES_Y + 18 + ISSUE_ROW_H * 6 + 8;
const HOST_ROW_H: i32 = 14;
const FOOTER_Y: i32 = 466;

/// Consecutive failed fetches before the last good screen is replaced by an
/// error. One failure is left alone: the screen still carries the stamp of
/// when it was true.
const FAILURES_SHOWN_AFTER: u32 = 3;

/// What the dashboard knows, fed by the fetch task.
#[derive(Default)]
pub struct Dash {
    pub net: Net,
    pub status: Option<Status>,
    pub failures: u32,
    pub last_error: Option<FetchError>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Net {
    #[default]
    Connecting,
    /// Associated (or trying) but no DHCP lease after the report threshold.
    NoLease,
    Online,
}

pub enum Update {
    Net(Net),
    Fetched(alloc::boxed::Box<Status>),
    Failed(FetchError),
}

impl Dash {
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Net(net) => self.net = net,
            Update::Fetched(status) => {
                self.status = Some(*status);
                self.failures = 0;
                self.last_error = None;
            }
            Update::Failed(e) => {
                self.failures += 1;
                self.last_error = Some(e);
            }
        }
    }
}

pub fn draw<D: DrawTarget<Color = Rgb565>>(target: &mut D, app: &App, dash: &Dash) {
    let status = dash.status.as_ref().filter(|_| dash.failures < FAILURES_SHOWN_AFTER);
    let Some(status) = status else {
        ui::strip(target, app, "HOMELAB", false);
        let (title, detail) = match (dash.net, dash.last_error) {
            (_, Some(e)) if dash.failures >= FAILURES_SHOWN_AFTER => ("NO DATA", e.detail()),
            (Net::NoLease, _) => ("NO NETWORK", "waiting for WiFi and DHCP..."),
            (Net::Connecting, _) => ("HOMELAB", "connecting to WiFi..."),
            (Net::Online, _) => ("HOMELAB", "fetching status..."),
        };
        center(target, title, 210, MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE));
        center(target, detail, 242, MonoTextStyle::new(&FONT_8X13, DIM));
        return;
    };

    // A stale document makes every number below it a lie of unknown age, so
    // the header turns into an alarm visible from across the room.
    let alarm = status.stale || !status.ready;
    ui::strip(target, app, if alarm { "STALE" } else { "HOMELAB" }, alarm);

    headline(target, status);
    groups(target, status);
    issues(target, status);
    hosts(target, status);
    footer(target, status);
}

fn headline<D: DrawTarget<Color = Rgb565>>(target: &mut D, status: &Status) {
    let mut s: String<24> = String::new();
    let color = if !status.kuma.ok {
        let _ = write!(s, "KUMA ERR");
        BAD
    } else {
        let _ = write!(s, "{}/{} UP", status.kuma.up, status.kuma.total);
        if status.kuma.down.is_empty() { OK } else { BAD }
    };
    text(target, &s, PAD, HEADLINE_Y, MonoTextStyle::new(&FONT_10X20, color));
}

fn groups<D: DrawTarget<Color = Rgb565>>(target: &mut D, status: &Status) {
    let label = MonoTextStyle::new(&FONT_8X13_BOLD, Rgb565::WHITE);
    let col_w = (W - PAD * 2) / 2;

    for (i, group) in status.kuma.groups.iter().enumerate() {
        let x = PAD + (i as i32 / GROUP_ROWS) * col_w;
        let y = GROUPS_Y + (i as i32 % GROUP_ROWS) * GROUP_ROW_H;
        let color = match (group.up, group.total) {
            (_, 0) => DIM,
            (up, total) if up == total => OK,
            (0, _) => BAD,
            _ => WARN,
        };

        text(target, &group.label, x, y, label);
        let mut count: String<12> = String::new();
        let _ = write!(count, "{}/{}", group.up, group.total);
        text_right(target, &count, x + col_w - PAD * 2, y, MonoTextStyle::new(&FONT_8X13, color));

        // A full bar is solid; a degraded one is partly filled, so "not full"
        // reads at a glance without counting.
        let bar_w = col_w - PAD * 2;
        fill(target, x, y + 15, bar_w, 5, TRACK);
        if group.total > 0 {
            fill(target, x, y + 15, bar_w * group.up as i32 / group.total as i32, 5, color);
        }
    }
}

fn issues<D: DrawTarget<Color = Rgb565>>(target: &mut D, status: &Status) {
    let bold = |c| MonoTextStyle::new(&FONT_8X13_BOLD, c);
    let row = MonoTextStyle::new(&FONT_8X13, Rgb565::WHITE);

    if !status.kuma.ok {
        text(target, "uptime kuma unreachable", PAD, ISSUES_Y, bold(BAD));
        return;
    }
    if status.kuma.down.is_empty() {
        text(target, "ALL SERVICES UP", PAD, ISSUES_Y, bold(OK));
        return;
    }

    let mut title: String<24> = String::new();
    let _ = write!(title, "DOWN ({})", status.kuma.down.len() as u16 + status.kuma.down_more);
    text(target, &title, PAD, ISSUES_Y, bold(BAD));
    if status.kuma.down_more > 0 {
        let mut more: String<20> = String::new();
        let _ = write!(more, "+{} more", status.kuma.down_more);
        text_right(target, &more, W - PAD, ISSUES_Y, row);
    }

    for (i, down) in status.kuma.down.iter().enumerate() {
        let y = ISSUES_Y + 18 + i as i32 * ISSUE_ROW_H;
        fill(target, PAD, y + 2, 4, 9, BAD);
        text(target, &down.name, PAD + 10, y, row);
        let elapsed = down.secs.map_or_else(|| String::try_from("?").unwrap_or_default(), duration);
        text_right(target, &elapsed, W - PAD, y, row);
    }
}

fn hosts<D: DrawTarget<Color = Rgb565>>(target: &mut D, status: &Status) {
    let head = MonoTextStyle::new(&FONT_6X10, DIM);
    let row = MonoTextStyle::new(&FONT_8X13, Rgb565::WHITE);

    if !status.hosts.ok {
        // Prometheus lives in the cluster, so this is the expected reading
        // during the outage the rest of the screen is reporting. Say which
        // half is missing instead of drawing zeros.
        text(target, "host metrics unavailable (prometheus)", PAD, HOSTS_Y, MonoTextStyle::new(&FONT_8X13, DIM));
        return;
    }

    const COLS: [(&str, i32); 6] = [("HOST", PAD), ("CPU", 150), ("MEM", 215), ("DISK", 280), ("LOAD", 350), ("UP", 420)];
    for (name, x) in COLS {
        text(target, name, x, HOSTS_Y, head);
    }
    for (i, node) in status.hosts.nodes.iter().enumerate() {
        let y = HOSTS_Y + 12 + i as i32 * HOST_ROW_H;
        if y + HOST_ROW_H > FOOTER_Y - 4 {
            break;
        }
        text(target, &node.name, COLS[0].1, y, row);
        for (value, x) in [(node.cpu, COLS[1].1), (node.mem, COLS[2].1), (node.disk, COLS[3].1)] {
            let color = match value {
                Some(v) if v >= 90.0 => BAD,
                Some(v) if v >= 75.0 => WARN,
                _ => Rgb565::WHITE,
            };
            text(target, &opt(value, |o, v| write!(o, "{:.0}%", v)), x, y, MonoTextStyle::new(&FONT_8X13, color));
        }
        text(target, &opt(node.load, |o, v| write!(o, "{:.1}", v)), COLS[4].1, y, row);
        text(target, &opt(node.up_d, |o, v| write!(o, "{:.0}d", v)), COLS[5].1, y, row);
    }
}

fn footer<D: DrawTarget<Color = Rgb565>>(target: &mut D, status: &Status) {
    let style = MonoTextStyle::new(&FONT_6X10, DIM);
    // Excluded from the headline ratio, so reported here rather than dropped:
    // "54/56" never quietly hides two more.
    if status.kuma.unsettled > 0 {
        let mut pending: String<24> = String::new();
        let _ = write!(pending, "{} pending/maint", status.kuma.unsettled);
        text(target, &pending, PAD, FOOTER_Y, MonoTextStyle::new(&FONT_6X10, WARN));
    }
    let mut updated: String<32> = String::new();
    let _ = write!(updated, "updated {}", status.generated_at);
    text_right(target, &updated, W - PAD, FOOTER_Y, style);
}

/// `value` written by `f`, or "-" when the series is missing — a blank rather
/// than a confident zero.
fn opt(value: Option<f32>, f: impl Fn(&mut String<10>, f32) -> core::fmt::Result) -> String<10> {
    let mut out = String::new();
    let _ = match value {
        Some(v) => f(&mut out, v),
        None => out.push('-').map_err(|_| core::fmt::Error),
    };
    out
}

/// Compact duration: `19d04h`, `6h02m`, `41m`, `58s`.
fn duration(secs: u32) -> String<10> {
    let mut out = String::new();
    let _ = match secs {
        86_400.. => write!(out, "{}d{:02}h", secs / 86_400, (secs % 86_400) / 3_600),
        3_600.. => write!(out, "{}h{:02}m", secs / 3_600, (secs % 3_600) / 60),
        60.. => write!(out, "{}m", secs / 60),
        _ => write!(out, "{}s", secs),
    };
    out
}

fn text<D: DrawTarget<Color = Rgb565>>(target: &mut D, s: &str, x: i32, y: i32, style: MonoTextStyle<'_, Rgb565>) {
    let _ = Text::with_baseline(s, Point::new(x, y), style, Baseline::Top).draw(target);
}

fn text_right<D: DrawTarget<Color = Rgb565>>(target: &mut D, s: &str, right: i32, y: i32, style: MonoTextStyle<'_, Rgb565>) {
    let w = s.len() as i32 * style.font.character_size.width as i32;
    text(target, s, right - w, y, style);
}

fn center<D: DrawTarget<Color = Rgb565>>(target: &mut D, s: &str, y: i32, style: MonoTextStyle<'_, Rgb565>) {
    let centered = TextStyleBuilder::new().alignment(Alignment::Center).baseline(Baseline::Top).build();
    let _ = Text::with_text_style(s, Point::new(W / 2, y), style, centered).draw(target);
}

fn fill<D: DrawTarget<Color = Rgb565>>(target: &mut D, x: i32, y: i32, w: i32, h: i32, color: Rgb565) {
    if w > 0 && h > 0 {
        let _ = Rectangle::new(Point::new(x, y), Size::new(w as u32, h as u32))
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(target);
    }
}
