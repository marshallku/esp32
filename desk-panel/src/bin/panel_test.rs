//! Panel and touch bring-up test for the Guition ESP32-S3-4848S040 (ST7701S
//! 480×480 RGB, GT911 touch). No WiFi, no application.
//!
//! An RGB panel can fail in more ways than an SPI one, and most of them look
//! alike, so each stage reports on the console before the next one starts:
//!
//! - PSRAM size, or a panic before it: octal PSRAM did not come up, and the
//!   480 KB frames have nowhere to live
//! - GT911 product ID: "911" means touch is wired and alive
//! - backlight on but the screen stays black: ST7701 init or 3-wire SPI pins
//! - colours or bars wrong: DPI data pins or timing
//! - STRESS: the picture must hold still while PSRAM is hammered. Every frame
//!   is restarted from the top of the buffer (see `scanout`), so an underrun
//!   can spoil one frame but must never leave the border slid off the edge.
//! - TOUCH: a dot follows the finger. A dot that lands mirrored or rotated
//!   from the finger means the touch axes need swapping, not the panel.
//!
//! Run with:  make flash PKG=desk-panel BIN=panel-test PORT=/dev/ttyUSB0

#![no_std]
#![no_main]

extern crate alloc;

use desk_panel::{
    display::Display,
    display_pins,
    framebuffer::{HEIGHT, WIDTH},
    gt911::Gt911,
};
use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_10X20},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use esp_backtrace as _;
// The library carries the WiFi status client, so esp-radio ends up in this
// binary's link even though it never touches the radio. Pulling esp-rtos in
// supplies the scheduler symbols esp-radio references; nothing here starts it.
use esp_rtos as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    i2c::master::{Config as I2cConfig, I2c},
    main,
    psram::{PsramConfig, PsramMode, SpiRamFreq},
    system::{CpuControl, Stack},
    time::{Instant, Rate},
};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

const HOLD_MS: u32 = 3000;
const STRESS_MS: u64 = 10_000;

#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let delay = Delay::new();

    println!("=== 4848S040 panel + touch bring-up test ===");

    esp_alloc::psram_allocator!(
        peripherals.PSRAM,
        esp_hal::psram,
        PsramConfig {
            mode: PsramMode::OctalSpi,
            ram_frequency: SpiRamFreq::Freq80m,
            ..Default::default()
        }
    );
    println!("1/4 PSRAM: {} KB free on the heap", esp_alloc::HEAP.free() / 1024);

    let i2c = I2c::new(peripherals.I2C0, I2cConfig::default().with_frequency(Rate::from_khz(400)))
        .expect("I2C0 config")
        .with_sda(peripherals.GPIO19)
        .with_scl(peripherals.GPIO45);
    let mut touch = Gt911::new(i2c);
    match touch.product_id() {
        Ok(id) => println!("2/4 GT911 product ID: {:?}", core::str::from_utf8(&id[..3]).unwrap_or("?")),
        Err(e) => println!("2/4 GT911 not answering: {:?}", e),
    }

    let (mut display, scanout) = Display::new(peripherals.LCD_CAM, peripherals.DMA_CH0, display_pins!(peripherals));
    static mut SCANOUT_STACK: Stack<8192> = Stack::new();
    let mut cpu = CpuControl::new(peripherals.CPU_CTRL);
    // Held for the life of the program: dropping the guard parks the core.
    let _scanout_core = cpu
        .start_app_core(unsafe { &mut *core::ptr::addr_of_mut!(SCANOUT_STACK) }, move || scanout.run())
        .expect("second core start");
    println!("3/4 scanout running on the second core, restarted every frame");

    let style = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    let fills: [(&str, Rgb565); 3] = [("RED", Rgb565::RED), ("GREEN", Rgb565::GREEN), ("BLUE", Rgb565::BLUE)];
    for (name, color) in fills {
        println!("FILL {}", name);
        let _ = display.canvas().clear(color);
        display.present();
        delay.delay_millis(HOLD_MS);
    }

    // Two frames that differ everywhere, presented back to back: each present
    // is a full-frame PSRAM-to-PSRAM copy competing with the scanout for the
    // same bus — far more traffic than the real UI will ever make.
    println!("STRESS — {} s of back-to-back full-frame presents; the border must not move", STRESS_MS / 1000);
    let start = Instant::now();
    let mut presents: u32 = 0;
    while start.elapsed().as_millis() < STRESS_MS {
        let mut fb = display.canvas();
        let _ = fb.clear(if presents % 2 == 0 { Rgb565::BLACK } else { Rgb565::CSS_DARK_SLATE_GRAY });
        draw_border(&mut fb);
        display.present();
        presents += 1;
    }
    println!("  {} presents in {} s = {} per second", presents, STRESS_MS / 1000, presents as u64 * 1000 / STRESS_MS);

    println!("4/4 TOUCH — draw with a finger; coordinates print below");
    {
        let mut fb = display.canvas();
        let _ = fb.clear(Rgb565::BLACK);
        draw_border(&mut fb);
        let _ = Text::with_baseline("touch to draw", Point::new(170, 10), style, Baseline::Top).draw(&mut fb);
        let _ = Text::with_baseline("TL", Point::new(8, 8), style, Baseline::Top).draw(&mut fb);
        let _ = Text::with_baseline("BR", Point::new(WIDTH as i32 - 30, HEIGHT as i32 - 28), style, Baseline::Top)
            .draw(&mut fb);
    }
    display.present();

    let mut down = false;
    loop {
        match touch.poll() {
            Ok(Some(Some(pt))) => {
                if !down {
                    println!("  down at ({}, {})", pt.x, pt.y);
                }
                down = true;
                let _ = Circle::with_center(Point::new(pt.x as i32, pt.y as i32), 9)
                    .into_styled(PrimitiveStyle::with_fill(Rgb565::YELLOW))
                    .draw(&mut display.canvas());
                display.present();
            }
            Ok(Some(None)) => {
                if down {
                    println!("  up");
                }
                down = false;
            }
            Ok(None) => {}
            Err(e) => println!("  touch read failed: {:?}", e),
        }
        delay.delay_millis(10);
    }
}

fn draw_border<D: DrawTarget<Color = Rgb565>>(target: &mut D) {
    let _ = Rectangle::new(Point::zero(), Size::new(WIDTH, HEIGHT))
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::YELLOW, 2))
        .draw(target);
}
