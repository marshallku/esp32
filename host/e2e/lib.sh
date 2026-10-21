# Shared helpers for the hardware e2e scripts. Sourced, not run.
#
# Every script drives the real panel through a real deskd: gestures are
# injected over deskd's control socket (`deskd ctl inject`), so no finger is
# needed, but the board must be plugged in and flashed with matching firmware.
# Usage: host/e2e/<name>.sh [path/to/deskd]   (defaults to the debug build)

set -u
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BIN=${1:-$HERE/../target/x86_64-unknown-linux-gnu/debug/deskd}
OUT=$(mktemp -d)
export E2E_OUT=$OUT
fail=0

# Retries for up to 3 s: the panel round trip is asynchronous, and a fixed
# sleep turns ordinary scheduling jitter into a failure. Slow passes are
# reported so latency regressions stay visible.
check() {
    local t0=$(date +%s%N)
    for _ in $(seq 1 30); do
        if eval "$2"; then
            local ms=$(( ($(date +%s%N) - t0) / 1000000 ))
            echo "PASS $1$([ $ms -gt 500 ] && echo " (after ${ms} ms)")"
            return
        fi
        sleep 0.1
    done
    echo "FAIL $1"
    fail=1
}

ctl() { "$BIN" ctl "$1"; }
inject() { ctl "{\"cmd\":\"inject\",\"gesture\":$1}" >/dev/null; }
tap() { inject "{\"g\":\"tap\",\"x\":$1,\"y\":$2}"; sleep 0.4; }
swipe() { inject "{\"g\":\"swipe\",\"dir\":\"$1\"}"; sleep 0.4; }
# dash | stats | notes | p<N>
screen() { ctl '{"cmd":"status"}' | python3 -c 'import json,sys; s=json.load(sys.stdin)["screen"]; print(s if isinstance(s,str) else "p%d" % s["page"])'; }
state() { ctl '{"cmd":"states"}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["states"].get(sys.argv[1]))' "$1"; }

# Key centres on the 3x3 grid, from desk_core::geometry.
KEY0="83 118"; KEY1="240 118"; KEY2="397 118"

start_deskd() {
    "$BIN" >>"$OUT/deskd.log" 2>&1 &
    DESKD=$!
    trap 'kill $DESKD 2>/dev/null; wait $DESKD 2>/dev/null' EXIT
    for _ in $(seq 1 50); do
        ctl '{"cmd":"status"}' 2>/dev/null | grep -q '"ready":true' && return 0
        sleep 0.2
    done
    return 1
}

stop_deskd() {
    kill $DESKD 2>/dev/null
    wait $DESKD 2>/dev/null
    trap - EXIT
}

walk_to() {
    for _ in $(seq 1 10); do swipe right; done
    [ "$1" = dash ] && return
    swipe left
    [ "$1" = stats ] && return
    swipe left
}
