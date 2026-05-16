#![no_std]
#![no_main]

use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{ascii::{FONT_10X20, FONT_6X10}, MonoTextStyle, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    i2c::master::{BusTimeout, Config as I2cConfig, Error as I2cError, I2c},
    main,
    time::Rate,
    Blocking,
};
use esp_println::println;
use heapless::String;
use ssd1306::{
    mode::{BufferedGraphicsMode, DisplayConfig},
    prelude::{DisplayRotation, I2CInterface},
    size::DisplaySize128x64,
    I2CDisplayInterface, Ssd1306,
};

esp_bootloader_esp_idf::esp_app_desc!();

const SCD41_ADDR: u8 = 0x62;

/// Outcome of one cycle's SCD41 query.
enum Sample {
    Ok { co2: u16, temp_c: f32, hum_pct: f32 },
    NotReady,
    Error(&'static str),
}

fn poll_scd41(i2c: &mut I2c<'_, Blocking>, delay: &Delay) -> Sample {
    // 1. data_ready_status (cmd 0xE4B8)
    if i2c.write(SCD41_ADDR, &[0xE4, 0xB8]).is_err() {
        return Sample::Error("DR write");
    }
    delay.delay_millis(2);
    let mut ready = [0u8; 3];
    if i2c.read(SCD41_ADDR, &mut ready).is_err() {
        return Sample::Error("DR read");
    }
    let ready_word = u16::from_be_bytes([ready[0], ready[1]]);
    if ready_word & 0x07FF == 0 {
        return Sample::NotReady;
    }

    // 2. read_measurement (cmd 0xEC05, read 9 bytes)
    if i2c.write(SCD41_ADDR, &[0xEC, 0x05]).is_err() {
        return Sample::Error("RM write");
    }
    delay.delay_millis(2);
    let mut m = [0u8; 9];
    if i2c.read(SCD41_ADDR, &mut m).is_err() {
        return Sample::Error("RM read");
    }

    let co2 = u16::from_be_bytes([m[0], m[1]]);
    let temp_raw = u16::from_be_bytes([m[3], m[4]]);
    let hum_raw = u16::from_be_bytes([m[6], m[7]]);
    Sample::Ok {
        co2,
        temp_c: -45.0 + 175.0 * (temp_raw as f32) / 65535.0,
        hum_pct: 100.0 * (hum_raw as f32) / 65535.0,
    }
}

// Try to (re)start periodic measurement. Sends wake_up + stop + start.
// Returns true if start_periodic ACKed.
fn start_periodic(i2c: &mut I2c<'_, Blocking>, delay: &Delay) -> Result<(), I2cError> {
    let _ = i2c.write(SCD41_ADDR, &[0x36, 0xF6]); // wake_up (no ACK)
    delay.delay_millis(30);
    let _ = i2c.write(SCD41_ADDR, &[0x3F, 0x86]); // stop_periodic (idempotent)
    delay.delay_millis(500);
    i2c.write(SCD41_ADDR, &[0x21, 0xB1]) // start_periodic
}

type Display<'a> = Ssd1306<
    I2CInterface<I2c<'a, Blocking>>,
    DisplaySize128x64,
    BufferedGraphicsMode<DisplaySize128x64>,
>;

fn render(display: &mut Display<'_>, big: &MonoTextStyle<BinaryColor>, line1: &str, line2: &str, line3: &str) {
    display.clear_buffer();
    // 128x64, FONT_10X20 — 3 lines at y = 0, 22, 44
    Text::with_baseline(line1, Point::new(0, 0), *big, Baseline::Top)
        .draw(display)
        .ok();
    Text::with_baseline(line2, Point::new(0, 22), *big, Baseline::Top)
        .draw(display)
        .ok();
    Text::with_baseline(line3, Point::new(0, 44), *big, Baseline::Top)
        .draw(display)
        .ok();
    if display.flush().is_err() {
        println!("display flush failed");
    }
}

#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    println!("=== SCD41 (I2C0 SDA=8/SCL=9) + SSD1306 (I2C1 SDA=10/SCL=11) ===");

    let i2c_config = I2cConfig::default()
        .with_frequency(Rate::from_khz(100))
        .with_timeout(BusTimeout::Maximum);
    let mut i2c = I2c::new(peripherals.I2C0, i2c_config)
        .expect("I2C0 init failed")
        .with_sda(peripherals.GPIO8)
        .with_scl(peripherals.GPIO9);

    let i2c1_config = I2cConfig::default()
        .with_frequency(Rate::from_khz(100))
        .with_timeout(BusTimeout::Maximum);
    let i2c1 = I2c::new(peripherals.I2C1, i2c1_config)
        .expect("I2C1 init failed")
        .with_sda(peripherals.GPIO10)
        .with_scl(peripherals.GPIO11);

    let delay = Delay::new();
    delay.delay_millis(1500);

    // First start attempt (logged but not fatal — loop will retry).
    let mut have_sensor = start_periodic(&mut i2c, &delay).is_ok();
    println!("initial start_periodic: {}", if have_sensor { "OK" } else { "FAIL" });

    // --- SSD1306 init ---------------------------------------------------
    let interface = I2CDisplayInterface::new(i2c1);
    let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();
    display.init().expect("SSD1306 init failed");
    println!("SSD1306 init OK");

    let big = MonoTextStyleBuilder::new()
        .font(&FONT_10X20)
        .text_color(BinaryColor::On)
        .build();
    let small = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();

    // Splash (small font, more info fits)
    display.clear_buffer();
    Text::with_baseline("SCD41 monitor", Point::new(0, 0), small, Baseline::Top)
        .draw(&mut display)
        .ok();
    Text::with_baseline("waiting for sample...", Point::new(0, 12), small, Baseline::Top)
        .draw(&mut display)
        .ok();
    Text::with_baseline(
        if have_sensor { "sensor: OK" } else { "sensor: NACK" },
        Point::new(0, 24),
        small,
        Baseline::Top,
    )
    .draw(&mut display)
    .ok();
    display.flush().ok();

    // --- main loop ------------------------------------------------------
    let mut tick: u32 = 0;
    let mut consecutive_err: u32 = 0;
    loop {
        delay.delay_millis(5000);
        tick = tick.wrapping_add(1);

        let sample = if have_sensor {
            poll_scd41(&mut i2c, &delay)
        } else {
            Sample::Error("no sensor")
        };

        let mut line1: String<24> = String::new();
        let mut line2: String<24> = String::new();
        let mut line3: String<24> = String::new();

        match sample {
            Sample::Ok { co2, temp_c, hum_pct } => {
                consecutive_err = 0;
                println!(
                    "[{}] CO2: {} ppm  |  Temp: {:.2} C  |  Humidity: {:.2} %",
                    tick, co2, temp_c, hum_pct
                );
                let _ = write!(line1, "CO2 {} ppm", co2);
                let _ = write!(line2, "T {:.1}C H {:.0}%", temp_c, hum_pct);
                let _ = write!(line3, "t={}", tick);
            }
            Sample::NotReady => {
                println!("[{}] not ready", tick);
                let _ = write!(line1, "SCD41 wait");
                let _ = write!(line2, "warming up");
                let _ = write!(line3, "t={}", tick);
            }
            Sample::Error(stage) => {
                consecutive_err += 1;
                println!("[{}] SCD41 error ({}), streak={}", tick, stage, consecutive_err);
                let _ = write!(line1, "SCD41 ERR");
                let _ = write!(line2, "@{} x{}", stage, consecutive_err);
                let _ = write!(line3, "t={}", tick);

                // If we are persistently failing, try to re-initialize the sensor.
                // (Recovers from brown-outs or warm restarts mid-measurement.)
                if consecutive_err >= 3 {
                    println!("attempting sensor re-init...");
                    have_sensor = start_periodic(&mut i2c, &delay).is_ok();
                    println!("re-init: {}", if have_sensor { "OK" } else { "FAIL" });
                    consecutive_err = 0;
                }
            }
        }

        render(&mut display, &big, &line1, &line2, &line3);
    }
}
