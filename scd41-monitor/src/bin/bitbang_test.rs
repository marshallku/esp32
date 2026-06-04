//! Software bit-bang I2C test for SCD41 on GPIO8(SDA)/GPIO9(SCL).
//! Bypasses the hardware I2C controller and prints enough bus state to separate
//! controller failure from wiring, pin choice, power, and pull-up problems.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    gpio::{Flex, InputConfig, OutputConfig, Pull},
    main,
};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

const SCD41_ADDR: u8 = 0x62;
const HALF_PERIOD_US: u32 = 5; // ~100 kHz (with external 10k pull-ups)

struct BitbangI2c<'a> {
    sda: Flex<'a>,
    scl: Flex<'a>,
    delay: Delay,
}

impl<'a> BitbangI2c<'a> {
    fn new(mut sda: Flex<'a>, mut scl: Flex<'a>, delay: Delay) -> Self {
        sda.apply_input_config(&InputConfig::default().with_pull(Pull::Up));
        sda.apply_output_config(&OutputConfig::default());
        scl.apply_input_config(&InputConfig::default().with_pull(Pull::Up));
        scl.apply_output_config(&OutputConfig::default());
        sda.set_high();
        scl.set_high();
        sda.set_input_enable(true);
        scl.set_input_enable(true);
        sda.set_output_enable(false);
        scl.set_output_enable(false);
        Self { sda, scl, delay }
    }

    #[inline]
    fn half(&self) {
        self.delay.delay_micros(HALF_PERIOD_US);
    }

    #[inline]
    fn sda_release(&mut self) {
        self.sda.set_output_enable(false);
    }
    #[inline]
    fn sda_low(&mut self) {
        self.sda.set_low();
        self.sda.set_output_enable(true);
    }
    #[inline]
    fn scl_release(&mut self) {
        self.scl.set_output_enable(false);
    }
    #[inline]
    fn scl_low(&mut self) {
        self.scl.set_low();
        self.scl.set_output_enable(true);
    }
    #[inline]
    fn sda_read(&self) -> bool {
        self.sda.is_high()
    }
    #[inline]
    fn scl_read(&self) -> bool {
        self.scl.is_high()
    }

    fn print_levels(&mut self, label: &str) {
        self.sda_release();
        self.scl_release();
        self.half();
        println!(
            "{}: SDA={} SCL={}",
            label,
            if self.sda_read() { "HIGH" } else { "LOW" },
            if self.scl_read() { "HIGH" } else { "LOW" },
        );
    }

    fn recover_bus(&mut self) {
        self.sda_release();
        self.scl_release();
        self.half();

        for _ in 0..9 {
            if self.sda_read() && self.scl_read() {
                break;
            }
            self.scl_low();
            self.half();
            self.scl_release();
            self.half();
        }

        self.stop();
    }

    fn start(&mut self) {
        self.sda_release();
        self.scl_release();
        self.half();
        self.sda_low();
        self.half();
        self.scl_low();
    }
    fn stop(&mut self) {
        self.sda_low();
        self.half();
        self.scl_release();
        self.half();
        self.sda_release();
        self.half();
    }
    fn write_bit(&mut self, b: bool) {
        if b {
            self.sda_release()
        } else {
            self.sda_low()
        }
        self.half();
        self.scl_release();
        self.half();
        self.scl_low();
    }
    fn read_bit(&mut self) -> bool {
        self.sda_release();
        self.half();
        self.scl_release();
        self.half();
        let bit = self.sda_read();
        self.scl_low();
        bit
    }
    fn write_byte(&mut self, byte: u8) -> bool {
        for i in 0..8 {
            self.write_bit((byte >> (7 - i)) & 1 != 0);
        }
        !self.read_bit() // ACK = SDA low
    }
    fn read_byte(&mut self, ack: bool) -> u8 {
        let mut byte = 0u8;
        for _ in 0..8 {
            byte = (byte << 1) | (self.read_bit() as u8);
        }
        self.write_bit(!ack);
        byte
    }
    fn write(&mut self, addr: u8, data: &[u8]) -> Result<(), &'static str> {
        self.start();
        if !self.write_byte(addr << 1) {
            self.stop();
            return Err("addr-w NACK");
        }
        for &b in data {
            if !self.write_byte(b) {
                self.stop();
                return Err("data NACK");
            }
        }
        self.stop();
        Ok(())
    }
    fn read(&mut self, addr: u8, buf: &mut [u8]) -> Result<(), &'static str> {
        self.start();
        if !self.write_byte((addr << 1) | 1) {
            self.stop();
            return Err("addr-r NACK");
        }
        let len = buf.len();
        for i in 0..len {
            buf[i] = self.read_byte(i < len - 1);
        }
        self.stop();
        Ok(())
    }
    fn probe_write_addr(&mut self, addr: u8) -> bool {
        self.start();
        let ack = self.write_byte(addr << 1);
        self.stop();
        ack
    }
}

#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let delay = Delay::new();
    delay.delay_millis(500);

    println!("=== bitbang-test: SCD41 on GPIO8(SDA)/GPIO9(SCL) ===");

    let sda = Flex::new(peripherals.GPIO8);
    let scl = Flex::new(peripherals.GPIO9);
    let mut i2c = BitbangI2c::new(sda, scl, delay);

    delay.delay_millis(1500);
    i2c.print_levels("idle before recovery");
    i2c.recover_bus();
    i2c.print_levels("idle after recovery");

    println!("scanning bus (0x08..0x77)...");
    let mut found = 0u32;
    for addr in 0x08u8..=0x77 {
        if i2c.probe_write_addr(addr) {
            println!("  ACK at 0x{:02x}", addr);
            found += 1;
        }
    }
    println!("scan done: {} device(s) (SCD41 expected at 0x62)", found);

    println!("SCD41 wake_up...");
    let _ = i2c.write(SCD41_ADDR, &[0x36, 0xF6]);
    delay.delay_millis(30);
    println!("SCD41 stop_periodic...");
    let _ = i2c.write(SCD41_ADDR, &[0x3F, 0x86]);
    delay.delay_millis(500);
    println!("SCD41 start_periodic...");
    match i2c.write(SCD41_ADDR, &[0x21, 0xB1]) {
        Ok(_) => println!("start_periodic: OK"),
        Err(e) => println!("start_periodic: FAIL ({})", e),
    }

    loop {
        delay.delay_millis(5000);

        if let Err(e) = i2c.write(SCD41_ADDR, &[0xE4, 0xB8]) {
            println!("DR write err: {}", e);
            continue;
        }
        delay.delay_millis(2);
        let mut ready = [0u8; 3];
        if let Err(e) = i2c.read(SCD41_ADDR, &mut ready) {
            println!("DR read err: {}", e);
            continue;
        }
        let dr = u16::from_be_bytes([ready[0], ready[1]]);
        if dr & 0x07FF == 0 {
            println!("not ready (DR=0x{:04x})", dr);
            continue;
        }

        if let Err(e) = i2c.write(SCD41_ADDR, &[0xEC, 0x05]) {
            println!("RM write err: {}", e);
            continue;
        }
        delay.delay_millis(2);
        let mut m = [0u8; 9];
        if let Err(e) = i2c.read(SCD41_ADDR, &mut m) {
            println!("RM read err: {}", e);
            continue;
        }
        let co2 = u16::from_be_bytes([m[0], m[1]]);
        let t_raw = u16::from_be_bytes([m[3], m[4]]);
        let h_raw = u16::from_be_bytes([m[6], m[7]]);
        let t = -45.0 + 175.0 * (t_raw as f32) / 65535.0;
        let h = 100.0 * (h_raw as f32) / 65535.0;
        println!("CO2 {} ppm  T {:.2} C  H {:.2} %", co2, t, h);
    }
}
