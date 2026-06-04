//! Presence monitor: detects whether known devices are on the home WiFi by
//! periodically probing their IPs and consulting the ARP/neighbor cache.
//!
//! Why this exists: companion board to the SCD41 monitor whose I2C/3V3 rails
//! are suspect — runs network-only so no external peripherals are needed.
//!
//! Required env vars at build time (loaded by the workspace Makefile from .env):
//!   WIFI_SSID, WIFI_PASSWORD
//!
//! TODO(scan): wire up the actual presence probe — either ICMP echo, a short
//! TCP SYN, or an mDNS query — against a configured list of target hosts,
//! then publish state via MQTT/InfluxDB/ESPNow.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_net::{Runner, StackResources};
use embassy_time::{Duration, Timer};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    interrupt::software::SoftwareInterruptControl,
    timer::timg::TimerGroup,
};
use esp_println::println;
use esp_radio::wifi::{
    sta::StationConfig, Config as WifiCfg, ControllerConfig, Interface as WifiInterface,
    WifiController,
};

esp_bootloader_esp_idf::esp_app_desc!();

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASSWORD");

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

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

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(size: 100 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    println!("=== presence-monitor — WiFi-only (no I2C, no display) ===");

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
        println!("IP: {}  gateway: {:?}", cfg.address, cfg.gateway);
    }

    loop {
        // TODO(scan): probe target hosts, consult ARP cache, publish state.
        println!("presence-monitor heartbeat (scan stub)");
        Timer::after(Duration::from_secs(10)).await;
    }
}
