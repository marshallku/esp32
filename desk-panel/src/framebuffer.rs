//! An RGB565 frame in a byte slice, as embedded-graphics draws it and as the
//! DPI DMA reads it.
//!
//! Pixels are little-endian u16s: the `lcd_cam` peripheral shifts the low byte
//! out first, and the board wires D0–D4 to blue, D5–D10 to green and D11–D15
//! to red, so a little-endian RGB565 word lands on the right pins unchanged.

use embedded_graphics::{
    pixelcolor::{Rgb565, raw::RawU16},
    prelude::*,
};

pub const WIDTH: u32 = 480;
pub const HEIGHT: u32 = 480;
pub const BYTES: usize = (WIDTH * HEIGHT * 2) as usize;

pub struct Framebuffer<'a> {
    bytes: &'a mut [u8],
}

impl<'a> Framebuffer<'a> {
    /// Panics if `bytes` is not exactly one frame — a short buffer would
    /// otherwise surface as a DMA underrun, which looks like a timing fault.
    pub fn new(bytes: &'a mut [u8]) -> Self {
        assert_eq!(bytes.len(), BYTES, "framebuffer must be exactly one 480x480 RGB565 frame");
        Self { bytes }
    }
}

impl OriginDimensions for Framebuffer<'_> {
    fn size(&self) -> Size {
        Size::new(WIDTH, HEIGHT)
    }
}

impl DrawTarget for Framebuffer<'_> {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if point.x < 0 || point.y < 0 || point.x >= WIDTH as i32 || point.y >= HEIGHT as i32 {
                continue;
            }
            let i = (point.y as usize * WIDTH as usize + point.x as usize) * 2;
            let raw = RawU16::from(color).into_inner().to_le_bytes();
            self.bytes[i..i + 2].copy_from_slice(&raw);
        }
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        let raw = RawU16::from(color).into_inner().to_le_bytes();
        for px in self.bytes.chunks_exact_mut(2) {
            px.copy_from_slice(&raw);
        }
        Ok(())
    }
}
