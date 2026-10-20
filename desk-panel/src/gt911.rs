//! GT911 capacitive touch controller, polled over I²C.
//!
//! The board wires the GT911's INT and RESET lines nowhere the S3 can reach,
//! so there is no interrupt to wait on: the controller is polled, and its
//! address is whatever it latched at power-up (0x5D on this board, found by
//! the bring-up scan). Polling at 50–100 Hz is well inside what it reports at.
//!
//! Registers are 16-bit, big-endian on the wire.

use embedded_hal::i2c::I2c;

pub const ADDR: u8 = 0x5D;

const PRODUCT_ID: u16 = 0x8140;
const STATUS: u16 = 0x814E;
const POINT1: u16 = 0x8150;

/// `STATUS` bit 7: a fresh report is in the buffer.
const BUFFER_READY: u8 = 0x80;
/// `STATUS` bits 0–3: how many fingers that report holds.
const COUNT_MASK: u8 = 0x0F;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: u16,
    pub y: u16,
}

pub struct Gt911<I> {
    i2c: I,
}

impl<I: I2c> Gt911<I> {
    pub fn new(i2c: I) -> Self {
        Self { i2c }
    }

    /// The four-byte product ID, "911\0" on a genuine part. Used at boot to
    /// tell "touch is wired and alive" apart from "an I²C device answered".
    pub fn product_id(&mut self) -> Result<[u8; 4], I::Error> {
        let mut id = [0u8; 4];
        self.read(PRODUCT_ID, &mut id)?;
        Ok(id)
    }

    /// The latest report, if the controller has a new one.
    ///
    /// - `Ok(None)`: nothing new since the last poll — keep whatever touch
    ///   state you had
    /// - `Ok(Some(None))`: a fresh report with no finger down — a release
    /// - `Ok(Some(Some(p)))`: a fresh report, first finger at `p`
    ///
    /// Only the first finger is read; the UI is single-touch by design.
    pub fn poll(&mut self) -> Result<Option<Option<Point>>, I::Error> {
        let mut status = [0u8; 1];
        self.read(STATUS, &mut status)?;
        if status[0] & BUFFER_READY == 0 {
            return Ok(None);
        }

        let touch = if status[0] & COUNT_MASK > 0 {
            let mut raw = [0u8; 4];
            self.read(POINT1, &mut raw)?;
            Some(Point { x: u16::from_le_bytes([raw[0], raw[1]]), y: u16::from_le_bytes([raw[2], raw[3]]) })
        } else {
            None
        };

        // The controller will not latch another report until the host clears
        // the ready flag, so this is what keeps the reports coming.
        let [hi, lo] = STATUS.to_be_bytes();
        self.i2c.write(ADDR, &[hi, lo, 0])?;
        Ok(Some(touch))
    }

    fn read(&mut self, reg: u16, buf: &mut [u8]) -> Result<(), I::Error> {
        self.i2c.write_read(ADDR, &reg.to_be_bytes(), buf)
    }
}
