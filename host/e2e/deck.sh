#!/bin/bash
# Deck e2e: handshake, screen order, key actions, page navigation, a burst of
# layout pushes during redraws, and a deskd restart that must not reboot the
# panel.
source "$(dirname "$0")/lib.sh"
export XDG_CONFIG_HOME=$HERE/fixtures/deck

check "handshake + profile push" start_deskd
walk_to dash
check "swipes reach the dashboard" '[ "$(screen)" = dash ]'
swipe left
check "swipe left from the dashboard opens the stats screen" '[ "$(screen)" = stats ]'
sleep 2.5
check "stats lines are accepted by the panel" '! grep -qE "bad host line|panel rejected" $OUT/deskd.log'
swipe left
check "next swipe opens page 0" '[ "$(screen)" = p0 ]'
tap $KEY0
check "tap on key 0 runs its shell action" '[ -s $OUT/pressed-a ]'
tap $KEY2
check "page action moves the panel to page 1" '[ "$(screen)" = p1 ]'
tap $KEY0
check "key 0 on page 1 runs that page's action" "[ -s $OUT/pressed-b ]"
tap 240 30
check "tap on the status strip does nothing" '[ "$(screen)" = p1 ]'

# Ten full layout pushes back to back: each redraws the panel, so host lines
# keep arriving while the UI task is busy drawing.
for _ in $(seq 1 10); do ctl '{"cmd":"reload"}' >/dev/null; done
sleep 3
check "reload burst: link survived" '! grep -qE "session ended|no ack" $OUT/deskd.log'
check "reload burst: still ready" 'ctl "{\"cmd\":\"status\"}" | grep -q "\"ready\":true"'

stop_deskd
banners=$(grep -c "ESP-ROM" $OUT/deskd.log)
check "restarted deskd re-handshakes" start_deskd
check "restart did not reboot the panel" '[ "$(grep -c ESP-ROM $OUT/deskd.log)" = "$banners" ]'
check "panel kept its screen across the restart" '[ "$(screen)" = p1 ]'
check "no protocol errors from the panel" '! grep -q "panel rejected" $OUT/deskd.log'
check "no UART receive overruns on the panel" '! grep -q FifoOverflowed $OUT/deskd.log'
exit $fail
