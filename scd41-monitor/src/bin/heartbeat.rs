//! Minimal binary: prints "tick N" every second. Used to isolate whether the
//! board's native USB-Serial-JTAG can actually carry esp-println output.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::{clock::CpuClock, delay::Delay, main};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

#[main]
fn main() -> ! {
    let _peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let delay = Delay::new();
    let mut n: u32 = 0;
    loop {
        println!("tick {}", n);
        n += 1;
        delay.delay_millis(1000);
    }
}
