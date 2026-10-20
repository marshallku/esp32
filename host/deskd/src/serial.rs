//! Finding and opening the panel's CH340 without rebooting it.
//!
//! The board's auto-reset circuit pulls the ESP32's EN low from the CH340's
//! DTR/RTS lines. Linux drops both on close when HUPCL is set, and raises them
//! again on the next open — an edge pair that reboots the board every time
//! deskd restarts. Clearing HUPCL right after opening leaves the lines
//! asserted across closes, so only the very first open after the board
//! appears (plug-in, or an espflash run) still resets it. Measured on this
//! board: a reopen with HUPCL cleared produces no boot banner.

use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use nix::sys::termios::{self, BaudRate, ControlFlags, SetArg, SpecialCharacterIndices};
use tokio::sync::mpsc;

/// The CH340 on this board, as udev names it.
const BY_ID: &str = "/dev/serial/by-id";
const CH340: &str = "usb-1a86_USB_Serial";

/// Longest partial line kept while waiting for its newline. Well above the
/// protocol's `MAX_LINE`; anything longer is noise and gets cut.
const MAX_PENDING: usize = 8192;

pub fn find() -> Option<PathBuf> {
    std::fs::read_dir(BY_ID)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(CH340)))
}

/// Opens `path` raw at 115200 8N1 and returns the writing end plus a channel
/// of complete lines. The channel closes when the port does — unplugging the
/// board ends the session.
pub fn open(path: &Path) -> Result<(File, mpsc::Receiver<String>)> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_NOCTTY)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;

    let mut tio = termios::tcgetattr(&file)?;
    termios::cfmakeraw(&mut tio);
    termios::cfsetspeed(&mut tio, BaudRate::B115200)?;
    tio.control_flags.insert(ControlFlags::CLOCAL | ControlFlags::CREAD);
    tio.control_flags.remove(ControlFlags::HUPCL);
    // Reads return after at most 200 ms even with nothing to read, so the
    // reader thread gets to notice that its session has gone away.
    tio.control_chars[SpecialCharacterIndices::VMIN as usize] = 0;
    tio.control_chars[SpecialCharacterIndices::VTIME as usize] = 2;
    termios::tcsetattr(&file, SetArg::TCSANOW, &tio)?;

    let reader = file.try_clone()?;
    let (tx, rx) = mpsc::channel(64);
    std::thread::spawn(move || read_lines(reader, tx));
    Ok((file, rx))
}

/// Splits the port's bytes into lines until the session drops `tx`.
///
/// A blocking `read` cannot be interrupted from another thread, and this
/// thread holds its own handle to the port. If it outlived its session it
/// would keep reading after the next session reopened the port — and swallow
/// that session's handshake. The 200 ms read timeout bounds how long it can
/// linger; `session::run` waits longer than that before reopening.
fn read_lines(mut port: File, tx: mpsc::Sender<String>) {
    let mut buf = [0u8; 512];
    let mut line = Vec::new();
    while !tx.is_closed() {
        let n = match port.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // The board was unplugged.
            Err(_) => return,
        };
        for &b in &buf[..n] {
            if b != b'\n' {
                // Bounded: boot noise or a wrong baud rate can produce bytes
                // with no newline for a long time.
                if line.len() < MAX_PENDING {
                    line.push(b);
                }
                continue;
            }
            let text = String::from_utf8_lossy(&line).trim_end_matches('\r').to_string();
            line.clear();
            if tx.blocking_send(text).is_err() {
                return;
            }
        }
    }
}
