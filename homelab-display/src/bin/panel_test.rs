//! Panel bring-up test for the ESP32-S3-RLCD-4.2. No WiFi, no sensors.
//!
//! Exists to split one ambiguous symptom ("nothing on screen") into two very
//! different problems. It walks a sequence of whole-screen patterns and names
//! each one on the serial console, so what you see — or do not see — says
//! where the fault is:
//!
//! - nothing at all, ever: transport, init sequence or wiring
//! - the screen changes: transport and init are fine, and the fault is in what
//!   the app draws rather than in the driver
//! - inverted (white on black): flip `INK_CLEARS_BIT` in st7305.rs
//!
//! Run with:  make flash PKG=homelab-display BIN=panel-test

#![no_std]
#![no_main]

use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_10X20},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use esp_backtrace as _;
// The library now carries the networking module, so esp-radio ends up in this
// binary's link even though it never touches the radio. Pulling esp-rtos in
// supplies the scheduler symbols esp-radio references; nothing here starts it.
use esp_rtos as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
    main,
};
use esp_println::println;
use homelab_display::{
    bitbang_spi::BitBangSpi,
    st7305::{HEIGHT, St7305, WIDTH},
};

esp_bootloader_esp_idf::esp_app_desc!();

type Panel = St7305<BitBangSpi, Output<'static>, Output<'static>, Output<'static>>;

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

const HOLD_MS: u32 = 5000;

fn show(panel: &mut Panel, label: &str) {
    match panel.flush() {
        Ok(()) => println!("  [{}] flushed OK", label),
        Err(e) => println!("  [{}] flush FAILED: {:?}", label, e),
    }
}

#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let mut delay = Delay::new();

    println!("=== ST7305 panel bring-up test ===");
    println!("    {}x{}, bit-banged SPI, SCK=11 MOSI=12 DC=5 CS=40 RST=41", WIDTH, HEIGHT);

    let out = OutputConfig::default();
    let spi = BitBangSpi::new(
        Output::new(peripherals.GPIO11, Level::Low, out),
        Output::new(peripherals.GPIO12, Level::Low, out),
    );

    let dc = Output::new(peripherals.GPIO5, Level::Low, out);
    let cs = Output::new(peripherals.GPIO40, Level::High, out);
    let rst = Output::new(peripherals.GPIO41, Level::High, out);

    let panel: &'static mut Panel = mk_static!(Panel, St7305::new(spi, dc, cs, rst));
    match panel.init(&mut delay) {
        Ok(()) => println!("init OK"),
        Err(e) => println!("init FAILED: {:?}", e),
    }

    let style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    let mut pass: u32 = 0;

    loop {
        pass += 1;
        println!("--- pass {} ---", pass);

        // 1. Everything on. If the panel can show anything at all, it shows
        //    this. A screen that stays blank here never got the data.
        println!("1/4 ALL INK — expect a fully dark screen");
        panel.clear(BinaryColor::On);
        show(panel, "all-ink");
        delay.delay_millis(HOLD_MS);

        // 2. The inverse, to prove the change is driven and not a coincidence
        //    of how the panel powers up.
        println!("2/4 ALL BACKGROUND — expect a fully light screen");
        panel.clear(BinaryColor::Off);
        show(panel, "all-bg");
        delay.delay_millis(HOLD_MS);

        // 3. Quadrants plus a border. Wrong pixel packing survives steps 1 and
        //    2 untouched but falls apart here, and the diagonals show it as
        //    stripes or scatter rather than clean lines.
        println!("3/4 QUADRANTS + BORDER + DIAGONALS — expect crisp geometry");
        panel.clear(BinaryColor::Off);
        let _ = Rectangle::new(Point::new(0, 0), Size::new(WIDTH / 2, HEIGHT / 2))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(panel);
        let _ = Rectangle::new(
            Point::new(WIDTH as i32 / 2, HEIGHT as i32 / 2),
            Size::new(WIDTH / 2, HEIGHT / 2),
        )
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(panel);
        let _ = Rectangle::new(Point::new(0, 0), Size::new(WIDTH, HEIGHT))
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 3))
            .draw(panel);
        let _ = Line::new(Point::new(0, 0), Point::new(WIDTH as i32 - 1, HEIGHT as i32 - 1))
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
            .draw(panel);
        let _ = Line::new(Point::new(WIDTH as i32 - 1, 0), Point::new(0, HEIGHT as i32 - 1))
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
            .draw(panel);
        show(panel, "quadrants");
        delay.delay_millis(HOLD_MS);

        // 4. Text, with the corners labelled. Readable text proves the mapping
        //    end to end; mirrored or upside-down text means the orientation is
        //    wrong rather than the packing.
        println!("4/4 TEXT — expect readable text and corner labels");
        panel.clear(BinaryColor::Off);
        let _ = Text::with_baseline("ST7305 OK", Point::new(20, 120), style, Baseline::Top)
            .draw(panel);
        let _ = Text::with_baseline("TOP LEFT", Point::new(4, 4), style, Baseline::Top).draw(panel);
        let _ = Text::with_baseline(
            "BOTTOM RIGHT",
            Point::new(WIDTH as i32 - 124, HEIGHT as i32 - 22),
            style,
            Baseline::Top,
        )
        .draw(panel);
        show(panel, "text");
        delay.delay_millis(HOLD_MS);
    }
}
