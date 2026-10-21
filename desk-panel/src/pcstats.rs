//! The PC stats screen: CPU, per-core load, memory, GPU and network, with a
//! two-minute history graph.
//!
//! Every reading may be missing and is drawn as "-" when it is. A sample
//! older than `STATS_STALE_MS` turns the header amber: a frozen 3 % is a lie
//! the screen must not tell quietly.

use core::fmt::Write as _;

use desk_core::{
    app::{App, HISTORY_LEN, History},
    protocol::Stats,
};
use embedded_graphics::{
    mono_font::{
        MonoTextStyle,
        ascii::{FONT_6X10, FONT_8X13, FONT_10X20},
    },
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use heapless::String;

use crate::ui;

const W: i32 = 480;
const PAD: i32 = 12;

const CPU: Rgb565 = Rgb565::new(31, 45, 0);
const MEM: Rgb565 = Rgb565::new(8, 40, 31);
const GPU: Rgb565 = Rgb565::new(20, 50, 8);
const DIM: Rgb565 = Rgb565::new(12, 24, 12);
const TRACK: Rgb565 = Rgb565::new(5, 10, 5);
const HOT: Rgb565 = Rgb565::new(28, 8, 6);

const CPU_Y: i32 = 52;
const GRAPH_Y: i32 = 150;
const GRAPH_H: i32 = 90;
const MEM_Y: i32 = 262;
const GPU_Y: i32 = 322;
const NET_Y: i32 = 412;

pub fn draw<D: DrawTarget<Color = Rgb565>>(target: &mut D, app: &App, stats: Option<&Stats>, stale: bool, history: &History) {
    ui::strip(target, app, if stale { "PC (stale)" } else { "PC" }, false);
    let Some(s) = stats else {
        text(target, "waiting for stats from deskd", PAD, 220, MonoTextStyle::new(&FONT_10X20, DIM));
        return;
    };
    let big = |c| MonoTextStyle::new(&FONT_10X20, c);
    let label = MonoTextStyle::new(&FONT_6X10, DIM);
    let value = MonoTextStyle::new(&FONT_8X13, Rgb565::WHITE);

    // CPU: total, temperature, one bar per core.
    text(target, "CPU", PAD, CPU_Y, label);
    text(target, &pct(s.cpu), PAD, CPU_Y + 14, big(level(s.cpu, CPU)));
    let mut temp: String<12> = String::new();
    match s.cpu_temp {
        Some(t) => {
            let _ = write!(temp, "{t} C");
        }
        None => {
            let _ = temp.push('-');
        }
    }
    text(target, &temp, PAD, CPU_Y + 40, MonoTextStyle::new(&FONT_8X13, temp_color(s.cpu_temp)));
    if !s.cores.is_empty() {
        let (x0, h) = (110, 80);
        let slot = (W - PAD - x0) / s.cores.len() as i32;
        for (i, &busy) in s.cores.iter().enumerate() {
            let x = x0 + i as i32 * slot;
            fill(target, x, CPU_Y, slot - 3, h, TRACK);
            let filled = h * i32::from(busy.min(100)) / 100;
            fill(target, x, CPU_Y + h - filled, slot - 3, filled, level(Some(busy), CPU));
        }
    }

    graph(target, history);

    // Memory.
    text(target, "MEM", PAD, MEM_Y, label);
    let mut mem: String<32> = String::new();
    match (s.mem_used, s.mem_total) {
        (Some(used), Some(total)) => {
            let _ = write!(mem, "{:.1} / {:.1} GB", gib(used), gib(total));
        }
        _ => {
            let _ = mem.push('-');
        }
    }
    text(target, &mem, 70, MEM_Y, value);
    bar(target, MEM_Y + 18, s.mem_used.zip(s.mem_total), MEM);

    // GPU.
    text(target, "GPU", PAD, GPU_Y, label);
    match &s.gpu {
        Some(g) => {
            let mut line: String<48> = String::new();
            let _ = write!(line, "{}  ", pct(g.util));
            match g.temp {
                Some(t) => {
                    let _ = write!(line, "{t} C");
                }
                None => {
                    let _ = line.push('-');
                }
            }
            text(target, &line, 70, GPU_Y, MonoTextStyle::new(&FONT_8X13, temp_color(g.temp)));
            bar(target, GPU_Y + 18, g.util.map(|u| (u32::from(u), 100)), GPU);
            let mut vram: String<32> = String::new();
            match (g.vram_used, g.vram_total) {
                (Some(used), Some(total)) => {
                    let _ = write!(vram, "VRAM {:.1} / {:.1} GB", gib(used), gib(total));
                }
                _ => {
                    let _ = vram.push_str("VRAM -");
                }
            }
            text(target, &vram, 70, GPU_Y + 34, value);
            bar(target, GPU_Y + 52, g.vram_used.zip(g.vram_total), GPU);
        }
        None => text(target, "-", 70, GPU_Y, value),
    }

    // Network.
    text(target, "NET", PAD, NET_Y, label);
    let mut net: String<48> = String::new();
    let _ = write!(net, "down {}   up {}", rate(s.net_rx).as_str(), rate(s.net_tx).as_str());
    text(target, &net, 70, NET_Y, value);
}

/// CPU (amber) and memory (blue) over the last `HISTORY_LEN` samples, right
/// aligned so the newest is always at the right edge. Gaps break the line.
fn graph<D: DrawTarget<Color = Rgb565>>(target: &mut D, history: &History) {
    let (x0, x1) = (PAD, W - PAD);
    let step = (x1 - x0) as f32 / (HISTORY_LEN - 1) as f32;
    let _ = Rectangle::new(Point::new(x0, GRAPH_Y), Size::new((x1 - x0) as u32, GRAPH_H as u32))
        .into_styled(PrimitiveStyle::with_stroke(TRACK, 1))
        .draw(target);
    let samples: alloc::vec::Vec<_> = history.iter().collect();
    let offset = HISTORY_LEN - samples.len();
    let y = |v: u8| GRAPH_Y + GRAPH_H - 1 - (GRAPH_H - 2) * i32::from(v.min(100)) / 100;
    for (i, pair) in samples.windows(2).enumerate() {
        let (Some((c0, m0)), Some((c1, m1))) = (pair[0], pair[1]) else { continue };
        let xa = x0 + ((offset + i) as f32 * step) as i32;
        let xb = x0 + ((offset + i + 1) as f32 * step) as i32;
        for (a, b, color) in [(m0, m1, MEM), (c0, c1, CPU)] {
            let _ = Line::new(Point::new(xa, y(a)), Point::new(xb, y(b)))
                .into_styled(PrimitiveStyle::with_stroke(color, 2))
                .draw(target);
        }
    }
    text(target, "cpu", x0 + 4, GRAPH_Y + 3, MonoTextStyle::new(&FONT_6X10, CPU));
    text(target, "mem", x0 + 30, GRAPH_Y + 3, MonoTextStyle::new(&FONT_6X10, MEM));
}

fn bar<D: DrawTarget<Color = Rgb565>>(target: &mut D, y: i32, part: Option<(u32, u32)>, color: Rgb565) {
    let w = W - 2 * PAD;
    fill(target, PAD, y, w, 8, TRACK);
    if let Some((used, total)) = part.filter(|(_, t)| *t > 0) {
        fill(target, PAD, y, (w as i64 * i64::from(used.min(total)) / i64::from(total)) as i32, 8, color);
    }
}

fn level(busy: Option<u8>, base: Rgb565) -> Rgb565 {
    match busy {
        Some(b) if b >= 90 => HOT,
        _ => base,
    }
}

fn temp_color(temp: Option<i16>) -> Rgb565 {
    match temp {
        Some(t) if t >= 85 => HOT,
        Some(t) if t >= 70 => CPU,
        _ => Rgb565::WHITE,
    }
}

fn pct(v: Option<u8>) -> String<8> {
    let mut s = String::new();
    match v {
        Some(v) => {
            let _ = write!(s, "{v}%");
        }
        None => {
            let _ = s.push('-');
        }
    }
    s
}

fn rate(kib: Option<u32>) -> String<16> {
    let mut s = String::new();
    let _ = match kib {
        Some(k) if k >= 1024 => write!(s, "{:.1} MB/s", k as f32 / 1024.0),
        Some(k) => write!(s, "{k} KB/s"),
        None => s.push('-').map_err(|_| core::fmt::Error),
    };
    s
}

fn gib(mib: u32) -> f32 {
    mib as f32 / 1024.0
}

fn text<D: DrawTarget<Color = Rgb565>>(target: &mut D, s: &str, x: i32, y: i32, style: MonoTextStyle<'_, Rgb565>) {
    let _ = Text::with_baseline(s, Point::new(x, y), style, Baseline::Top).draw(target);
}

fn fill<D: DrawTarget<Color = Rgb565>>(target: &mut D, x: i32, y: i32, w: i32, h: i32, color: Rgb565) {
    if w > 0 && h > 0 {
        let _ = Rectangle::new(Point::new(x, y), Size::new(w as u32, h as u32))
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(target);
    }
}
