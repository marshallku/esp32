//! Bit-banged SPI transport for the panel.
//!
//! # Why this exists instead of `esp_hal::spi::master::Spi`
//!
//! The SPI peripheral does not drive this panel. Bit-banging the *same*
//! driver — same init table, same CS/DC framing, same framebuffer — draws
//! correctly on the same pins, so the fault is isolated to the peripheral
//! path. What was ruled out along the way, on hardware:
//!
//! - **The panel and wiring.** Waveshare's factory firmware, reflashed from
//!   `03_Firmware/01_Factory_V1.bin`, draws fine.
//! - **The init sequence.** Compared byte for byte against the vendor BSP's
//!   `RLCD_Init`: 28 steps, identical commands, parameters and delays.
//! - **The framebuffer packing.** Verified as an exact bijection against the
//!   vendor's `RLCD_SetLandscapePixel` over all 120,000 pixels.
//! - **CS framing.** Tried both continuous CS across command+parameters and
//!   the vendor's one-transaction-per-byte framing. Neither helped over the
//!   peripheral; the latter is what this module is used with.
//! - **Peripheral choice.** SPI2 and SPI3 both silent.
//! - **Clock rate and mode.** Swept 400 kHz / 1 / 4 / 10 MHz across modes 0
//!   and 3. All silent — and the panel visibly retained the last bit-banged
//!   image throughout, which is what proved no data was arriving.
//! - **Bit order and `Config` defaults.** MSB-first, mode 0, as required.
//!
//! That leaves something in how esp-hal 1.1 routes or drives this peripheral
//! here, which is unresolved rather than explained. Bit-banging is not a
//! workaround chosen over a fix — it is the transport that demonstrably works,
//! and the cost is one that this application cannot feel.
//!
//! # Cost
//!
//! A full frame is 15,000 bytes and the screen is redrawn once per refresh,
//! 30 s apart. Even a slow bit-bang finishes in well under a second, on a core
//! that has nothing else to do. There is no partial update and no tearing to
//! manage, so the only thing spent is idle time.

use embedded_hal::spi::{ErrorType, SpiBus};
use esp_hal::gpio::Output;

/// MSB-first SPI mode 0 over two GPIOs: data is placed while the clock is low
/// and the panel samples it on the rising edge.
///
/// Write-only. MISO is not wired on this board, so the reading half of
/// `SpiBus` returns 0xFF — what an undriven line reads as — rather than
/// leaving the caller's buffer untouched and reporting success.
pub struct BitBangSpi {
    sck: Output<'static>,
    mosi: Output<'static>,
}

impl BitBangSpi {
    /// Takes the clock and data pins. Both must already be push-pull outputs;
    /// the clock is left low, which is mode 0's idle state.
    pub fn new(sck: Output<'static>, mosi: Output<'static>) -> Self {
        let mut this = Self { sck, mosi };
        this.sck.set_low();
        this
    }
}

impl ErrorType for BitBangSpi {
    /// Driving a GPIO cannot fail, so neither can this transport.
    type Error = core::convert::Infallible;
}

impl SpiBus<u8> for BitBangSpi {
    fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
        for byte in words {
            for bit in (0..8).rev() {
                self.sck.set_low();
                if (byte >> bit) & 1 == 1 {
                    self.mosi.set_high();
                } else {
                    self.mosi.set_low();
                }
                self.sck.set_high();
            }
        }
        // Leave the clock idle low so the next transaction starts from mode
        // 0's resting state rather than mid-bit.
        self.sck.set_low();
        Ok(())
    }

    // MISO is not wired on this board, so there is no data to return. Rather
    // than leave the caller's buffer untouched and call that success — which
    // would hand back whatever it happened to contain — these fill with 0xFF,
    // the value an undriven line reads as. Nothing here calls them; they exist
    // so that reuse elsewhere fails obviously instead of subtly.
    fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        words.fill(0xFF);
        Ok(())
    }

    fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
        self.write(write)?;
        read.fill(0xFF);
        Ok(())
    }

    fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        for word in words.iter() {
            self.write(&[*word])?;
        }
        words.fill(0xFF);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
