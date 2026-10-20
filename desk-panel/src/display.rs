//! The 4848S040's panel: ST7701 init, the DPI bus, and the PSRAM frames.
//!
//! `Display::new` returns the drawing side and a `Scanout` that must be run
//! for the panel to show anything — see `scanout` for why it is a loop on a
//! core rather than a DMA ring.
//!
//! Drawing goes to a back buffer and `present` copies it to the front in one
//! pass. Drawing straight into the scanned-out frame would show every
//! intermediate step — a page is a clear followed by dozens of primitives, and
//! the panel would flash the cleared state on its way there.

use core::ptr::NonNull;

use esp_hal::{
    dma::DmaDescriptor,
    gpio::{Level, Output, OutputConfig},
    lcd_cam::{
        LcdCam,
        lcd::{
            ClockMode, Phase, Polarity,
            dpi::{Config as DpiConfig, Dpi, Format, FrameTiming},
        },
    },
    peripherals::{self, DMA_CH0, LCD_CAM},
    time::Rate,
};

use crate::{
    framebuffer::{self, Framebuffer, HEIGHT, WIDTH},
    scanout::{self, FrameBuf, Scanout},
    st7701::St7701,
};

// The 4848S040 timings from ESPHome's device page, in esp-idf's terms. esp-hal
// counts the horizontal and vertical "front porch" from the start of the sync
// pulse to the first active pixel, which is esp-idf's pulse + back porch.
pub const PCLK_MHZ: u32 = 12;
const HSYNC_PULSE: usize = 8;
const HSYNC_BACK: usize = 20;
const HSYNC_FRONT: usize = 10;
const VSYNC_PULSE: usize = 8;
const VSYNC_BACK: usize = 10;
const VSYNC_FRONT: usize = 10;

/// Every pin the panel uses. Grouped so `Display::new` takes the board's
/// wiring as one value rather than twenty-four arguments.
pub struct Pins {
    pub backlight: peripherals::GPIO38<'static>,
    pub spi_cs: peripherals::GPIO39<'static>,
    pub spi_sck: peripherals::GPIO48<'static>,
    pub spi_sda: peripherals::GPIO47<'static>,
    pub vsync: peripherals::GPIO17<'static>,
    pub hsync: peripherals::GPIO16<'static>,
    pub de: peripherals::GPIO18<'static>,
    pub pclk: peripherals::GPIO21<'static>,
    pub b: (
        peripherals::GPIO4<'static>,
        peripherals::GPIO5<'static>,
        peripherals::GPIO6<'static>,
        peripherals::GPIO7<'static>,
        peripherals::GPIO15<'static>,
    ),
    pub g: (
        peripherals::GPIO8<'static>,
        peripherals::GPIO20<'static>,
        peripherals::GPIO3<'static>,
        peripherals::GPIO46<'static>,
        peripherals::GPIO9<'static>,
        peripherals::GPIO10<'static>,
    ),
    pub r: (
        peripherals::GPIO11<'static>,
        peripherals::GPIO12<'static>,
        peripherals::GPIO13<'static>,
        peripherals::GPIO14<'static>,
        peripherals::GPIO0<'static>,
    ),
}

pub struct Display {
    _backlight: Output<'static>,
    front: NonNull<u8>,
    back: &'static mut [u8],
}

impl Display {
    /// Brings the panel up. Nothing shows until the returned `Scanout` runs.
    /// Needs the PSRAM heap in place already: both frames live there.
    ///
    /// # Panics
    /// If PSRAM cannot hold two frames or the DPI config is rejected — both
    /// mean the board is not the one this was written for.
    pub fn new(lcd_cam: LCD_CAM<'static>, dma: DMA_CH0<'static>, pins: Pins) -> (Self, Scanout) {
        let out = OutputConfig::default();
        let backlight = Output::new(pins.backlight, Level::High, out);
        St7701::new(
            Output::new(pins.spi_cs, Level::High, out),
            Output::new(pins.spi_sck, Level::High, out),
            Output::new(pins.spi_sda, Level::Low, out),
        )
        .init();

        let front = alloc_frame();
        let back = unsafe { core::slice::from_raw_parts_mut(alloc_frame().as_ptr(), framebuffer::BYTES) };

        const DESCRIPTORS: usize = scanout::descriptor_count(framebuffer::BYTES);
        let descriptors = {
            static mut CHAIN: [DmaDescriptor; DESCRIPTORS] = [DmaDescriptor::EMPTY; DESCRIPTORS];
            // Taken exactly once: `Display` owns LCD_CAM, which is a
            // singleton, so a second `new` cannot be reached.
            unsafe { &mut *core::ptr::addr_of_mut!(CHAIN) }
        };
        // SAFETY: `front` is a leaked, 64-byte-aligned, frame-sized PSRAM
        // allocation from `alloc_frame`, so it outlives the scanout.
        let buf = unsafe { FrameBuf::new(descriptors, front, framebuffer::BYTES) };

        let config = DpiConfig::default()
            .with_frequency(Rate::from_mhz(PCLK_MHZ))
            .with_clock_mode(ClockMode { polarity: Polarity::IdleLow, phase: Phase::ShiftLow })
            .with_format(Format { enable_2byte_mode: true, ..Default::default() })
            .with_timing(FrameTiming {
                horizontal_active_width: WIDTH as usize,
                horizontal_total_width: WIDTH as usize + HSYNC_PULSE + HSYNC_BACK + HSYNC_FRONT,
                horizontal_blank_front_porch: HSYNC_PULSE + HSYNC_BACK,
                vertical_active_height: HEIGHT as usize,
                vertical_total_height: HEIGHT as usize + VSYNC_PULSE + VSYNC_BACK + VSYNC_FRONT,
                vertical_blank_front_porch: VSYNC_PULSE + VSYNC_BACK,
                hsync_width: HSYNC_PULSE,
                vsync_width: VSYNC_PULSE,
                hsync_position: 0,
            })
            .with_vsync_idle_level(Level::High)
            .with_hsync_idle_level(Level::High)
            .with_de_idle_level(Level::Low)
            .with_disable_black_region(false);

        let lcd_cam = LcdCam::new(lcd_cam);
        let dpi = Dpi::new(lcd_cam.lcd, dma, config)
            .unwrap_or_else(|e| panic!("DPI config rejected: {:?}", e))
            .with_vsync(pins.vsync)
            .with_hsync(pins.hsync)
            .with_de(pins.de)
            .with_pclk(pins.pclk)
            .with_data0(pins.b.0)
            .with_data1(pins.b.1)
            .with_data2(pins.b.2)
            .with_data3(pins.b.3)
            .with_data4(pins.b.4)
            .with_data5(pins.g.0)
            .with_data6(pins.g.1)
            .with_data7(pins.g.2)
            .with_data8(pins.g.3)
            .with_data9(pins.g.4)
            .with_data10(pins.g.5)
            .with_data11(pins.r.0)
            .with_data12(pins.r.1)
            .with_data13(pins.r.2)
            .with_data14(pins.r.3)
            .with_data15(pins.r.4);

        (Self { _backlight: backlight, front, back }, Scanout::new(dpi, buf))
    }

    /// The back buffer, to draw the next frame into.
    pub fn canvas(&mut self) -> Framebuffer<'_> {
        Framebuffer::new(self.back)
    }

    /// Shows the back buffer. Copies the whole frame (~10 ms PSRAM to PSRAM)
    /// and flushes it out of the cache; the copy is fast enough to land
    /// within about one frame, so a page change tears for at most one frame.
    pub fn present(&mut self) {
        let front = unsafe { core::slice::from_raw_parts_mut(self.front.as_ptr(), framebuffer::BYTES) };
        front.copy_from_slice(self.back);
        scanout::writeback(front.as_ptr(), front.len());
    }
}

/// One zeroed, 64-byte-aligned frame from the heap — which by now is PSRAM,
/// since nothing else is big enough. Leaked: frames live as long as the panel.
fn alloc_frame() -> NonNull<u8> {
    let layout = core::alloc::Layout::from_size_align(framebuffer::BYTES, 64).expect("frame layout");
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    NonNull::new(ptr).expect("PSRAM cannot hold a frame")
}

/// Takes the panel's pins out of `esp_hal::init`'s peripherals. A macro
/// because the fields have to be moved out of the caller's binding one by one.
#[macro_export]
macro_rules! display_pins {
    ($p:ident) => {
        $crate::display::Pins {
            backlight: $p.GPIO38,
            spi_cs: $p.GPIO39,
            spi_sck: $p.GPIO48,
            spi_sda: $p.GPIO47,
            vsync: $p.GPIO17,
            hsync: $p.GPIO16,
            de: $p.GPIO18,
            pclk: $p.GPIO21,
            b: ($p.GPIO4, $p.GPIO5, $p.GPIO6, $p.GPIO7, $p.GPIO15),
            g: ($p.GPIO8, $p.GPIO20, $p.GPIO3, $p.GPIO46, $p.GPIO9, $p.GPIO10),
            r: ($p.GPIO11, $p.GPIO12, $p.GPIO13, $p.GPIO14, $p.GPIO0),
        }
    };
}
