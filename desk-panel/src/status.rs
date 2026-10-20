//! Keeps the dashboard fed: waits for a lease, fetches the status document on
//! a fixed cadence, and hands every outcome to the UI task.
//!
//! The fetch itself and every bound on it live in `homelab_status::net`,
//! shared with homelab-display; this is only the loop around it.

use alloc::boxed::Box;

use embassy_net::{
    Stack,
    dns::DnsSocket,
    tcp::client::{TcpClient, TcpClientState},
};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Timer, with_timeout};
use homelab_status::net::{self, BODY_MAX, FETCH_TIMEOUT, FetchError, SOCKET_TIMEOUT};
use static_cell::StaticCell;

use crate::dashboard::{Net, Update};

/// The aggregator rebuilds every 30 s; fetching faster only adds traffic.
const REFRESH: Duration = Duration::from_secs(30);
/// Short enough that a blip self-heals unnoticed, long enough not to hammer
/// a host that is down.
const RETRY: Duration = Duration::from_secs(10);
/// How long to wait for DHCP before telling the screen. Not a failure — the
/// wait resumes — but the dashboard should not sit on "connecting" with no
/// explanation.
const DHCP_REPORT_AFTER: Duration = Duration::from_secs(20);

pub static UPDATES: Channel<CriticalSectionRawMutex, Update, 2> = Channel::new();

#[embassy_executor::task]
pub async fn fetch_task(stack: Stack<'static>) -> ! {
    while with_timeout(DHCP_REPORT_AFTER, stack.wait_config_up()).await.is_err() {
        UPDATES.send(Update::Net(Net::NoLease)).await;
    }
    if let Some(cfg) = stack.config_v4() {
        log::info!("IP: {}", cfg.address);
    }
    UPDATES.send(Update::Net(Net::Online)).await;

    static TCP: StaticCell<TcpClientState<1, 1500, 1500>> = StaticCell::new();
    static RX: StaticCell<[u8; BODY_MAX]> = StaticCell::new();
    let mut tcp = TcpClient::new(stack, TCP.init(TcpClientState::new()));
    tcp.set_timeout(Some(SOCKET_TIMEOUT));
    let dns = DnsSocket::new(stack);
    let rx = RX.init([0u8; BODY_MAX]);

    loop {
        let outcome = match with_timeout(FETCH_TIMEOUT, net::fetch_status(&tcp, &dns, rx)).await {
            Ok(result) => result,
            Err(_) => Err(FetchError::Timeout),
        };
        match outcome {
            Ok(status) => {
                log::info!("status: {}/{} up, {} down, stale {}", status.kuma.up, status.kuma.total, status.kuma.down.len(), status.stale);
                UPDATES.send(Update::Fetched(Box::new(status))).await;
                Timer::after(REFRESH).await;
            }
            Err(e) => {
                log::warn!("status fetch failed: {}", e.detail());
                UPDATES.send(Update::Failed(e)).await;
                Timer::after(RETRY).await;
            }
        }
    }
}
