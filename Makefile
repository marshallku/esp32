PORT     ?= /dev/ttyACM0
CHIP     := esp32s3
PKG      ?= scd41-monitor
BIN      ?= $(PKG)
LOCATION ?= main_room
BIN_PATH := target/xtensa-esp32s3-none-elf/debug/$(BIN)
# Source toolchain + .env (common) + .env.<LOCATION> (per-board overrides).
# .env* are gitignored — see .env.example for schema.
ESPENV := . $$HOME/.cargo/env-esp.sh; \
  if [ -f .env ]; then set -a; . ./.env; set +a; fi; \
  if [ -f .env.$(LOCATION) ]; then set -a; . ./.env.$(LOCATION); set +a; fi;

.PHONY: help build flash run monitor reset info release clean

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
	@echo ""
	@echo "현재 기본 패키지: $(PKG)"
	@echo "현재 기본 바이너리: $(BIN)"
	@echo "현재 location:   $(LOCATION) (.env.$(LOCATION) merged on top of .env)"
	@echo ""
	@echo "다른 패키지:  make run PKG=other-member"
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
