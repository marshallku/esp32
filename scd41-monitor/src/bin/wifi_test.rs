//! Phase 1+2: connect to WiFi via DHCP, then POST a fake counter to InfluxDB
//! every 5 s. Confirms credentials, route to the InfluxDB host, and v2 API
//! request shape before we wire SCD41 measurements in.

#![no_std]
#![no_main]

use core::fmt::Write as _;

use embassy_executor::Spawner;
use embassy_net::{
    dns::DnsSocket,
    tcp::client::{TcpClient, TcpClientState},
    Runner, StackResources,
};
use embassy_time::{Duration, Timer};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock, interrupt::software::SoftwareInterruptControl, timer::timg::TimerGroup,
};
use esp_println::println;
use esp_radio::wifi::{
    sta::StationConfig, Config, ControllerConfig, Interface, WifiController,
};
use heapless::String as HString;
use reqwless::{
    client::HttpClient,
    request::{Method, RequestBuilder},
};

esp_bootloader_esp_idf::esp_app_desc!();

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASSWORD");

// InfluxDB v2 write endpoint built at compile time.
const INFLUX_URL: &str = concat!(
    env!("INFLUX_URL"),
    "/api/v2/write?org=",
    env!("INFLUX_ORG"),
    "&bucket=",
    env!("INFLUX_BUCKET"),
    "&precision=s",
);
const AUTH: &str = concat!("Token ", env!("INFLUX_TOKEN"));

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(size: 100 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    println!("WiFi: connecting to '{}'", SSID);

    let station = Config::Station(
        StationConfig::default()
            .with_ssid(SSID)
            .with_password(PASSWORD.into()),
    );

    let (controller, interfaces) = esp_radio::wifi::new(
        peripherals.WIFI,
        ControllerConfig::default().with_initial_config(station),
    )
    .expect("wifi new");
    let wifi_interface = interfaces.station;

    let net_config = embassy_net::Config::dhcpv4(Default::default());
    let rng = esp_hal::rng::Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    let (stack, runner) = embassy_net::new(
        wifi_interface,
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

    // ---- HTTP client setup ----
    let tcp_state = mk_static!(
        TcpClientState<1, 1500, 1500>,
        TcpClientState::<1, 1500, 1500>::new()
    );
    let tcp_client = TcpClient::new(stack, tcp_state);
    let dns_client = DnsSocket::new(stack);

    let mut counter: u32 = 0;
    loop {
        counter = counter.wrapping_add(1);

        // Build a single Line Protocol record. No timestamp ⇒ server time.
        let mut line: HString<128> = HString::new();
        let _ = write!(line, "heartbeat,host=esp32s3 value={}u", counter);

        let mut http = HttpClient::new(&tcp_client, &dns_client);
        let mut rx_buf = [0u8; 1024];

        match http.request(Method::POST, INFLUX_URL).await {
            Ok(builder) => {
                let mut req = builder
                    .headers(&[
                        ("Authorization", AUTH),
                        ("Content-Type", "text/plain; charset=utf-8"),
                    ])
                    .body(line.as_bytes());
                match req.send(&mut rx_buf).await {
                    Ok(resp) => println!("POST #{} -> {}", counter, resp.status.0),
                    Err(e) => println!("send error #{}: {:?}", counter, e),
                }
            }
            Err(e) => println!("request error #{}: {:?}", counter, e),
        }

        Timer::after(Duration::from_secs(5)).await;
    }
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) -> ! {
    println!("connection task start");
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
async fn net_task(mut runner: Runner<'static, Interface<'static>>) -> ! {
    runner.run().await
}
