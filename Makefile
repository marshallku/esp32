PORT ?= /dev/ttyACM0
CHIP := esp32s3
PKG  ?= scd41-monitor
BIN  := target/xtensa-esp32s3-none-elf/debug/$(PKG)
ESPENV := . $$HOME/.cargo/env-esp.sh &&

.PHONY: help build flash run monitor reset info release clean

help:
	@echo "ESP32-S3 monorepo workspace — usage:"
	@echo "  make run [PKG=...]    빌드 + 플래시 + 시리얼 모니터 (인터랙티브, ctrl+c로 종료)"
	@echo "  make flash [PKG=...]  빌드 + 플래시만 (모니터 없음)"
	@echo "  make monitor          시리얼 모니터만 (플래시 안 함)"
	@echo "  make build [PKG=...]  cargo build (Xtensa toolchain)"
	@echo "  make reset            보드 reset (정상 부팅)"
	@echo "  make info             칩 정보 출력"
	@echo "  make release [PKG=]   release profile 빌드"
	@echo "  make clean            cargo clean (workspace 전체)"
	@echo ""
	@echo "현재 기본 패키지: $(PKG)"
	@echo "다른 패키지: make run PKG=other-member"
	@echo "다른 포트: make run PORT=/dev/ttyACM1"

build:
	$(ESPENV) cargo build -p $(PKG)

release:
	$(ESPENV) cargo build --release -p $(PKG)

flash: build
	$(ESPENV) espflash flash --chip $(CHIP) --port $(PORT) $(BIN)

# Flash without entering download mode after, then open monitor without resetting.
# This avoids the "waiting for download" hang on USB-Serial-JTAG boards.
run: flash
	$(ESPENV) espflash monitor --chip $(CHIP) --port $(PORT) --baud 115200 --before no-reset

monitor:
	$(ESPENV) espflash monitor --chip $(CHIP) --port $(PORT) --baud 115200 --before no-reset

reset:
	$(ESPENV) espflash reset --port $(PORT)

info:
	$(ESPENV) espflash board-info --port $(PORT)

clean:
	cargo clean
