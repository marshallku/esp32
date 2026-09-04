//! SHTC3 temperature and humidity, on the board's shared I2C bus.
//!
//! The bus carries the PCF85063 RTC and both audio codecs as well, so a
//! corrupted read is a live possibility rather than a theoretical one. Both
//! CRC bytes are checked and a mismatch discards the sample: putting a
//! plausible but wrong temperature on a screen whose whole job is being
//! trustworthy at a glance is worse than showing nothing there.

use esp_hal::{Blocking, delay::Delay, i2c::master::I2c};
use esp_println::println;

const ADDR: u8 = 0x70;

/// A single environment reading.
#[derive(Clone, Copy, Debug)]
pub struct Room {
    pub temp_c: f32,
    pub hum_pct: f32,
}

/// Sensirion CRC-8: polynomial 0x31, init 0xFF, no reflection, no final xor.
fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0xFFu8;
    for byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x31
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// One-shot temperature + humidity read, clock stretching disabled.
///
/// The sensor sleeps between reads, so a measurement is wake -> measure ->
/// sleep. Returns `None` for a missing sensor, a failed transfer or a CRC
/// mismatch; the caller draws the header without a reading rather than
/// failing the screen over it.
pub fn read(i2c: &mut I2c<'_, Blocking>, delay: &Delay) -> Option<Room> {
    i2c.write(ADDR, &[0x35, 0x17]).ok()?; // wake up
    delay.delay_millis(1);
    i2c.write(ADDR, &[0x78, 0x66]).ok()?; // measure T first, no stretch
    delay.delay_millis(15);

    let mut raw = [0u8; 6];
    let read = i2c.read(ADDR, &mut raw);
    let _ = i2c.write(ADDR, &[0xB0, 0x98]); // sleep, regardless
    read.ok()?;

    if crc8(&raw[0..2]) != raw[2] || crc8(&raw[3..5]) != raw[5] {
        println!("SHTC3 CRC mismatch — discarding sample");
        return None;
    }

    let temp_raw = u16::from_be_bytes([raw[0], raw[1]]);
    let hum_raw = u16::from_be_bytes([raw[3], raw[4]]);
    Some(Room {
        temp_c: -45.0 + 175.0 * (temp_raw as f32) / 65535.0,
        hum_pct: 100.0 * (hum_raw as f32) / 65535.0,
    })
}
