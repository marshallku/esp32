//! Keeping the RGB panel fed: one frame in PSRAM, sent from its first byte
//! every single frame.
//!
//! An RGB panel has no memory of its own — it shows whatever is on the bus at
//! the moment, so something has to clock a whole frame out ~45 times a second
//! forever.
//!
//! # Why every frame is restarted rather than looped in hardware
//!
//! The first version chained the last DMA descriptor back to the first and
//! set the LCD's `next_frame_en`, so GDMA walked a ring with no CPU involved.
//! It held still through a PSRAM stress test, then — once WiFi was running —
//! the picture slid sideways and stayed there: the right edge of the screen
//! drawn at the left. That is a FIFO underrun. When the PSRAM bus is busy
//! (WiFi, flash, the CPU's own cache traffic) the LCD outruns the DMA for a
//! moment and clocks out pixels it does not have; the ring keeps going from
//! wherever it was, so every later frame starts at the wrong byte, forever.
//! esp-idf has the same failure and the same remedy —
//! `esp_lcd_rgb_panel_restart`, which ESPHome calls on every loop for this
//! board.
//!
//! Here each frame is its own transfer from the start of the buffer, re-sent
//! by `Scanout::run` the moment the previous one ends. An underrun can now
//! spoil at most the one frame it happens in. The cost is a CPU core spinning
//! on the LCD, which is why `run` is meant for the second core.

use core::ptr::NonNull;

use esp_hal::{
    Blocking,
    dma::{
        BurstConfig, DmaDescriptor, DmaTxBuffer, ExternalBurstConfig, InternalBurstConfig, Owner, Preparation,
        TransferDirection,
    },
    lcd_cam::lcd::dpi::Dpi,
};

/// Bytes per descriptor. The hardware ceiling is 4095; this is the largest
/// multiple of the 64-byte PSRAM burst below it, so every chunk but the last
/// starts burst-aligned.
pub const CHUNK: usize = 4032;

/// Descriptors needed to chain `len` bytes.
pub const fn descriptor_count(len: usize) -> usize {
    len.div_ceil(CHUNK)
}

const BURST: ExternalBurstConfig = ExternalBurstConfig::Size64;

pub struct FrameBuf {
    descriptors: &'static mut [DmaDescriptor],
}

// SAFETY: the descriptors and the frame they point at are 'static and owned
// by this value alone on the DMA side; moving it to the core that runs the
// scanout loop moves that ownership with it.
unsafe impl Send for FrameBuf {}

impl FrameBuf {
    /// Chains `descriptors` over `frame`, start to end.
    ///
    /// The caller keeps writing to the frame while the DMA reads it, which is
    /// the point, so this takes a raw pointer rather than a borrow.
    ///
    /// # Safety
    /// `frame` must point to one allocation of at least `len` bytes, 64-byte
    /// aligned, in PSRAM, that is never freed — the scanout reads it for as
    /// long as the panel is on, which in practice is forever.
    ///
    /// # Panics
    /// If there are too few descriptors for `len`.
    pub unsafe fn new(descriptors: &'static mut [DmaDescriptor], frame: NonNull<u8>, len: usize) -> Self {
        let count = descriptor_count(len);
        assert!(descriptors.len() >= count, "need {} descriptors for {} bytes", count, len);

        let base = descriptors.as_mut_ptr();
        for i in 0..count {
            let offset = i * CHUNK;
            let size = CHUNK.min(len - offset);
            let last = i + 1 == count;
            let d = &mut descriptors[i];
            d.set_size(size);
            d.set_length(size);
            d.set_owner(Owner::Dma);
            d.set_suc_eof(last);
            d.buffer = unsafe { frame.as_ptr().add(offset) };
            d.next = if last { core::ptr::null_mut() } else { unsafe { base.add(i + 1) } };
        }

        Self { descriptors }
    }
}

unsafe impl DmaTxBuffer for FrameBuf {
    type View = FrameBuf;
    type Final = FrameBuf;

    fn prepare(&mut self) -> Preparation {
        Preparation {
            start: self.descriptors.as_mut_ptr(),
            direction: TransferDirection::Out,
            accesses_psram: true,
            burst_transfer: BurstConfig { external_memory: BURST, internal_memory: InternalBurstConfig::Enabled },
            // The same descriptors go out every frame and nothing hands owner
            // bits back between sends, so the DMA must not insist on them.
            check_owner: Some(false),
            auto_write_back: false,
        }
    }

    fn into_view(self) -> Self::View {
        self
    }

    fn from_view(view: Self::View) -> Self {
        view
    }
}

/// The DPI bus and the frame it sends; `run` keeps the panel lit.
pub struct Scanout {
    dpi: Dpi<'static, Blocking>,
    buf: FrameBuf,
}

impl Scanout {
    pub fn new(dpi: Dpi<'static, Blocking>, buf: FrameBuf) -> Self {
        Self { dpi, buf }
    }

    /// Sends the frame, waits for it to finish, sends it again — forever. A
    /// transfer error is skipped over rather than fatal: the next frame
    /// starts clean, which is the whole point of restarting every frame.
    pub fn run(self) -> ! {
        let Self { mut dpi, mut buf } = self;
        loop {
            match dpi.send(false, buf) {
                Ok(transfer) => {
                    let (_, d, b) = transfer.wait();
                    (dpi, buf) = (d, b);
                }
                Err((_, d, b)) => (dpi, buf) = (d, b),
            }
        }
    }
}

/// Pushes CPU writes to `[addr, addr + len)` out of the data cache into PSRAM.
///
/// The CPU reaches PSRAM through the cache but GDMA reads PSRAM directly, so
/// without this a freshly drawn frame can sit in cache lines the scanout never
/// sees. These are the ROM routines esp-hal calls for its own PSRAM DMA
/// buffers; esp-hal keeps its wrapper private, so they are declared here.
pub fn writeback(addr: *const u8, len: usize) {
    unsafe extern "C" {
        fn rom_Cache_WriteBack_Addr(addr: u32, size: u32);
        fn Cache_Suspend_DCache_Autoload() -> u32;
        fn Cache_Resume_DCache_Autoload(value: u32);
    }
    // Autoload is suspended for the duration, as esp-hal does, so the cache
    // does not prefetch lines back in while they are being written out.
    unsafe {
        let autoload = Cache_Suspend_DCache_Autoload();
        rom_Cache_WriteBack_Addr(addr as u32, len as u32);
        Cache_Resume_DCache_Autoload(autoload);
    }
}
