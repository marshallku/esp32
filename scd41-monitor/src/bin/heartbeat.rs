// Minimal sanity-check binary: print a counter once per second.
// If `make flash PKG=scd41-monitor BIN=...` flashes this and the counter
// shows up on serial, esp-println output path is fine and any silence
// from the real firmware means it's hanging during init.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::{clock::CpuClock, delay::Delay, main};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

#[main]
fn main() -> ! {
    let _ = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let delay = Delay::new();
    let mut i: u32 = 0;
    loop {
        println!("heartbeat {}", i);
        i = i.wrapping_add(1);
        delay.delay_millis(1000);
    }
}
