//! The serial link to deskd: lines in from UART0's RX, protocol lines out
//! through esp-println.
//!
//! TX stays with esp-println so logs and protocol lines share one writer and
//! never interleave mid-line; the host tells them apart by the `@` prefix.
//! RX is a separate esp-hal driver, run on a higher-priority interrupt
//! executor: the UI task spends ~100 ms at a time drawing, and the UART's
//! 128-byte hardware FIFO would overflow behind it at 115200 baud.

use alloc::string::String;

use desk_core::protocol::{self, DeviceMsg, MAX_LINE};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use esp_hal::{
    peripherals::{GPIO44, UART0},
    uart::{Config, UartRx},
};
use esp_println::{print, println};

/// Complete lines waiting for the UI task. Stop-and-wait means the host has
/// at most one line in flight, so this only ever holds one or two.
pub static LINES: Channel<CriticalSectionRawMutex, Line, 4> = Channel::new();

pub enum Line {
    Text(String),
    /// A line that overran `MAX_LINE` and was discarded up to its newline.
    Overlong,
}

pub fn send(msg: &DeviceMsg) {
    print!("{}", protocol::encode_device(msg));
}

/// Takes the raw peripherals rather than a driver: an async UART driver is
/// not `Send`, and this task is spawned onto another executor, so the driver
/// is built inside it.
#[embassy_executor::task]
pub async fn rx_task(uart: UART0<'static>, pin: GPIO44<'static>) -> ! {
    // Default config is 115200 8N1, matching what esp-println and the ROM
    // already use on the shared TX side.
    let mut rx = UartRx::new(uart, Config::default()).expect("UART0 RX config").with_rx(pin).into_async();
    let mut line: heapless::Vec<u8, MAX_LINE> = heapless::Vec::new();
    let mut discarding = false;
    let mut buf = [0u8; 128];

    loop {
        let n = match rx.read_async(&mut buf).await {
            Ok(n) => n,
            Err(e) => {
                // A framing or FIFO error corrupts whatever line was in
                // progress. Drop it; the host times out the missing ack and
                // resends.
                println!("uart rx error: {:?}", e);
                line.clear();
                discarding = true;
                continue;
            }
        };

        for &b in &buf[..n] {
            if b == b'\n' {
                if discarding {
                    LINES.send(Line::Overlong).await;
                } else if let Ok(s) = core::str::from_utf8(&line) {
                    LINES.send(Line::Text(String::from(s))).await;
                }
                line.clear();
                discarding = false;
            } else if !discarding && line.push(b).is_err() {
                line.clear();
                discarding = true;
            }
        }
    }
}
