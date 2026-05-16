#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    i2c::master::{BusTimeout, Config as I2cConfig, I2c},
    main,
    time::Rate,
};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

const SCD41_ADDR: u8 = 0x62;

#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    println!("=== SCD41 CO2/Temp/Humidity monitor (SDA=GPIO8, SCL=GPIO9) ===");

    let i2c_config = I2cConfig::default()
        .with_frequency(Rate::from_khz(100))
        .with_timeout(BusTimeout::Maximum);
    let mut i2c = I2c::new(peripherals.I2C0, i2c_config)
        .expect("I2C init failed")
        .with_sda(peripherals.GPIO8)
        .with_scl(peripherals.GPIO9);

    let delay = Delay::new();
    delay.delay_millis(1500);

    // wake_up (no ACK expected)
    let _ = i2c.write(SCD41_ADDR, &[0x36, 0xF6]);
    delay.delay_millis(30);

    // stop_periodic_measurement in case of warm restart
    let _ = i2c.write(SCD41_ADDR, &[0x3F, 0x86]);
    delay.delay_millis(500);

    // Read serial number to confirm sensor is alive
    if i2c.write(SCD41_ADDR, &[0x36, 0x82]).is_ok() {
        delay.delay_millis(2);
        let mut sn = [0u8; 9];
        if i2c.read(SCD41_ADDR, &mut sn).is_ok() {
            let w1 = u16::from_be_bytes([sn[0], sn[1]]);
            let w2 = u16::from_be_bytes([sn[3], sn[4]]);
            let w3 = u16::from_be_bytes([sn[6], sn[7]]);
            println!("SCD41 serial: {:04x}{:04x}{:04x}", w1, w2, w3);
        }
    }

    // start_periodic_measurement (cmd 0x21B1) — samples every 5 seconds
    if i2c.write(SCD41_ADDR, &[0x21, 0xB1]).is_ok() {
        println!("Periodic measurement started. First sample in 5s.");
    } else {
        println!("FAILED to start periodic measurement");
    }

    loop {
        delay.delay_millis(5000);

        // get_data_ready_status (cmd 0xE4B8, read 3 bytes)
        if i2c.write(SCD41_ADDR, &[0xE4, 0xB8]).is_err() {
            println!("data_ready write failed");
            continue;
        }
        delay.delay_millis(2);
        let mut ready = [0u8; 3];
        if i2c.read(SCD41_ADDR, &mut ready).is_err() {
            println!("data_ready read failed");
            continue;
        }
        // Ready if the lower 11 bits are non-zero
        let ready_word = u16::from_be_bytes([ready[0], ready[1]]);
        if ready_word & 0x07FF == 0 {
            println!("(not ready yet, ready_word={:04x})", ready_word);
            continue;
        }

        // read_measurement (cmd 0xEC05, read 9 bytes)
        if i2c.write(SCD41_ADDR, &[0xEC, 0x05]).is_err() {
            println!("read_measurement write failed");
            continue;
        }
        delay.delay_millis(2);
        let mut m = [0u8; 9];
        if i2c.read(SCD41_ADDR, &mut m).is_err() {
            println!("read_measurement read failed");
            continue;
        }

        let co2 = u16::from_be_bytes([m[0], m[1]]);
        let temp_raw = u16::from_be_bytes([m[3], m[4]]);
        let hum_raw = u16::from_be_bytes([m[6], m[7]]);

        // Sensirion formulas
        let temp_c = -45.0 + 175.0 * (temp_raw as f32) / 65535.0;
        let hum_pct = 100.0 * (hum_raw as f32) / 65535.0;

        println!(
            "CO2: {} ppm  |  Temp: {:.2} C  |  Humidity: {:.2} %",
            co2, temp_c, hum_pct
        );
    }
}
