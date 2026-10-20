//! A DMA buffer that never ends: one frame in PSRAM, its descriptors chained
//! into a ring.
//!
//! An RGB panel has no memory of its own — it shows whatever is on the bus at
//! the moment, so something has to clock a whole frame out ~45 times a second
//! forever. Re-sending each frame from the CPU (what `panel-test` did first)
//! works, but it pins a core to the job and stops the picture the moment
//! anything else runs long. Chaining the last descriptor back to the first and
//! setting the LCD's `next_frame_en` hands the loop to hardware instead: GDMA
//! walks the ring and `lcd_cam` turns it into frames with no CPU involved.
//!
//! This is the same shape esp-idf's RGB driver uses, minus its bounce buffers.
//! It relies on the buffer being exactly one frame long, so the ring's seam
//! always falls on a frame boundary. A FIFO underrun would break that and
//! roll the picture, which is why `panel-test` loads PSRAM on purpose while
//! scanning out.

use core::ptr::NonNull;

use esp_hal::dma::{
    BurstConfig, DmaDescriptor, DmaTxBuffer, ExternalBurstConfig, InternalBurstConfig, Owner,
    Preparation, TransferDirection,
};

/// Bytes per descriptor. The hardware ceiling is 4095; this is the largest
/// multiple of the 64-byte PSRAM burst below it, so every chunk but the last
/// starts burst-aligned.
pub const CHUNK: usize = 4032;

/// Descriptors needed to ring `len` bytes.
pub const fn descriptor_count(len: usize) -> usize {
    len.div_ceil(CHUNK)
}

const BURST: ExternalBurstConfig = ExternalBurstConfig::Size64;

pub struct ScanoutBuf {
    descriptors: &'static mut [DmaDescriptor],
    frame: NonNull<u8>,
    len: usize,
}

impl ScanoutBuf {
    /// Links `descriptors` into a ring over `frame`.
    ///
    /// The caller keeps writing to the frame while the DMA reads it, which is
    /// the point, so this takes a raw pointer rather than a borrow.
    ///
    /// # Safety
    /// `frame` must point to one allocation of at least `len` bytes, 64-byte
    /// aligned, in PSRAM, that is never freed — the ring keeps reading it for
    /// as long as the transfer runs, which in practice is forever.
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
            let d = &mut descriptors[i];
            d.set_size(size);
            d.set_length(size);
            d.set_owner(Owner::Dma);
            // No EOF anywhere: the ring has no end, and an EOF mid-stream
            // would only raise an interrupt nobody handles.
            d.set_suc_eof(false);
            d.buffer = unsafe { frame.as_ptr().add(offset) };
            d.next = unsafe { base.add((i + 1) % count) };
        }

        Self { descriptors, frame, len }
    }
}

unsafe impl DmaTxBuffer for ScanoutBuf {
    type View = ScanoutBuf;
    type Final = ScanoutBuf;

    fn prepare(&mut self) -> Preparation {
        Preparation {
            start: self.descriptors.as_mut_ptr(),
            direction: TransferDirection::Out,
            accesses_psram: true,
            burst_transfer: BurstConfig { external_memory: BURST, internal_memory: InternalBurstConfig::Enabled },
            // Owner bits are never handed back by software on a ring, so the
            // DMA must not stop at a descriptor it thinks the CPU still owns.
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

impl ScanoutBuf {
    pub fn frame(&self) -> (NonNull<u8>, usize) {
        (self.frame, self.len)
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
