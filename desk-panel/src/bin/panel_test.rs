//! Panel bring-up test for the Guition ESP32-S3-4848S040 (ST7701S 480×480
//! RGB, GT911 touch). No WiFi, no application.
//!
//! An RGB panel can fail in more ways than an SPI one, and most of them look
//! alike, so each stage reports on the console before the next one starts:
//!
//! - PSRAM size, or a panic before it: octal PSRAM did not come up, and the
//!   480 KB frame has nowhere to live
//! - I2C scan: GT911 at 0x5D or 0x14 confirms SDA=19/SCL=45 and that the
//!   touch half of the pinout is right
//! - backlight on but the screen stays black: ST7701 init or 3-wire SPI pins
//! - the screen shows noise or stripes: init worked, DPI timing or data pins
//!   are wrong
//! - solid fills are right but bars come out in the wrong order: R/B swapped
//! - colours right but the picture rolls or tears: PSRAM bandwidth
//!
//! Each pattern prints the frame rate it actually achieved. Frames are re-sent
//! from the CPU one at a time here, so the gap between them is part of the
//! measurement — the panel tolerates it as a slightly longer vertical blank.
//!
//! Run with:  make flash PKG=desk-panel BIN=panel-test PORT=/dev/ttyUSB0

#![no_std]
#![no_main]

extern crate alloc;

use desk_panel::{
    framebuffer::{self, Framebuffer, HEIGHT, WIDTH},
    st7701::St7701,
};
use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii::FONT_10X20},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    dma::{DmaTxBuf, ExternalBurstConfig},
    gpio::{Level, Output, OutputConfig},
    i2c::master::{Config as I2cConfig, I2c},
    lcd_cam::{
        LcdCam,
        lcd::{
            ClockMode, Phase, Polarity,
            dpi::{Config as DpiConfig, Dpi, Format, FrameTiming},
        },
    },
    main,
    psram::{PsramConfig, PsramMode, SpiRamFreq},
    time::{Instant, Rate},
};
use esp_println::{print, println};

esp_bootloader_esp_idf::esp_app_desc!();

const HOLD_MS: u64 = 4000;

// The 4848S040 timings from ESPHome's device page, in esp-idf's terms. esp-hal
// counts the horizontal and vertical "front porch" from the start of the sync
// pulse to the first active pixel, which is esp-idf's pulse + back porch.
const PCLK_MHZ: u32 = 12;
const HSYNC_PULSE: usize = 8;
const HSYNC_BACK: usize = 20;
const HSYNC_FRONT: usize = 10;
const VSYNC_PULSE: usize = 8;
const VSYNC_BACK: usize = 10;
const VSYNC_FRONT: usize = 10;

#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    println!("=== 4848S040 panel bring-up test ===");

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

    // Touch first: it is the cheapest pin check on the board and it does not
    // depend on anything below working.
    match I2c::new(peripherals.I2C0, I2cConfig::default()) {
        Ok(i2c) => {
            let mut i2c = i2c.with_sda(peripherals.GPIO19).with_scl(peripherals.GPIO45);
            print!("2/4 I2C scan (SDA=19 SCL=45):");
            let mut found = 0;
            for addr in 0x08u8..0x78 {
                if i2c.write(addr, &[]).is_ok() {
                    print!(" 0x{:02X}", addr);
                    found += 1;
                }
            }
            println!(
                "{}",
                if found == 0 { " nothing — touch pins or pull-ups are wrong" } else { "  (GT911 is 0x5D or 0x14)" }
            );
        }
        Err(e) => println!("2/4 I2C config FAILED: {:?}", e),
    }

    let out = OutputConfig::default();
    let _backlight = Output::new(peripherals.GPIO38, Level::High, out);
    let mut panel = St7701::new(
        Output::new(peripherals.GPIO39, Level::High, out),
        Output::new(peripherals.GPIO48, Level::High, out),
        Output::new(peripherals.GPIO47, Level::Low, out),
    );
    panel.init();
    println!("3/4 backlight on, ST7701 init sent over 3-wire SPI (CS=39 SCK=48 SDA=47)");

    let fb_bytes: &'static mut [u8] = alloc::vec![0u8; framebuffer::BYTES].leak();
    let (_, descriptors) = esp_hal::dma_descriptors!(0, framebuffer::BYTES);
    let mut buf = match DmaTxBuf::new_with_config(descriptors, fb_bytes, ExternalBurstConfig::Size64) {
        Ok(buf) => buf,
        Err(e) => panic!("DMA buffer over PSRAM rejected: {:?}", e),
    };

    let dpi_config = DpiConfig::default()
        .with_frequency(Rate::from_mhz(PCLK_MHZ))
        .with_clock_mode(ClockMode { polarity: Polarity::IdleLow, phase: Phase::ShiftLow })
        .with_format(Format { enable_2byte_mode: true, ..Default::default() })
        .with_timing(FrameTiming {
            horizontal_active_width: WIDTH as usize,
            horizontal_total_width: WIDTH as usize + HSYNC_PULSE + HSYNC_BACK + HSYNC_FRONT,
            horizontal_blank_front_porch: HSYNC_PULSE + HSYNC_BACK,
            vertical_active_height: HEIGHT as usize,
            vertical_total_height: HEIGHT as usize + VSYNC_PULSE + VSYNC_BACK + VSYNC_FRONT,
            vertical_blank_front_porch: VSYNC_PULSE + VSYNC_BACK,
            hsync_width: HSYNC_PULSE,
            vsync_width: VSYNC_PULSE,
            hsync_position: 0,
        })
        .with_vsync_idle_level(Level::High)
        .with_hsync_idle_level(Level::High)
        .with_de_idle_level(Level::Low)
        .with_disable_black_region(false);

    let lcd_cam = LcdCam::new(peripherals.LCD_CAM);
    let mut dpi = Dpi::new(lcd_cam.lcd, peripherals.DMA_CH0, dpi_config)
        .unwrap_or_else(|e| panic!("DPI config rejected: {:?}", e))
        .with_vsync(peripherals.GPIO17)
        .with_hsync(peripherals.GPIO16)
        .with_de(peripherals.GPIO18)
        .with_pclk(peripherals.GPIO21)
        // Blue
        .with_data0(peripherals.GPIO4)
        .with_data1(peripherals.GPIO5)
        .with_data2(peripherals.GPIO6)
        .with_data3(peripherals.GPIO7)
        .with_data4(peripherals.GPIO15)
        // Green
        .with_data5(peripherals.GPIO8)
        .with_data6(peripherals.GPIO20)
        .with_data7(peripherals.GPIO3)
        .with_data8(peripherals.GPIO46)
        .with_data9(peripherals.GPIO9)
        .with_data10(peripherals.GPIO10)
        // Red
        .with_data11(peripherals.GPIO11)
        .with_data12(peripherals.GPIO12)
        .with_data13(peripherals.GPIO13)
        .with_data14(peripherals.GPIO14)
        .with_data15(peripherals.GPIO0);
    println!("4/4 DPI up: {}x{} RGB565 @ {} MHz PCLK, frame in PSRAM", WIDTH, HEIGHT, PCLK_MHZ);

    let style = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    let fills: [(&str, Rgb565); 5] = [
        ("RED", Rgb565::RED),
        ("GREEN", Rgb565::GREEN),
        ("BLUE", Rgb565::BLUE),
        ("WHITE", Rgb565::WHITE),
        ("BLACK", Rgb565::BLACK),
    ];
    let mut pass: u32 = 0;

    loop {
        pass += 1;
        println!("--- pass {} ---", pass);

        for (name, color) in fills {
            println!("FILL {} — expect the whole screen {}", name, name);
            let _ = Framebuffer::new(buf.as_mut_slice()).clear(color);
            (dpi, buf) = hold(dpi, buf, name);
        }

        println!("BARS — expect red, green, blue, white left to right, text upright");
        {
            let mut fb = Framebuffer::new(buf.as_mut_slice());
            let _ = fb.clear(Rgb565::BLACK);
            for (i, color) in [Rgb565::RED, Rgb565::GREEN, Rgb565::BLUE, Rgb565::WHITE].into_iter().enumerate() {
                let _ = Rectangle::new(Point::new(i as i32 * 120, 0), Size::new(120, 360))
                    .into_styled(PrimitiveStyle::with_fill(color))
                    .draw(&mut fb);
            }
            let _ = Rectangle::new(Point::zero(), Size::new(WIDTH, HEIGHT))
                .into_styled(PrimitiveStyle::with_stroke(Rgb565::YELLOW, 2))
                .draw(&mut fb);
            let _ = Text::with_baseline("TOP LEFT", Point::new(8, 370), style, Baseline::Top).draw(&mut fb);
            let _ =
                Text::with_baseline("4848S040 OK", Point::new(170, 410), style, Baseline::Top).draw(&mut fb);
            let _ = Text::with_baseline("BOTTOM RIGHT", Point::new(348, 450), style, Baseline::Top)
                .draw(&mut fb);
        }
        (dpi, buf) = hold(dpi, buf, "bars");
    }
}

/// Re-sends the frame back to back for `HOLD_MS` and reports the rate it got.
/// A DMA error ends the hold early and is reported rather than panicking, so a
/// marginal timing setting shows up as a number instead of a reboot loop.
fn hold(mut dpi: Dpi<'static, esp_hal::Blocking>, mut buf: DmaTxBuf, label: &str) -> (Dpi<'static, esp_hal::Blocking>, DmaTxBuf) {
    let start = Instant::now();
    let mut frames: u32 = 0;
    let mut errors: u32 = 0;

    while start.elapsed().as_millis() < HOLD_MS {
        let transfer = match dpi.send(false, buf) {
            Ok(t) => t,
            Err((e, d, b)) => {
                println!("  [{}] send FAILED: {:?}", label, e);
                return (d, b);
            }
        };
        let (result, d, b) = transfer.wait();
        dpi = d;
        buf = b;
        match result {
            Ok(()) => frames += 1,
            Err(_) => errors += 1,
        }
    }

    let secs = start.elapsed().as_millis() as f32 / 1000.0;
    println!("  [{}] {} frames in {:.1}s = {:.1} fps, {} DMA errors", label, frames, secs, frames as f32 / secs, errors);
    (dpi, buf)
}
