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
    Runner, StackResources,
    dns::DnsSocket,
    tcp::client::{TcpClient, TcpClientState},
};
use embassy_time::{Duration, Timer, with_timeout};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{
    Blocking,
    clock::CpuClock,
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
    i2c::master::{BusTimeout, Config as I2cConfig, I2c},
    interrupt::software::SoftwareInterruptControl,
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_println::println;
use esp_radio::wifi::{
    Config as WifiCfg, ControllerConfig, Interface as WifiInterface, WifiController,
    sta::StationConfig,
};
use homelab_display::{
    bitbang_spi::BitBangSpi,
    model::Status,
    render::{self, Room},
    st7305::St7305,
};
use reqwless::{
    client::HttpClient,
    request::{Method, RequestBuilder},
};

esp_bootloader_esp_idf::esp_app_desc!();

// --- env-injected configuration --------------------------------------------
const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASSWORD");
const STATUS_URL: &str = env!("STATUS_URL");
/// Shared secret for the aggregator. The document names internal hosts and
/// ports — a subset of what Uptime Kuma keeps behind a login — so the endpoint
/// is not left open on the LAN. Same shape as `scd41-monitor`'s InfluxDB token.
const AUTH: &str = concat!("Bearer ", env!("STATUS_TOKEN"));

/// How often to refetch and redraw. The aggregator rebuilds every 30 s, so
/// anything much faster only adds traffic; the panel itself does not care.
const REFRESH: Duration = Duration::from_secs(30);
/// Backoff after a failed fetch. Short enough that a brief blip self-heals
/// before anyone notices, long enough not to hammer a host that is down.
const RETRY: Duration = Duration::from_secs(10);
/// Hard ceiling on one fetch. `reqwless` over `embassy-net` will otherwise
/// wait forever on a half-open connection, and a wedged fetch is worse than a
/// failed one: it stops the loop before the failure path can report anything.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
/// Per-socket timeout, so a peer that accepts and then goes silent cannot pin
/// the single TCP socket this client owns.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for DHCP before telling the panel about it. Not a failure
/// — the wait simply resumes — but the screen should not sit on a splash
/// indefinitely with no explanation.
const DHCP_REPORT_AFTER: Duration = Duration::from_secs(20);

/// The document is ~1 KB today. This is sized for growth, and an overrun is
/// reported rather than silently truncated into a parse error.
const BODY_MAX: usize = 4096;

const SHTC3_ADDR: u8 = 0x70;

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

type Panel = St7305<BitBangSpi, Output<'static>, Output<'static>, Output<'static>>;

// --- SHTC3 ------------------------------------------------------------------

/// Sensirion CRC-8: polynomial 0x31, init 0xFF, no reflection, no final xor.
fn sensirion_crc(data: &[u8]) -> u8 {
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
/// The sensor sleeps between reads, so every measurement is wake -> measure ->
/// sleep. A missing or wrongly-wired sensor just yields `None` and the header
/// omits the reading; it is not worth failing the screen over.
///
/// Both CRC bytes are checked. This bus is shared with the RTC and two audio
/// codecs, so a corrupted read is a real possibility — and an unchecked one
/// would put a plausible, wrong temperature on a screen whose whole purpose is
/// being trustworthy at a glance. Omitting the reading is the safer failure.
fn read_shtc3(i2c: &mut I2c<'_, Blocking>, delay: &Delay) -> Option<Room> {
    i2c.write(SHTC3_ADDR, &[0x35, 0x17]).ok()?; // wake up
    delay.delay_millis(1);
    i2c.write(SHTC3_ADDR, &[0x78, 0x66]).ok()?; // measure T first, no stretch
    delay.delay_millis(15);

    let mut raw = [0u8; 6];
    let read = i2c.read(SHTC3_ADDR, &mut raw);
    let _ = i2c.write(SHTC3_ADDR, &[0xB0, 0x98]); // sleep, regardless
    read.ok()?;

    if sensirion_crc(&raw[0..2]) != raw[2] || sensirion_crc(&raw[3..5]) != raw[5] {
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

// --- fetch ------------------------------------------------------------------

/// Why a fetch did not produce a document. Rendered on the panel verbatim, so
/// the messages are written to be read from a shelf rather than logged.
#[derive(Clone, Copy)]
enum FetchError {
    Timeout,
    Request,
    Unauthorized,
    Send,
    Status,
    TooLarge,
    Body,
    Parse,
}

impl FetchError {
    fn detail(self) -> &'static str {
        match self {
            FetchError::Timeout => "pi01 did not answer in time",
            FetchError::Unauthorized => "STATUS_TOKEN rejected - rebuild firmware",
            FetchError::Request => "could not open connection to pi01",
            FetchError::Send => "no response from homelab-status",
            FetchError::Status => "homelab-status returned an error",
            FetchError::TooLarge => "response outgrew the parse buffer",
            FetchError::Body => "response body could not be read",
            FetchError::Parse => "response did not match expected shape",
        }
    }
}

async fn fetch_status(
    tcp_client: &TcpClient<'static, 1, 1500, 1500>,
    dns_client: &DnsSocket<'static>,
    rx_buf: &mut [u8; BODY_MAX],
) -> Result<Status, FetchError> {
    let mut http = HttpClient::new(tcp_client, dns_client);

    let builder = http.request(Method::GET, STATUS_URL).await.map_err(|e| {
        println!("status request error: {:?}", e);
        FetchError::Request
    })?;
    let mut request = builder.headers(&[
        ("Accept", "application/json"),
        ("Authorization", AUTH),
    ]);

    let response = request.send(rx_buf).await.map_err(|e| {
        println!("status send error: {:?}", e);
        FetchError::Send
    })?;

    let status = response.status;
    if status.0 == 401 || status.0 == 403 {
        println!("status HTTP {} — token rejected", status.0);
        return Err(FetchError::Unauthorized);
    }
    if !status.is_successful() {
        println!("status HTTP {}", status.0);
        return Err(FetchError::Status);
    }

    let body = response.body().read_to_end().await.map_err(|e| {
        println!("status body error: {:?}", e);
        // An outgrown document and a broken connection need different fixes —
        // raise BODY_MAX versus go look at the network — so they get different
        // messages. `read_to_end` reports the first as `BufferTooSmall`.
        match e {
            reqwless::Error::BufferTooSmall => FetchError::TooLarge,
            _ => FetchError::Body,
        }
    })?;

    // A body that exactly fills the buffer is indistinguishable from one that
    // was truncated at the limit, so treat it as oversized too.
    if body.len() >= BODY_MAX {
        println!("status body filled the {}-byte buffer", BODY_MAX);
        return Err(FetchError::TooLarge);
    }

    Status::parse(body).map_err(|e| {
        println!("status parse error: {:?}", e);
        FetchError::Parse
    })
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

    println!("=== homelab display — ST7305 400x300 + WiFi ===");
    println!("    source: {}", STATUS_URL);

    let mut delay = Delay::new();

    // ---- panel ----
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

    // ---- SHTC3 on the board's shared I2C bus ----
    let i2c_config = I2cConfig::default()
        .with_frequency(Rate::from_khz(100))
        .with_timeout(BusTimeout::Maximum);
    let mut i2c = I2c::new(peripherals.I2C0, i2c_config)
        .expect("I2C0 init")
        .with_sda(peripherals.GPIO13)
        .with_scl(peripherals.GPIO14);

    match read_shtc3(&mut i2c, &delay) {
        Some(room) => println!("SHTC3: {:.1}C {:.0}%", room.temp_c, room.hum_pct),
        None => println!("SHTC3 not responding — header will omit room readings"),
    }

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

    let net_config = embassy_net::Config::dhcpv4(Default::default());
    let rng = esp_hal::rng::Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    let (stack, runner) = embassy_net::new(
        interfaces.station,
        net_config,
        mk_static!(StackResources<3>, StackResources::<3>::new()),
        seed,
    );

    spawner.spawn(connection(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());

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

    // ---- main loop ----
    let mut consecutive_failures: u32 = 0;
    loop {
        let room = read_shtc3(&mut i2c, &delay);

        let outcome = match with_timeout(
            FETCH_TIMEOUT,
            fetch_status(&tcp_client, &dns_client, rx_buf),
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
