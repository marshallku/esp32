PORT     ?= /dev/ttyACM0
CHIP     := esp32s3
PKG      ?= scd41-monitor
BIN      ?= $(PKG)
LOCATION ?= main_room
BIN_PATH := target/xtensa-esp32s3-none-elf/debug/$(BIN)
# Source toolchain + .env (common) + .env.<LOCATION> (per-board overrides)
# + .env.<PKG> (per-package overrides, e.g. a board on a different WiFi).
# .env* are gitignored — see .env.example for schema.
ESPENV := . $$HOME/.cargo/env-esp.sh; \
  if [ -f .env ]; then set -a; . ./.env; set +a; fi; \
  if [ -f .env.$(LOCATION) ]; then set -a; . ./.env.$(LOCATION); set +a; fi; \
  if [ -f .env.$(PKG) ]; then set -a; . ./.env.$(PKG); set +a; fi;

.PHONY: help build flash run monitor reset info release clean host-test host-build e2e

help:
	@echo "ESP32-S3 monorepo workspace — usage:"
	@echo "  make run [PKG=...]    빌드 + 플래시 + 시리얼 모니터 (인터랙티브, ctrl+c로 종료)"
	@echo "  make flash [PKG=...]  빌드 + 플래시만 (모니터 없음)"
	@echo "  make run BIN=...      특정 바이너리 빌드 + 플래시 + 모니터"
	@echo "  make monitor          시리얼 모니터만 (플래시 안 함)"
	@echo "  make build [PKG=...]  cargo build (Xtensa toolchain)"
	@echo "  make reset            보드 reset (정상 부팅)"
	@echo "  make info             칩 정보 출력"
	@echo "  make release [PKG=]   release profile 빌드"
	@echo "  make clean            cargo clean (workspace 전체)"
	@echo "  make host-test        desk-core + deskd 테스트 (PC 툴체인)"
	@echo "  make host-build       deskd release 빌드 → host/target/x86_64-unknown-linux-gnu/release/deskd"
	@echo "  make e2e              desk-panel 실기기 e2e: deck, 알림, 앱별 프로필, Claude 승인 (보드 연결 + 플래시 필요, deskd 서비스는 멈춘 상태로)"
	@echo ""
	@echo "현재 기본 패키지: $(PKG)"
	@echo "현재 기본 바이너리: $(BIN)"
	@echo "현재 location:   $(LOCATION) (.env.$(LOCATION) merged on top of .env)"
	@echo "                 — LOCATION은 scd41-monitor 전용, homelab-display는 무시"
	@echo ""
	@echo "패키지:       scd41-monitor (기본) / homelab-display"
	@echo "다른 패키지:  make run PKG=homelab-display"
	@echo "다른 포트:    make run PORT=/dev/ttyACM1"
	@echo "다른 location: make flash LOCATION=living_room PORT=/dev/ttyACM1"

build:
	$(ESPENV) cargo build -p $(PKG) --bin $(BIN)

release:
	$(ESPENV) cargo build --release -p $(PKG) --bin $(BIN)

flash: build
	$(ESPENV) espflash flash --chip $(CHIP) --port $(PORT) $(BIN_PATH)

# espflash monitor tries a chip handshake that hangs on ESP32-S3 native USB.
# Use plain stty+cat — USB-CDC is just a serial stream, no protocol needed.
run: flash
	@stty -F $(PORT) 115200 raw -echo -echoe -echok -echoctl -echoke -ixon -hupcl 2>/dev/null || true
	@echo "[monitor] $(PORT) — ctrl+c to exit"
	@cat $(PORT)

monitor:
	@stty -F $(PORT) 115200 raw -echo -echoe -echok -echoctl -echoke -ixon -hupcl 2>/dev/null || true
	@echo "[monitor] $(PORT) — ctrl+c to exit"
	@cat $(PORT)

reset:
	$(ESPENV) espflash reset --port $(PORT)

info:
	$(ESPENV) espflash board-info --port $(PORT)

clean:
	cargo clean

# The PC side lives in its own workspace and must be built from inside host/:
# cargo picks its config by working directory, and from here it would target
# the ESP32-S3.
host-test:
	cd host && cargo test

host-build:
	cd host && cargo build --release -p deskd

# Hardware e2e for desk-panel: needs the board plugged in, flashed with the
# current firmware, and no other deskd holding its port. OBS has its own
# script (host/e2e/obs.sh) because it needs OBS running.
e2e:
	cd host && cargo build -p deskd
	host/e2e/deck.sh
	host/e2e/notes.sh
	host/e2e/apps.sh
	host/e2e/claude.sh
