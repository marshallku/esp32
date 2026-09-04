//! WiFi association and fetching the status document.
//!
//! Every wait here is bounded. `reqwless` over `embassy-net` will otherwise
//! sit forever on a half-open connection, and a wedged fetch is worse than a
//! failed one — it stops the refresh loop before the failure path can put
//! anything on the screen.

use embassy_net::{
    Runner,
    dns::DnsSocket,
    tcp::client::TcpClient,
};
use embassy_time::{Duration, Timer};
use esp_println::println;
use esp_radio::wifi::{Interface as WifiInterface, WifiController};
use reqwless::{
    client::HttpClient,
    request::{Method, RequestBuilder},
};

use crate::model::Status;

/// Shared secret for the aggregator. The document names internal hosts and
/// ports — a subset of what Uptime Kuma keeps behind a login — so the endpoint
/// is not left open on the LAN. Same shape as `scd41-monitor`'s InfluxDB token.
const AUTH: &str = concat!("Bearer ", env!("STATUS_TOKEN"));
const STATUS_URL: &str = env!("STATUS_URL");

/// Hard ceiling on one fetch, so a silent peer cannot stall the refresh loop.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
/// Per-socket timeout, so a peer that accepts and then goes quiet cannot pin
/// the single TCP socket this client owns.
pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

/// The document is ~1 KB today. Sized for growth, and an overrun is reported
/// rather than silently truncated into a parse error.
pub const BODY_MAX: usize = 4096;

/// The aggregator's URL, for logging it at startup.
pub const fn status_url() -> &'static str {
    STATUS_URL
}

/// Keeps the station associated, reconnecting after a drop.
#[embassy_executor::task]
pub async fn connection(mut controller: WifiController<'static>) -> ! {
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

/// Drives the network stack.
#[embassy_executor::task]
pub async fn net_task(mut runner: Runner<'static, WifiInterface<'static>>) -> ! {
    runner.run().await
}

/// Why a fetch did not produce a document.
///
/// These are rendered on the panel verbatim, so the messages are written to be
/// read from a shelf rather than logged. The distinctions are the ones that
/// need different fixes: reflash the firmware, raise a buffer, or go look at
/// the network.
#[derive(Clone, Copy, Debug)]
pub enum FetchError {
    Timeout,
    Request,
    NoResponse,
    Unauthorized,
    Status,
    TooLarge,
    Body,
    Parse,
}

impl FetchError {
    /// One line of plain English for the panel.
    pub fn detail(self) -> &'static str {
        match self {
            FetchError::Timeout => "pi01 did not answer in time",
            FetchError::Request => "could not open connection to pi01",
            FetchError::NoResponse => "no response from homelab-status",
            FetchError::Unauthorized => "STATUS_TOKEN rejected - rebuild firmware",
            FetchError::Status => "homelab-status returned an error",
            FetchError::TooLarge => "response outgrew the parse buffer",
            FetchError::Body => "response body could not be read",
            FetchError::Parse => "response did not match expected shape",
        }
    }
}

/// GET the status document and parse it.
pub async fn fetch_status(
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
        FetchError::NoResponse
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
