# ESP32 projects

Rust `no_std` firmware (esp-hal) for the ESP32-S3 boards in the homelab. One
Cargo workspace, one Xtensa toolchain, one Makefile; each board is a workspace
member.

| Package | Board | Does |
| --- | --- | --- |
| `scd41-monitor` | ESP32-S3 DevKitC + SCD41 + SSD1306 | Room CO₂/temp/humidity and LD2410 presence → InfluxDB, with a 0.96" OLED readout. Deployed per room (`main_room`, `living_room`, `kitchen`). |
| `homelab-display` | Waveshare **ESP32-S3-RLCD-4.2** | Shelf display for homelab status: fetches one pre-aggregated JSON document and draws it on a 400×300 reflective LCD. |

## Build and flash

```sh
make run                          # default package (scd41-monitor), flash + monitor
make run PKG=homelab-display      # the shelf display
make flash PKG=homelab-display    # flash only
make monitor                      # serial monitor only
make help                         # everything else
```

Configuration is injected at build time from `.env` (gitignored; see
`.env.example` for the schema). `scd41-monitor` additionally merges
`.env.<LOCATION>` for its per-room InfluxDB tags — `homelab-display` has no
per-location variant and ignores `LOCATION`.

`homelab-display` needs `STATUS_URL` and `STATUS_TOKEN`. The token has to match
`STATUS_TOKEN` in the aggregator's own `.env` on pi01 — rotating it means
editing both files, restarting that stack and reflashing the board.

## homelab-display

The board is a Waveshare ESP32-S3-RLCD-4.2: ESP32-S3-WROOM-1-N16R8 (16 MB
flash, 8 MB octal PSRAM) behind a 4.2" **reflective LCD** — an ST7305 panel,
400×300, 1 bit deep. It looks like e-paper and needs no backlight, but it is an
LCD: a full frame goes out in tens of milliseconds with no ghosting, so the
firmware simply redraws everything every refresh.

Being 1 bit deep is the constraint that shapes the layout — no state can be
carried by colour, so it is carried by inversion and by fixed position.

```
LCD  (bit-banged): SCK=GPIO11 MOSI=GPIO12 DC=GPIO5 CS=GPIO40 RST=GPIO41 TE=GPIO6
I2C0:        SDA=GPIO13  SCL=GPIO14   — SHTC3, PCF85063 RTC, ES8311, ES7210
I2S:         MCLK=16 BCLK=9 WS=45 DIN=10 DOUT=8, speaker PA=46
SD (1-bit):  CLK=38  CMD=21  D0=39
Battery:     ADC1_CH3 (GPIO4), ×3 divider        KEY button: GPIO18
```

Only the LCD and the SHTC3 are used so far. The audio codecs, RTC, SD slot and
battery gauge are present on the board and unclaimed.

### Where the data comes from

The device does **no** aggregation. It fetches `STATUS_URL` — a ~1 KB document
served by the `homelab-status` stack on pi01
([manifest](https://github.com/marshallku/manifest),
`docker-compose/pi01/homelab-status`) — and renders it.

That split is deliberate. Uptime Kuma has 58 monitors across 8 groups; picking
the interesting ones, computing outage durations and pulling host gauges out of
Prometheus is work that belongs on a host with a clock, a package manager and
room to fail. The board gets a fixed shape it can parse into fixed-size structs.

It also decides *where* the aggregator runs: on pi01, next to Kuma's database,
so the display keeps naming what is down during a `prd01` outage — the one
moment it is worth looking at. Prometheus lives in-cluster and dies with
`prd01`, so its half degrades to a "host metrics unavailable" line while the
Kuma half stays live.

The board has no synchronised clock, so it cannot tell how old a document is
from a timestamp. The server stamps `age` at *request* time instead, and the
screen inverts its whole header when that goes stale — a monitoring display
that fails quietly is worse than no display at all.

The endpoint is authenticated. The document names internal hosts, ports and
services — a subset of what Uptime Kuma keeps behind a login — so it is not
left open on the LAN. The firmware sends `Authorization: Bearer $STATUS_TOKEN`,
the same shape `scd41-monitor` uses for InfluxDB, and reports a 401 on the
panel as a token mismatch rather than as an outage: those need different fixes.

### Array capacities are a contract

`model.rs` parses into `heapless::Vec`s whose capacities mirror `MAX_GROUPS`,
`MAX_DOWN` and `MAX_HOSTS` in the aggregator's `aggregate.py`.
`serde-json-core` fails a parse outright when an array overruns, so raising a
cap on the server without raising it here takes the screen down. Change both.

The counts have a contract of their own: `total` covers only *settled* monitors
because a retrying one in the denominator would make every group flap between
`8/8` and `7/8`. The excluded ones come back as `unsettled` and the footer
prints them, so the screen never shows a denominator that quietly shrank.

### The panel is driven by bit-banged SPI, not the SPI peripheral

`esp_hal::spi::master::Spi` does not drive this panel. Bit-banging the same
driver — same init table, same CS/DC framing, same framebuffer — draws
correctly on the same pins, which isolates the fault to the peripheral path.
Ruled out on hardware, in this order:

| Suspect | How it was ruled out |
| --- | --- |
| Panel or wiring | Waveshare's factory firmware, reflashed, draws fine |
| Init sequence | Byte-for-byte against the vendor BSP: 28 steps, identical |
| Framebuffer packing | Exact bijection vs the vendor C over all 120,000 pixels |
| CS framing | Continuous CS *and* the vendor's per-byte framing: both silent |
| Peripheral choice | SPI2 and SPI3: both silent |
| Clock rate and mode | 400 kHz / 1 / 4 / 10 MHz across modes 0 and 3: all silent |
| Bit order | `Config` defaults are MSB-first, mode 0, as required |

The clinching observation came from the sweep: the panel kept displaying the
image left over from the bit-bang test the whole way through. A reflective LCD
holds its last frame, so that leftover was proof that not one byte reached the
controller under any peripheral setting.

**Why esp-hal 1.1 fails here is unresolved, not explained.** Bit-banging is not
a workaround preferred over a fix; it is the transport that demonstrably works.
The cost is invisible at this duty cycle — 15,000 bytes once per 30 s refresh,
on a core with nothing else to do. `src/bitbang_spi.rs` carries the same list
so the next person to look does not start over.

If the screen ever goes blank again, `make flash PKG=homelab-display
BIN=panel-test` walks solid fills, geometry and text with each step named on
the console, which separates "transport or init" from "what the app draws" in
one flash.

### The framebuffer mapping is load-bearing

`st7305.rs` packs a 2(x)×4(y) pixel block per byte with the panel's rows
running bottom-up, ported from the vendor BSP. A mistake there does not fail
loudly — it draws a scrambled but plausible image — so it was verified as an
exact bijection against the vendor C over all 120 000 pixels before it was
trusted.
