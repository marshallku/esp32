//! Air sensor: reads SCD41 (CO2/temp/humidity), shows on SSD1306 OLED, and
//! POSTs samples to InfluxDB v2 over WiFi.
//!
//! Bus layout (screw-terminal expansion board accepts one wire per pin, so
//! SCD41 and SSD1306 each get their own I2C controller):
//!   - I2C0: SCD41   (SDA=GPIO8,  SCL=GPIO9)
//!   - I2C1: SSD1306 (SDA=GPIO10, SCL=GPIO11)
//!
//! Required env vars at build time (loaded by the workspace Makefile from .env):
//!   WIFI_SSID, WIFI_PASSWORD,
//!   INFLUX_URL, INFLUX_ORG, INFLUX_BUCKET, INFLUX_TOKEN,
//!   INFLUX_MEASUREMENT, INFLUX_TAGS.

#![no_std]
#![no_main]

use core::fmt::Write as _;

use embassy_executor::Spawner;
use embassy_net::{
    dns::DnsSocket,
    tcp::client::{TcpClient, TcpClientState},
    Runner, Stack, StackResources,
};
use embassy_time::{Duration, Timer};
use embedded_graphics::{
    mono_font::{ascii::{FONT_10X20, FONT_6X10}, MonoTextStyle, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    i2c::master::{BusTimeout, Config as I2cConfig, I2c},
    interrupt::software::SoftwareInterruptControl,
    timer::timg::TimerGroup,
    Blocking,
};
use esp_println::println;
use esp_radio::wifi::{
    sta::StationConfig, Config as WifiCfg, ControllerConfig, Interface as WifiInterface,
    WifiController,
};
use heapless::String as HString;
use reqwless::{
    client::HttpClient,
    request::{Method, RequestBuilder},
};
use ssd1306::{
    mode::{BufferedGraphicsMode, DisplayConfig},
    prelude::{DisplayRotation, I2CInterface},
    size::DisplaySize128x64,
    I2CDisplayInterface, Ssd1306,
};

esp_bootloader_esp_idf::esp_app_desc!();

// --- env-injected configuration --------------------------------------------
const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASSWORD");
const INFLUX_URL: &str = concat!(
    env!("INFLUX_URL"),
    "/api/v2/write?org=", env!("INFLUX_ORG"),
    "&bucket=", env!("INFLUX_BUCKET"),
    "&precision=s",
);
const AUTH: &str = concat!("Token ", env!("INFLUX_TOKEN"));
const MEASUREMENT: &str = env!("INFLUX_MEASUREMENT");
const TAGS: &str = env!("INFLUX_TAGS");

const SCD41_ADDR: u8 = 0x62;

// --- helper: static cell so async tasks can hold long-lived references -----
macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

// --- SCD41 ------------------------------------------------------------------
enum Sample {
    Ok { co2: u16, temp_c: f32, hum_pct: f32 },
    NotReady,
    Error(&'static str),
}

fn poll_scd41(i2c: &mut I2c<'_, Blocking>, delay: &Delay) -> Sample {
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

fn start_periodic(i2c: &mut I2c<'_, Blocking>, delay: &Delay) -> bool {
    let _ = i2c.write(SCD41_ADDR, &[0x36, 0xF6]); // wake_up
    delay.delay_millis(30);
    let _ = i2c.write(SCD41_ADDR, &[0x3F, 0x86]); // stop_periodic
    delay.delay_millis(500);
    i2c.write(SCD41_ADDR, &[0x21, 0xB1]).is_ok() // start_periodic
}

// --- display ----------------------------------------------------------------
type Display<'a> = Ssd1306<
    I2CInterface<I2c<'a, Blocking>>,
    DisplaySize128x64,
    BufferedGraphicsMode<DisplaySize128x64>,
>;

fn render(display: &mut Display<'_>, style: &MonoTextStyle<BinaryColor>, l1: &str, l2: &str, l3: &str) {
    display.clear_buffer();
    Text::with_baseline(l1, Point::new(0, 0), *style, Baseline::Top)
        .draw(display).ok();
    Text::with_baseline(l2, Point::new(0, 22), *style, Baseline::Top)
        .draw(display).ok();
    Text::with_baseline(l3, Point::new(0, 44), *style, Baseline::Top)
        .draw(display).ok();
    if display.flush().is_err() {
        println!("display flush failed");
    }
}

// --- WiFi tasks -------------------------------------------------------------
#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) -> ! {
    println!("WiFi connection task start");
    loop {
        match controller.connect_async().await {
            Ok(info) => {
                println!("WiFi connected: {:?}", info);
                let info = controller.wait_for_disconnect_async().await.ok();
                println!("WiFi disconnected: {:?}", info);
            }
            Err(e) => println!("WiFi connect failed: {:?}", e),
        }
        Timer::after(Duration::from_millis(5000)).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, WifiInterface<'static>>) -> ! {
    runner.run().await
}

// --- InfluxDB POST ---------------------------------------------------------
async fn post_influx(
    stack: Stack<'static>,
    tcp_client: &TcpClient<'static, 1, 1500, 1500>,
    dns_client: &DnsSocket<'static>,
    line: &str,
) -> Result<u16, ()> {
    let _ = stack; // stack reachable via tcp/dns clients
    let mut http = HttpClient::new(tcp_client, dns_client);
    let mut rx_buf = [0u8; 512];

    let builder = http
        .request(Method::POST, INFLUX_URL)
        .await
        .map_err(|e| {
            println!("influx request error: {:?}", e);
        })?;
    let mut req = builder
        .headers(&[
            ("Authorization", AUTH),
            ("Content-Type", "text/plain; charset=utf-8"),
        ])
        .body(line.as_bytes());
    match req.send(&mut rx_buf).await {
        Ok(resp) => Ok(resp.status.0),
        Err(e) => {
            println!("influx send error: {:?}", e);
            Err(())
        }
    }
}

// --- main -------------------------------------------------------------------
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(size: 100 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    println!(
        "=== air monitor — SCD41(I2C0)+SSD1306(I2C1)+WiFi+InfluxDB ===\n     {} ({})",
        MEASUREMENT, TAGS
    );

    // ---- I2C: SCD41 on I2C0, SSD1306 on I2C1 ----
    let i2c0_cfg = I2cConfig::default()
        .with_frequency(esp_hal::time::Rate::from_khz(100))
        .with_timeout(BusTimeout::Maximum);
    let mut scd_i2c = I2c::new(peripherals.I2C0, i2c0_cfg)
        .expect("I2C0 init")
        .with_sda(peripherals.GPIO8)
        .with_scl(peripherals.GPIO9);

    let i2c1_cfg = I2cConfig::default()
        .with_frequency(esp_hal::time::Rate::from_khz(100))
        .with_timeout(BusTimeout::Maximum);
    let display_i2c = I2c::new(peripherals.I2C1, i2c1_cfg)
        .expect("I2C1 init")
        .with_sda(peripherals.GPIO10)
        .with_scl(peripherals.GPIO11);

    let delay = Delay::new();
    delay.delay_millis(1500); // SCD41 power-up settle

    let mut have_sensor = start_periodic(&mut scd_i2c, &delay);
    println!("SCD41 start_periodic: {}", if have_sensor { "OK" } else { "FAIL" });

    // ---- SSD1306 ----
    let interface = I2CDisplayInterface::new(display_i2c);
    let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();
    display.init().expect("SSD1306 init");

    let big = MonoTextStyleBuilder::new()
        .font(&FONT_10X20)
        .text_color(BinaryColor::On)
        .build();
    let small = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();

    // Splash while WiFi comes up.
    display.clear_buffer();
    Text::with_baseline("air monitor", Point::new(0, 0), small, Baseline::Top)
        .draw(&mut display).ok();
    Text::with_baseline("connecting WiFi...", Point::new(0, 12), small, Baseline::Top)
        .draw(&mut display).ok();
    Text::with_baseline(SSID, Point::new(0, 24), small, Baseline::Top)
        .draw(&mut display).ok();
    display.flush().ok();

    // ---- WiFi ----
    println!("WiFi: connecting to '{}'", SSID);
    let station = WifiCfg::Station(
        StationConfig::default()
            .with_ssid(SSID)
            .with_password(PASSWORD.into()),
    );
    let (controller, interfaces) = esp_radio::wifi::new(
        peripherals.WIFI,
        ControllerConfig::default().with_initial_config(station),
    )
    .expect("wifi new");
    let wifi_iface = interfaces.station;

    let net_config = embassy_net::Config::dhcpv4(Default::default());
    let rng = esp_hal::rng::Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    let (stack, runner) = embassy_net::new(
        wifi_iface,
        net_config,
        mk_static!(StackResources<3>, StackResources::<3>::new()),
        seed,
    );

    spawner.spawn(connection(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());

    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        println!("IP: {}", cfg.address);
    }

    let tcp_state = mk_static!(
        TcpClientState<1, 1500, 1500>,
        TcpClientState::<1, 1500, 1500>::new()
    );
    let tcp_client = TcpClient::new(stack, tcp_state);
    let dns_client = DnsSocket::new(stack);

    // ---- main loop ----
    let mut consecutive_err: u32 = 0;
    loop {
        Timer::after(Duration::from_secs(5)).await;

        let sample = if have_sensor {
            poll_scd41(&mut scd_i2c, &delay)
        } else {
            Sample::Error("no sensor")
        };

        let mut l1: HString<24> = HString::new();
        let mut l2: HString<24> = HString::new();
        let mut l3: HString<24> = HString::new();

        match sample {
            Sample::Ok { co2, temp_c, hum_pct } => {
                consecutive_err = 0;
                println!(
                    "CO2: {} ppm  |  T: {:.2} C  |  H: {:.2} %",
                    co2, temp_c, hum_pct
                );

                let _ = write!(l1, "CO2 {} ppm", co2);
                let _ = write!(l2, "T {:.1}C H {:.0}%", temp_c, hum_pct);

                // Line Protocol: <measurement>,<tags> co2=<i>,temp=<f>,humid=<f>
                let mut line: HString<192> = HString::new();
                let _ = write!(
                    line,
                    "{},{} co2={}i,temp={:.2},humid={:.2}",
                    MEASUREMENT, TAGS, co2, temp_c, hum_pct
                );
                match post_influx(stack, &tcp_client, &dns_client, &line).await {
                    Ok(status) => {
                        println!("InfluxDB POST -> {}", status);
                        let _ = write!(l3, "InfluxDB: {}", status);
                    }
                    Err(_) => {
                        let _ = write!(l3, "InfluxDB: err");
                    }
                }
            }
            Sample::NotReady => {
                println!("SCD41 not ready");
                let _ = write!(l1, "SCD41 wait");
                let _ = write!(l2, "warming up");
                let _ = write!(l3, "");
            }
            Sample::Error(stage) => {
                consecutive_err += 1;
                println!("SCD41 error ({}) streak={}", stage, consecutive_err);
                let _ = write!(l1, "SCD41 ERR");
                let _ = write!(l2, "@{} x{}", stage, consecutive_err);
                let _ = write!(l3, "");

                if consecutive_err >= 3 {
                    println!("SCD41 re-init attempt");
                    have_sensor = start_periodic(&mut scd_i2c, &delay);
                    println!("re-init: {}", if have_sensor { "OK" } else { "FAIL" });
                    consecutive_err = 0;
                }
            }
        }

        render(&mut display, &big, &l1, &l2, &l3);
    }
}
