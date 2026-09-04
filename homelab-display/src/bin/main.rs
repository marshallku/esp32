//! Homelab status display for the Waveshare ESP32-S3-RLCD-4.2.
//!
//! Fetches one pre-aggregated JSON document from the `homelab-status` stack on
//! pi01 and draws it on the 400x300 reflective LCD. All the aggregation lives
//! on the server side (see `marshallku/manifest`,
//! `docker-compose/pi01/homelab-status`) — this end only parses a fixed shape
//! and renders it.
//!
//! Board wiring, from the vendor BSP (`user_config.h`):
//!   - LCD  (bit-banged SPI): SCK=GPIO11 MOSI=GPIO12 DC=GPIO5 CS=GPIO40 RST=GPIO41
//!   - I2C0:        SDA=GPIO13  SCL=GPIO14   — SHTC3, PCF85063, ES8311, ES7210
//!
//! The board has no usable clock of its own here, so the footer shows the
//! wall-clock stamp the server preformats into `generated_at` rather than any
//! locally computed age.
//!
//! Required env vars at build time (loaded by the workspace Makefile from
//! `.env`): `WIFI_SSID`, `WIFI_PASSWORD`, `STATUS_URL`, `STATUS_TOKEN`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_net::{
    StackResources,
    dns::DnsSocket,
    tcp::client::{TcpClient, TcpClientState},
};
use embassy_time::{Duration, Timer, with_timeout};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
    i2c::master::{BusTimeout, Config as I2cConfig, I2c},
    interrupt::software::SoftwareInterruptControl,
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_println::println;
use esp_radio::wifi::{Config as WifiCfg, ControllerConfig, sta::StationConfig};
use homelab_display::{
    bitbang_spi::BitBangSpi,
    net::{self, BODY_MAX, FETCH_TIMEOUT, FetchError, SOCKET_TIMEOUT},
    render, shtc3,
    st7305::St7305,
};

esp_bootloader_esp_idf::esp_app_desc!();

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASSWORD");

/// How often to refetch and redraw. The aggregator rebuilds every 30 s, so
/// anything much faster only adds traffic; the panel itself does not care.
const REFRESH: Duration = Duration::from_secs(30);
/// Backoff after a failed fetch. Short enough that a brief blip self-heals
/// before anyone notices, long enough not to hammer a host that is down.
const RETRY: Duration = Duration::from_secs(10);
/// How long to wait for DHCP before telling the panel about it. Not a failure
/// — the wait simply resumes — but the screen should not sit on a splash
/// indefinitely with no explanation.
const DHCP_REPORT_AFTER: Duration = Duration::from_secs(20);

type Panel = St7305<BitBangSpi, Output<'static>, Output<'static>, Output<'static>>;

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

    println!("=== homelab display — ST7305 400x300 + WiFi ===");
    println!("    source: {}", net::status_url());

    let mut delay = Delay::new();

    // Bit-banged rather than the SPI peripheral: esp-hal's SPI does not drive
    // this panel, and the same driver over bit-banged GPIO does. The evidence
    // and everything ruled out is in `bitbang_spi`.
    let out = OutputConfig::default();
    let spi = BitBangSpi::new(
        Output::new(peripherals.GPIO11, Level::Low, out),
        Output::new(peripherals.GPIO12, Level::Low, out),
    );
    let dc = Output::new(peripherals.GPIO5, Level::Low, out);
    let cs = Output::new(peripherals.GPIO40, Level::High, out);
    let rst = Output::new(peripherals.GPIO41, Level::High, out);

    // 15 KB of framebuffer: too big for a task stack, so it is built straight
    // into a static.
    let panel: &'static mut Panel = mk_static!(Panel, St7305::new(spi, dc, cs, rst));
    match panel.init(&mut delay) {
        Ok(()) => println!("ST7305 init OK"),
        Err(e) => println!("ST7305 init failed: {:?} — continuing headless", e),
    }

    render::message(panel, "HOMELAB", "connecting to WiFi...");
    let _ = panel.flush();

    let i2c_config = I2cConfig::default()
        .with_frequency(Rate::from_khz(100))
        .with_timeout(BusTimeout::Maximum);
    let mut i2c = I2c::new(peripherals.I2C0, i2c_config)
        .expect("I2C0 init")
        .with_sda(peripherals.GPIO13)
        .with_scl(peripherals.GPIO14);

    match shtc3::read(&mut i2c, &delay) {
        Some(room) => println!("SHTC3: {:.1}C {:.0}%", room.temp_c, room.hum_pct),
        None => println!("SHTC3 not responding — header will omit room readings"),
    }

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

    let net_config = embassy_net::Config::dhcpv4(Default::default());
    let rng = esp_hal::rng::Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    let (stack, runner) = embassy_net::new(
        interfaces.station,
        net_config,
        mk_static!(StackResources<3>, StackResources::<3>::new()),
        seed,
    );

    spawner.spawn(net::connection(controller).unwrap());
    spawner.spawn(net::net_task(runner).unwrap());

    // Bounded rather than a bare await: DHCP may never complete (wrong PSK,
    // router down, out of leases), and an unbounded wait here would freeze the
    // panel on the splash screen with nothing to explain it.
    let mut waited = 0u32;
    while with_timeout(DHCP_REPORT_AFTER, stack.wait_config_up())
        .await
        .is_err()
    {
        waited += DHCP_REPORT_AFTER.as_secs() as u32;
        println!("still waiting for DHCP after {}s", waited);
        render::message(panel, "NO NETWORK", "waiting for WiFi and DHCP...");
        let _ = panel.flush();
    }
    if let Some(cfg) = stack.config_v4() {
        println!("IP: {}", cfg.address);
    }

    let tcp_state = mk_static!(
        TcpClientState<1, 1500, 1500>,
        TcpClientState::<1, 1500, 1500>::new()
    );
    let mut tcp_client = TcpClient::new(stack, tcp_state);
    tcp_client.set_timeout(Some(SOCKET_TIMEOUT));
    let dns_client = DnsSocket::new(stack);
    let rx_buf = mk_static!([u8; BODY_MAX], [0u8; BODY_MAX]);

    let mut consecutive_failures: u32 = 0;
    loop {
        let room = shtc3::read(&mut i2c, &delay);

        let outcome = match with_timeout(
            FETCH_TIMEOUT,
            net::fetch_status(&tcp_client, &dns_client, rx_buf),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                println!("fetch timed out after {}s", FETCH_TIMEOUT.as_secs());
                Err(FetchError::Timeout)
            }
        };

        match outcome {
            Ok(status) => {
                consecutive_failures = 0;
                println!(
                    "status: {}/{} up, {} down, age {}s, stale {}",
                    status.kuma.up,
                    status.kuma.total,
                    status.kuma.down.len(),
                    status.age,
                    status.stale,
                );
                render::draw(panel, &status, room);
                if let Err(e) = panel.flush() {
                    println!("panel flush failed: {:?}", e);
                }
                Timer::after(REFRESH).await;
            }
            Err(error) => {
                consecutive_failures += 1;
                println!("fetch failed (streak {})", consecutive_failures);

                // The first failure is left alone: the panel keeps showing
                // the last good screen, which still carries the wall-clock
                // stamp of when it was true. Only a sustained outage is worth
                // overwriting it with — before that, a reader comparing the
                // footer against a clock already has the whole story.
                if consecutive_failures >= 3 {
                    render::message(panel, "NO DATA", error.detail());
                    let _ = panel.flush();
                }
                Timer::after(RETRY).await;
            }
        }
    }
}
