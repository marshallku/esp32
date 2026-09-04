//! ST7305 reflective-LCD driver for the Waveshare ESP32-S3-RLCD-4.2.
//!
//! The panel is 300x400 native (portrait), driven here as **400x300
//! landscape**, 1 bit per pixel. Despite looking like e-paper it is an LCD:
//! a full frame goes out in tens of milliseconds with no ghosting and no
//! full-refresh ritual, so there is no partial-update machinery here — every
//! `flush` ships the whole 15 000-byte buffer.
//!
//! Wiring is fixed by the board (see `main.rs` for the pin constants); this
//! module only cares that it is handed an SPI bus plus DC/CS/RST outputs.
//!
//! ## Framebuffer layout
//!
//! One byte holds a 2(x) x 4(y) block of pixels, and the panel's rows run
//! bottom-up relative to the landscape origin, so the mapping is not the usual
//! row-major packing:
//!
//! ```text
//! inv_y   = 299 - y
//! index   = (x / 2) * 75 + inv_y / 4
//! bit     = 7 - ((inv_y % 4) * 2 + (x % 2))
//! ```
//!
//! Ported from the vendor BSP (`display_bsp.cpp`, `RLCD_SetLandscapePixel`).
//! Getting this wrong does not fail loudly — it draws a scrambled but
//! plausible-looking image — so treat it as load-bearing.

use embedded_graphics::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Size},
    pixelcolor::BinaryColor,
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, spi::SpiBus};

/// Landscape width, in pixels.
pub const WIDTH: u32 = 400;
/// Landscape height, in pixels.
pub const HEIGHT: u32 = 300;
/// Framebuffer size: 400 * 300 / 8.
pub const BUF_LEN: usize = (WIDTH * HEIGHT / 8) as usize;

const BLOCKS_PER_COLUMN: usize = HEIGHT as usize / 4; // 75

/// A set bit lights the pixel as *white* in the vendor's framebuffer
/// convention, so `BinaryColor::On` (ink) has to clear it. The init sequence
/// enables display inversion (`0x21`), which is what makes this the right way
/// round; if a future init drops that command, flip this and nothing else.
const INK_CLEARS_BIT: bool = true;

/// ST7305 command set, as used by the vendor initialisation.
mod cmd {
    pub const SLEEP_OUT: u8 = 0x11;
    pub const INVERSION_ON: u8 = 0x21;
    pub const DISPLAY_ON: u8 = 0x29;
    pub const COLUMN_ADDRESS_SET: u8 = 0x2A;
    pub const ROW_ADDRESS_SET: u8 = 0x2B;
    pub const MEMORY_WRITE: u8 = 0x2C;
    pub const TEARING_EFFECT_ON: u8 = 0x35;
    pub const MEMORY_DATA_ACCESS: u8 = 0x36;
    pub const HIGH_POWER_MODE: u8 = 0x38;
    pub const PIXEL_FORMAT: u8 = 0x3A;
    pub const SOURCE_TIMING: u8 = 0x62;
    pub const DUTY_CYCLE: u8 = 0xB0;
    pub const FRAME_RATE: u8 = 0xB2;
    pub const UPDATE_PERIOD_HPM: u8 = 0xB3;
    pub const UPDATE_PERIOD_LPM: u8 = 0xB4;
    pub const GATE_EQ: u8 = 0xB7;
    pub const SOURCE_EQ: u8 = 0xB8;
    pub const GAMMA_MODE: u8 = 0xB9;
    pub const GATE_VOLTAGE: u8 = 0xC0;
    pub const VSHP: u8 = 0xC1;
    pub const VSLP: u8 = 0xC2;
    pub const VSHN: u8 = 0xC4;
    pub const VSLN: u8 = 0xC5;
    pub const OSC_ENABLE: u8 = 0xC9;
    pub const BOOSTER_ENABLE: u8 = 0xD1;
    pub const NVM_LOAD: u8 = 0xD6;
    pub const AUTO_POWER: u8 = 0xD0;
    pub const VCOM: u8 = 0xD8;
}

/// The window the vendor writes into: 25 column units x 200 page rows, which
/// is exactly the 15 000-byte buffer.
const COL_START: u8 = 0x12;
const COL_END: u8 = 0x2A;
const ROW_START: u8 = 0x00;
const ROW_END: u8 = 0xC7;

/// One `(command, params)` pair of the power-on sequence.
///
/// Values are transcribed from the vendor BSP. They configure the charge pump,
/// gate/source voltages and the update waveform; the datasheet does not
/// document usable defaults for this panel, so they are not derivable and must
/// not be "tidied".
const INIT: &[(u8, &[u8])] = &[
    (cmd::NVM_LOAD, &[0x17, 0x02]),
    (cmd::BOOSTER_ENABLE, &[0x01]),
    (cmd::GATE_VOLTAGE, &[0x11, 0x04]),
    (cmd::VSHP, &[0x41, 0x41, 0x41, 0x41]),
    (cmd::VSLP, &[0x19, 0x19, 0x19, 0x19]),
    (cmd::VSHN, &[0x41, 0x41, 0x41, 0x41]),
    (cmd::VSLN, &[0x19, 0x19, 0x19, 0x19]),
    (cmd::VCOM, &[0xA6, 0xE9]),
    (cmd::FRAME_RATE, &[0x05]),
    (
        cmd::UPDATE_PERIOD_HPM,
        &[0xE5, 0xF6, 0x05, 0x46, 0x77, 0x77, 0x77, 0x77, 0x76, 0x45],
    ),
    (
        cmd::UPDATE_PERIOD_LPM,
        &[0x05, 0x46, 0x77, 0x77, 0x77, 0x77, 0x76, 0x45],
    ),
    (cmd::SOURCE_TIMING, &[0x32, 0x03, 0x1F]),
    (cmd::GATE_EQ, &[0x13]),
    (cmd::DUTY_CYCLE, &[0x64]),
];

/// Init steps that follow the 200 ms sleep-out delay.
const INIT_AFTER_WAKE: &[(u8, &[u8])] = &[
    (cmd::OSC_ENABLE, &[0x00]),
    (cmd::MEMORY_DATA_ACCESS, &[0x48]),
    (cmd::PIXEL_FORMAT, &[0x11]),
    (cmd::GAMMA_MODE, &[0x20]),
    (cmd::SOURCE_EQ, &[0x29]),
    (cmd::INVERSION_ON, &[]),
    (cmd::COLUMN_ADDRESS_SET, &[COL_START, COL_END]),
    (cmd::ROW_ADDRESS_SET, &[ROW_START, ROW_END]),
    (cmd::TEARING_EFFECT_ON, &[0x00]),
    (cmd::AUTO_POWER, &[0xFF]),
    (cmd::HIGH_POWER_MODE, &[]),
    (cmd::DISPLAY_ON, &[]),
];

/// Errors the driver can surface. SPI and GPIO failures are collapsed because
/// neither is recoverable in this application — both mean the panel is not
/// wired the way the board says it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The SPI bus rejected a transfer.
    Spi,
    /// A control line (DC, CS or RST) could not be driven.
    Gpio,
}

/// Driver over an SPI bus plus the three control lines.
///
/// The 15 KB framebuffer lives inline, so a `St7305` must not be created on a
/// task stack — build it into a `static` (see `mk_static!` in `main.rs`).
pub struct St7305<SPI, DC, CS, RST> {
    spi: SPI,
    dc: DC,
    cs: CS,
    rst: RST,
    buf: [u8; BUF_LEN],
}

impl<SPI, DC, CS, RST> St7305<SPI, DC, CS, RST>
where
    SPI: SpiBus<u8>,
    DC: OutputPin,
    CS: OutputPin,
    RST: OutputPin,
{
    /// Wrap the bus and pins. Does not touch the panel — call [`Self::init`].
    pub fn new(spi: SPI, dc: DC, cs: CS, rst: RST) -> Self {
        Self {
            spi,
            dc,
            cs,
            rst,
            // All bits set = all white, matching a cleared screen.
            buf: [0xFF; BUF_LEN],
        }
    }

    /// One SPI transaction: assert CS, send `bytes` at the given DC level,
    /// release CS.
    ///
    /// CS is released on every path, including errors. Returning early with
    /// the panel still selected would leave it mid-transaction, and since
    /// `main` carries on after an init failure, every later command would be
    /// interpreted against that undefined state rather than simply failing.
    fn transaction(&mut self, data_phase: bool, bytes: &[u8]) -> Result<(), Error> {
        self.cs.set_low().map_err(|_| Error::Gpio)?;
        let result = (|| {
            if data_phase {
                self.dc.set_high().map_err(|_| Error::Gpio)?;
            } else {
                self.dc.set_low().map_err(|_| Error::Gpio)?;
            }
            self.spi.write(bytes).map_err(|_| Error::Spi)
        })();
        let released = self.cs.set_high().map_err(|_| Error::Gpio);
        result.and(released)
    }

    /// Send a command, then its parameters **one byte per transaction**.
    ///
    /// This framing is load-bearing, not stylistic. The vendor BSP goes
    /// through `esp_lcd_panel_io_spi`, which raises CS between every
    /// `tx_param` call — so the panel is used to seeing each parameter byte
    /// framed on its own. Holding CS low across the command and all of its
    /// parameters, which is the ordinary 4-wire SPI pattern and what this
    /// driver did first, produced a panel that accepted everything in silence
    /// and displayed nothing at all.
    fn command(&mut self, command: u8, params: &[u8]) -> Result<(), Error> {
        self.transaction(false, &[command])?;
        for byte in params {
            self.transaction(true, &[*byte])?;
        }
        Ok(())
    }

    /// Hardware reset, power-on sequence, blank screen, display on.
    pub fn init<D: DelayNs>(&mut self, delay: &mut D) -> Result<(), Error> {
        self.cs.set_high().map_err(|_| Error::Gpio)?;
        self.rst.set_high().map_err(|_| Error::Gpio)?;
        delay.delay_ms(50);
        self.rst.set_low().map_err(|_| Error::Gpio)?;
        delay.delay_ms(20);
        self.rst.set_high().map_err(|_| Error::Gpio)?;
        delay.delay_ms(50);

        for (command, params) in INIT {
            self.command(*command, params)?;
        }

        self.command(cmd::SLEEP_OUT, &[])?;
        // The charge pump needs this before any of the voltage settings above
        // take effect; skipping it yields a panel that accepts data and shows
        // nothing.
        delay.delay_ms(200);

        for (command, params) in INIT_AFTER_WAKE {
            self.command(*command, params)?;
        }

        self.clear(BinaryColor::Off);
        self.flush()
    }

    /// Fill the framebuffer without touching the panel.
    pub fn clear(&mut self, color: BinaryColor) {
        let byte = if is_ink(color) { 0x00 } else { 0xFF };
        self.buf.fill(byte);
    }

    /// Ship the whole framebuffer.
    pub fn flush(&mut self) -> Result<(), Error> {
        self.command(cmd::COLUMN_ADDRESS_SET, &[COL_START, COL_END])?;
        self.command(cmd::ROW_ADDRESS_SET, &[ROW_START, ROW_END])?;

        // Memory-write is its own transaction, then the frame is one more —
        // matching the vendor's `tx_param` then `tx_color` pair. The frame
        // itself does go out under a single CS assertion; it is one continuous
        // data phase, not a sequence of framed parameters.
        self.transaction(false, &[cmd::MEMORY_WRITE])?;

        self.cs.set_low().map_err(|_| Error::Gpio)?;
        let result = (|| {
            self.dc.set_high().map_err(|_| Error::Gpio)?;
            self.spi.write(&self.buf).map_err(|_| Error::Spi)
        })();
        let released = self.cs.set_high().map_err(|_| Error::Gpio);
        result.and(released)
    }

    /// The underlying bus, for reconfiguring clock rate or mode after
    /// construction. Reinitialise the panel after changing either.
    pub fn bus(&mut self) -> &mut SPI {
        &mut self.spi
    }

    fn set_pixel(&mut self, x: u32, y: u32, color: BinaryColor) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        let inv_y = HEIGHT as usize - 1 - y as usize;
        let index = (x as usize / 2) * BLOCKS_PER_COLUMN + inv_y / 4;
        let bit = 7 - ((inv_y % 4) * 2 + (x as usize % 2));
        let mask = 1u8 << bit;

        if is_ink(color) {
            self.buf[index] &= !mask;
        } else {
            self.buf[index] |= mask;
        }
    }
}

/// Whether this colour should be drawn as ink (dark) rather than background.
fn is_ink(color: BinaryColor) -> bool {
    match color {
        BinaryColor::On => INK_CLEARS_BIT,
        BinaryColor::Off => !INK_CLEARS_BIT,
    }
}

impl<SPI, DC, CS, RST> OriginDimensions for St7305<SPI, DC, CS, RST> {
    fn size(&self) -> Size {
        Size::new(WIDTH, HEIGHT)
    }
}

impl<SPI, DC, CS, RST> DrawTarget for St7305<SPI, DC, CS, RST>
where
    SPI: SpiBus<u8>,
    DC: OutputPin,
    CS: OutputPin,
    RST: OutputPin,
{
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if point.x >= 0 && point.y >= 0 {
                self.set_pixel(point.x as u32, point.y as u32, color);
            }
        }
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        St7305::clear(self, color);
        Ok(())
    }
}
