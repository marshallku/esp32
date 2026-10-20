//! ST7701S bring-up over its 3-wire SPI port.
//!
//! The ST7701S has two interfaces and this board uses both: pixels arrive over
//! the 16-bit RGB bus (`lcd_cam` DPI), but the controller only starts listening
//! there after this register table has been written over a separate 9-bit SPI
//! link. Nothing here touches pixels.
//!
//! The link is bit-banged rather than run on the SPI peripheral. Each word is
//! 9 bits — a D/C flag followed by the byte — which the peripheral can only do
//! through its command/address phases, and the whole table is a few hundred
//! words sent once at boot. Bit-banging also sidesteps whatever kept
//! `esp_hal::spi::master::Spi` from reaching the ST7305 in homelab-display.
//!
//! # Where the table comes from
//!
//! `INIT` is ESPHome's `ST7701S_1_INIT` (esphome 2025.9.0,
//! `components/st7701s/init_sequences.py`), extracted from that file's AST
//! rather than retyped, followed by the two board-specific commands from
//! ESPHome's Guition ESP32-S3-4848S040 device page. Every multi-byte parameter
//! block (gamma B0/B1, E1–E8, EB, ED) was then found byte for byte in this
//! board's factory firmware, so the table is the vendor's, not a near miss.

use esp_hal::{delay::Delay, gpio::Output};

/// One register write: command byte, then its parameters.
type Command = (u8, &'static [u8]);

const SW_RESET: u8 = 0x01;
const SLEEP_OUT: u8 = 0x11;
const INVERT_OFF: u8 = 0x20;
const DISPLAY_ON: u8 = 0x29;
const MADCTL: u8 = 0x36;
const SDIR: u8 = 0xC7;
const CMD2_BK0: Command = (0xFF, &[0x77, 0x01, 0x00, 0x00, 0x10]);

#[rustfmt::skip]
const INIT: &[Command] = &[
    (0xFF, &[0x77, 0x01, 0x00, 0x00, 0x10]),
    (0xC0, &[0x3B, 0x00]),
    (0xC1, &[0x0D, 0x02]),
    (0xC2, &[0x31, 0x05]),
    (0xCD, &[0x08]),
    (0xB0, &[0x00, 0x11, 0x18, 0x0E, 0x11, 0x06, 0x07, 0x08, 0x07, 0x22, 0x04, 0x12, 0x0F, 0xAA, 0x31, 0x18]),
    (0xB1, &[0x00, 0x11, 0x19, 0x0E, 0x12, 0x07, 0x08, 0x08, 0x08, 0x22, 0x04, 0x11, 0x11, 0xA9, 0x32, 0x18]),
    (0xFF, &[0x77, 0x01, 0x00, 0x00, 0x11]),
    (0xB0, &[0x60]),
    (0xB1, &[0x32]),
    (0xB2, &[0x07]),
    (0xB3, &[0x80]),
    (0xB5, &[0x49]),
    (0xB7, &[0x85]),
    (0xB8, &[0x21]),
    (0xC1, &[0x78]),
    (0xC2, &[0x78]),
    (0xE0, &[0x00, 0x1B, 0x02]),
    (0xE1, &[0x08, 0xA0, 0x00, 0x00, 0x07, 0xA0, 0x00, 0x00, 0x00, 0x44, 0x44]),
    (0xE2, &[0x11, 0x11, 0x44, 0x44, 0xED, 0xA0, 0x00, 0x00, 0xEC, 0xA0, 0x00, 0x00]),
    (0xE3, &[0x00, 0x00, 0x11, 0x11]),
    (0xE4, &[0x44, 0x44]),
    (0xE5, &[0x0A, 0xE9, 0xD8, 0xA0, 0x0C, 0xEB, 0xD8, 0xA0, 0x0E, 0xED, 0xD8, 0xA0, 0x10, 0xEF, 0xD8, 0xA0]),
    (0xE6, &[0x00, 0x00, 0x11, 0x11]),
    (0xE7, &[0x44, 0x44]),
    (0xE8, &[0x09, 0xE8, 0xD8, 0xA0, 0x0B, 0xEA, 0xD8, 0xA0, 0x0D, 0xEC, 0xD8, 0xA0, 0x0F, 0xEE, 0xD8, 0xA0]),
    (0xEB, &[0x02, 0x00, 0xE4, 0xE4, 0x88, 0x00, 0x40]),
    (0xEC, &[0x3C, 0x00]),
    (0xED, &[0xAB, 0x89, 0x76, 0x54, 0x02, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x20, 0x45, 0x67, 0x98, 0xBA]),
    (0xFF, &[0x77, 0x01, 0x00, 0x00, 0x13]),
    (0xE5, &[0xE4]),
    (0x3A, &[0x60]),
    // Board-specific, from the 4848S040 device page: back to bank 0 and clear
    // the MDT flag the generic table set above.
    (0xFF, &[0x77, 0x01, 0x00, 0x00, 0x10]),
    (0xCD, &[0x00]),
];

/// The 3-wire SPI link: chip select, clock and a bidirectional data line used
/// here as output only. Mode 3 — the clock idles high and the controller
/// samples on the rising edge.
pub struct St7701 {
    cs: Output<'static>,
    sck: Output<'static>,
    sda: Output<'static>,
    delay: Delay,
}

impl St7701 {
    pub fn new(mut cs: Output<'static>, mut sck: Output<'static>, sda: Output<'static>) -> Self {
        cs.set_high();
        sck.set_high();
        Self { cs, sck, sda, delay: Delay::new() }
    }

    /// Runs the full power-on sequence and leaves the panel displaying
    /// whatever arrives on the RGB bus. Takes about half a second, nearly all
    /// of it the datasheet's mandated waits.
    pub fn init(&mut self) {
        // There is no reset line on this board, so a software reset stands in
        // for one. 120 ms covers the datasheet's wait for both a reset issued
        // in sleep-in and the 5 ms minimum before the next command.
        self.command(SW_RESET, &[]);
        self.delay.delay_millis(120);

        for (cmd, params) in INIT {
            self.command(*cmd, params);
        }

        // Orientation, colour order and inversion. ESPHome makes these
        // configurable; this board wants all three at their defaults.
        self.command(CMD2_BK0.0, CMD2_BK0.1);
        self.command(SDIR, &[0x00]);
        self.command(MADCTL, &[0x00]);
        self.command(INVERT_OFF, &[]);

        self.command(SLEEP_OUT, &[]);
        self.delay.delay_millis(120);
        self.command(DISPLAY_ON, &[]);
        self.delay.delay_millis(20);
    }

    /// One command and its parameters inside a single chip-select window.
    fn command(&mut self, cmd: u8, params: &[u8]) {
        self.cs.set_low();
        self.word(false, cmd);
        for byte in params {
            self.word(true, *byte);
        }
        self.cs.set_high();
    }

    /// One 9-bit word, MSB first: the D/C flag (0 = command, 1 = parameter)
    /// and then the byte.
    fn word(&mut self, is_data: bool, byte: u8) {
        let bits = ((is_data as u16) << 8) | byte as u16;
        for i in (0..9).rev() {
            self.sck.set_low();
            if (bits >> i) & 1 == 1 {
                self.sda.set_high();
            } else {
                self.sda.set_low();
            }
            // ~500 kHz. The whole table is ~250 words, so speed is irrelevant
            // and slow edges remove one variable from bring-up.
            self.delay.delay_micros(1);
            self.sck.set_high();
            self.delay.delay_micros(1);
        }
    }
}
