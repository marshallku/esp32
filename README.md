# ESP32 projects

Rust `no_std` firmware (esp-hal) for the ESP32-S3 boards in the homelab. One
Cargo workspace, one Xtensa toolchain, one Makefile; each board is a workspace
member. `host/` is a separate workspace for the PC side of `desk-panel`.

| Package | Board | Does |
| --- | --- | --- |
| `scd41-monitor` | ESP32-S3 DevKitC + SCD41 + SSD1306 | Room CO₂/temp/humidity and LD2410 presence → InfluxDB, with a 0.96" OLED readout. Deployed per room (`main_room`, `living_room`, `kitchen`). |
| `homelab-display` | Waveshare **ESP32-S3-RLCD-4.2** | Shelf display for homelab status: fetches one pre-aggregated JSON document and draws it on a 400×300 reflective LCD. |
| `desk-panel` | Guition **ESP32-S3-4848S040** (480×480 touch) | Stream-Deck-style control surface for the desk PC, driven by `host/deskd` over USB serial, plus a colour homelab dashboard. |
| `homelab-status` | — | The status document and its fetch client, shared by both displays. |

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

The board has no synchronised clock, so the server sends the snapshot's
wall-clock time as a preformatted string and the footer echoes it. That
indirection is the point. A relative age — "21s ago" — is a claim the display
makes about itself, and firmware that wedges just after a good draw would keep
making that claim forever while looking perfectly healthy. A stamp the board
only copies stops agreeing with your wristwatch instead, which is checkable
without trusting the board at all.

The server still computes an age and the screen still inverts its whole header
when that goes stale; the two catch different failures — a wedged aggregator
versus a wedged display. A monitoring display that fails quietly is worse than
no display at all.

The endpoint is authenticated. The document names internal hosts, ports and
services — a subset of what Uptime Kuma keeps behind a login — so it is not
left open on the LAN. The firmware sends `Authorization: Bearer $STATUS_TOKEN`,
the same shape `scd41-monitor` uses for InfluxDB, and reports a 401 on the
panel as a token mismatch rather than as an outage: those need different fixes.

### Array capacities are a contract

`homelab-status/src/model.rs` parses into `heapless::Vec`s whose capacities mirror `MAX_GROUPS`,
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

## desk-panel

The board is sold as a "4 inch control display" (silkscreen `V8292`): an
ESP32-S3-N16R8 behind a 480×480 IPS panel driven by an **ST7701S over a
16-bit RGB bus**, with a **GT911** capacitive touch controller. The only USB
port goes through a **CH340** to UART0 — the S3's own USB pins are used for
touch and colour data — so the board can never appear to the PC as a USB
keyboard. Everything it does on the PC goes through a daemon.

```
RGB (DPI):  DE=18 VSYNC=17 HSYNC=16 PCLK=21
            B=4,5,6,7,15  G=8,20,3,46,9,10  R=11,12,13,14,0
ST7701 3-wire SPI (init only): CS=39 SCK=48 SDA=47     backlight GPIO38
GT911:      I2C0 SDA=19 SCL=45, address 0x5D, no INT/RST — polled
UART0:      CH340 — TX=43 (logs + protocol), RX=44 (from deskd)
```

The factory firmware is backed up outside the repo; its ST7701 init table was
checked byte for byte against the one in `desk-panel/src/st7701.rs`.

### It works like a Stream Deck

A real Stream Deck is a dumb device: it reports key presses and shows the
bitmaps the host software sends. Profiles, pages, actions and the live state
on each key belong to the host. `desk-panel` keeps that split:

- **The panel** draws what it is sent and reports taps, long presses and
  swipes. It knows no actions. Swiping walks the screens in a fixed order —
  homelab dashboard, PC stats, then the button pages — and a minute untouched
  on a button page returns to the dashboard.
- **PC stats** (CPU total and per core, temperature, memory, GPU via
  `nvidia-smi`, network) arrive from deskd once a second; the screen keeps a
  two-minute graph, breaks it where samples are missing, and marks itself
  stale when they stop.
- **`deskd`** (`host/deskd`) owns the profile in `~/.config/deskd/config.toml`
  (start from `host/deskd/config.example.toml`), pushes pages of keys, runs
  the action behind a press, and updates keys when their state changes.

Actions: shell commands, key combinations to the focused window (Hyprland's
Lua dispatcher), playerctl, PipeWire volume and mute, page navigation, and
OBS (scene, record, stream, input mute). Keys can show live state — mic muted,
OBS recording, the current scene — drawn inverted while true.

### The wire protocol

JSON lines over the CH340 at 921600 (`host/desk-core/src/protocol.rs`). The
same UART carries the panel's logs, so device lines start with `@` and deskd
treats everything else as log. It is deliberately strict, because a press on
this link can run a command:

- **Stop and wait.** The panel acks every host line; deskd sends nothing more
  until it does. The panel's RX runs on a higher-priority interrupt executor,
  because drawing a screen takes ~100 ms and the UART's FIFO is 128 bytes.
- **Layouts are generations.** A profile arrives as `begin` / `page`… /
  `commit` and replaces the old one only when complete. Every press carries
  the generation it was made against; deskd ignores presses for any other,
  so a press on the old layout cannot run what now sits at the same spot.
  Generations start at a random number so a restarted deskd cannot collide
  with its predecessor.
- **Both ends watch the other.** deskd pings every 2 s; either side gives up
  after 6 s of silence.
- **Priorities, not a queue.** deskd sends approval prompts and screen changes
  first, finishes a layout transfer it has started, builds a pending layout
  only when its turn comes (so a burst of profile switches costs one
  transfer), and keeps only the newest clock and stats sample.
- **A touch belongs to what it began on.** A tap whose finger went down
  before the screen, the layout or a modal changed underneath it does
  nothing, so a release can never press a key the user did not see.

The panel switches its UART to 921600 once booted; the ROM and bootloader
still talk at 115200, which deskd reads as noise and skips. At that rate the
UART's 128-byte FIFO fills in 1.4 ms, so the firmware raises its receive
interrupt at 32 bytes instead of the default 120 — with the scanout keeping
PSRAM busy, code fetched from flash could not get back to the FIFO in time
with less margin.

`desk-core` holds the protocol, the screen geometry, gesture recognition and
the panel's whole UI state machine. It is `no_std` and shared by the firmware
and deskd, and it lives in `host/` so its tests run on the PC:

```sh
make host-test      # desk-core + deskd unit tests
make host-build     # host/target/x86_64-unknown-linux-gnu/release/deskd
make e2e            # on the real panel: deck + Claude approval suites
host/e2e/obs.sh     # on the real panel, against the running OBS
```

The e2e scripts drive the panel through a real deskd and inject gestures over
its control socket, so they need the board but no finger. Stop a resident
deskd first: only one process can hold the port.

`host/` must be built from inside `host/` (the Makefile targets do): cargo
reads its config from the working directory, and from the repository root it
targets the ESP32-S3.

### Opening the port must not reboot the board

The board's auto-reset circuit is wired to the CH340's DTR/RTS. Linux drops
both when a port closes with HUPCL set and raises them on the next open —
which reboots the ESP32 every time deskd restarts. deskd clears HUPCL right
after opening, so the lines stay up across closes; only the first open after
the board appears (plug-in, or an espflash run) still resets it, and the panel
announces itself with an unprompted `hello` for that case.

### The scanout restarts every frame

An RGB panel has no frame memory; something must clock a full frame out ~45
times a second. The first design chained DMA descriptors into a ring and let
the hardware loop. It survived a PSRAM stress test, but with WiFi running the
picture slid sideways and stayed there — the right edge drawn at the left. A
moment of PSRAM contention starves the LCD's FIFO, and a ring keeps going from
the wrong byte forever. (esp-idf has the same failure; its
`esp_lcd_rgb_panel_restart` is the remedy, and ESPHome calls it every loop for
this board.) The scanout now sends each frame as its own transfer from the
top of the buffer, from a loop on the second core, so an underrun can spoil at
most one frame. See `desk-panel/src/scanout.rs`.

### Approving Claude Code from the panel

`deskd hook` is a Claude Code `PermissionRequest` hook. It hands the request
to the daemon, which shows it on the panel; the button pressed becomes the
decision. Every way of *not* answering — no daemon, no panel, a timeout, the
**Terminal** button — prints nothing, which hands the request back to Claude
Code's own prompt. Silence is never approval.

**Allow** is offered only when the panel can show the whole operation: the
full Bash command, or the path for file edits, in the panel's ASCII font. A
command too long for the modal, or containing characters the font cannot
draw, gets Deny and Terminal only. A finger already on the glass when the
modal appears cannot answer it, and concurrent requests queue.

The hook is not registered by default — it changes every Claude Code session.
To enable it, add to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "PermissionRequest": [
      { "matcher": "*", "hooks": [{ "type": "command", "command": "deskd hook", "timeout": 60 }] }
    ]
  }
}
```

The hook waits at most 55 s, under the 60 s above, so it falls back on its
own rather than being killed.

### Running deskd

```sh
make host-build
install -Dm755 host/target/x86_64-unknown-linux-gnu/release/deskd ~/.local/bin/deskd
install -Dm644 host/deskd/config.example.toml ~/.config/deskd/config.toml   # then edit
deskd check
install -Dm644 host/deskd/deskd.service ~/.config/systemd/user/deskd.service
systemctl --user enable --now deskd
deskd ctl '{"cmd":"status"}'
```

`deskd ctl '{"cmd":"reload"}'` re-reads the config and pushes it. OBS is
contacted only when some key uses it; the obs-websocket password comes from
`$OBS_WEBSOCKET_PASSWORD` or, failing that, from OBS's own config.

### The dashboard and the network

The dashboard is the same document homelab-display shows (see above), on the
same `STATUS_URL` and `STATUS_TOKEN`. `make … PKG=desk-panel` sources
`.env.desk-panel` after `.env`, which is where this board's WiFi credentials
go. Status fetching is network-dependent: the board must be on a segment
allowed to reach the status server, or the dashboard shows `NO DATA`. The
button pages do not need WiFi at all.
