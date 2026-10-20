//! Desk panel for the Guition ESP32-S3-4848S040: a Stream-Deck-style control
//! surface driven by `deskd` on the PC over the USB serial link.
//!
//! Like a real Stream Deck, the panel holds no actions of its own. deskd
//! pushes the pages of keys and live updates; the panel draws them and
//! reports presses, swipes and prompt answers back. All behaviour that is not
//! pixels or wires lives in `desk_core::app`, which is unit-tested on the PC.
//!
//! Board wiring (see `display` and `gt911` for how each was confirmed):
//!   - RGB panel: DPI on 24 GPIOs, ST7701 init over 3-wire SPI CS=39 SCK=48 SDA=47
//!   - backlight GPIO38, touch GT911 on I2C0 SDA=19 SCL=45 (polled, addr 0x5D)
//!   - UART0 (CH340, the only USB port): TX=43 esp-println, RX=44 deskd lines

#![no_std]
#![no_main]

extern crate alloc;

use desk_core::{
    app::App,
    gesture::Recognizer,
    protocol::{self, DeviceMsg},
};
use desk_panel::{
    display::Display,
    display_pins,
    gt911::Gt911,
    link::{self, LINES, Line},
    ui,
};
use embassy_executor::Spawner;
use embassy_time::{Instant, Timer};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    i2c::master::{Config as I2cConfig, I2c},
    interrupt::{Priority, software::SoftwareInterruptControl},
    psram::{PsramConfig, PsramMode, SpiRamFreq},
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_println::println;
use esp_rtos::embassy::InterruptExecutor;
use static_cell::StaticCell;

esp_bootloader_esp_idf::esp_app_desc!();

const FW: &str = concat!("desk-panel ", env!("CARGO_PKG_VERSION"));

/// Touch poll and UI loop period. The GT911 reports at ~100 Hz.
const POLL_MS: u64 = 10;

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Internal RAM first, so small allocations stay fast; the two frames are
    // too big for it and land in PSRAM.
    esp_alloc::heap_allocator!(size: 64 * 1024);
    esp_alloc::psram_allocator!(
        peripherals.PSRAM,
        esp_hal::psram,
        PsramConfig {
            mode: PsramMode::OctalSpi,
            ram_frequency: SpiRamFreq::Freq80m,
            ..Default::default()
        }
    );

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw.software_interrupt0);

    println!("=== {} ===", FW);

    static RX_EXECUTOR: StaticCell<InterruptExecutor<1>> = StaticCell::new();
    let rx_executor = RX_EXECUTOR.init(InterruptExecutor::new(sw.software_interrupt1));
    rx_executor.start(Priority::Priority2).spawn(link::rx_task(peripherals.UART0, peripherals.GPIO44).unwrap());

    let i2c = I2c::new(peripherals.I2C0, I2cConfig::default().with_frequency(Rate::from_khz(400)))
        .expect("I2C0 config")
        .with_sda(peripherals.GPIO19)
        .with_scl(peripherals.GPIO45);
    let mut touch = Gt911::new(i2c);
    if let Err(e) = touch.product_id() {
        println!("GT911 not answering: {:?} — touch disabled until it does", e);
    }

    let mut display = Display::new(peripherals.LCD_CAM, peripherals.DMA_CH0, display_pins!(peripherals));
    let mut app = App::new(FW);
    let mut gestures = Recognizer::default();
    let mut touch_failed = false;

    // Announce ourselves unprompted too: a deskd already listening when the
    // board resets learns about it without waiting for its next reconnect.
    link::send(&DeviceMsg::Hello { proto: protocol::PROTO, fw: FW.into() });

    loop {
        let now = Instant::now().as_millis();

        while let Ok(line) = LINES.try_receive() {
            match line {
                Line::Text(text) => match protocol::decode_host(&text) {
                    Ok(msg) => {
                        for reply in app.on_host(msg, now) {
                            link::send(&reply);
                        }
                        link::send(&DeviceMsg::Ack);
                    }
                    // No ack: the host resends after its ack timeout, which is
                    // the recovery a corrupted line needs.
                    Err(e) => println!("bad host line ({}): {:?}", e, text),
                },
                Line::Overlong => println!("dropped an overlong or corrupted host line"),
            }
        }

        match touch.poll() {
            Ok(Some(point)) => {
                touch_failed = false;
                if let Some(ev) = gestures.sample(now, point.map(|p| (p.x, p.y))) {
                    for reply in app.on_gesture(ev, now) {
                        link::send(&reply);
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                if !touch_failed {
                    println!("touch read failed: {:?}", e);
                }
                touch_failed = true;
            }
        }

        for reply in app.tick(now) {
            link::send(&reply);
        }

        if app.take_dirty() {
            ui::draw(&mut display.canvas(), &app);
            display.present();
        }

        Timer::after_millis(POLL_MS).await;
    }
}
