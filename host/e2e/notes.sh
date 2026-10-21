#!/bin/bash
# Notifications e2e: a real notify-send, overheard on D-Bus by deskd, lands on
# the panel; the banner opens the list, rows can be removed and cleared.
# Sends low-urgency notifications, so dunst shows each for a few seconds.
# Critical ones are not sent: dunst keeps those up until dismissed.
source "$(dirname "$0")/lib.sh"
export XDG_CONFIG_HOME=$HERE/fixtures/deck
status() { ctl '{"cmd":"status"}' | python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1"; }

check "deskd up" start_deskd
check "deskd is watching the session bus" 'grep -q "notifications: watching" $OUT/deskd.log'
walk_to page

notify-send -a desk-e2e -u low -t 2000 "desk-panel e2e 한글 제목" "본문 <b>markup</b> &amp; more"
check "notify-send reaches deskd with its real id" 'grep -q "notification [0-9]* from desk-e2e: desk-panel e2e 한글 제목" $OUT/deskd.log'
check "it is unread" '[ "$(status unread)" = 1 ]'
sleep 0.5
tap 240 40
check "tapping the banner opens the notifications" '[ "$(screen)" = notes ]'
check "opening them marks them read" '[ "$(status unread)" = 0 ]'

notify-send -a desk-e2e -u low -t 2000 "second" "on the notes screen already"
check "a second one arrives" '[ "$(status notes)" = 2 ]'
check "arriving on the open list counts as read" '[ "$(status unread)" = 0 ]'

inject '{"g":"long","x":100,"y":51}'; sleep 0.5
check "holding a row removes it" '[ "$(status notes)" = 1 ]'
tap 359 435
check "Clear removes the rest" '[ "$(status notes)" = 0 ]'
check "no panel errors" '! grep -qE "panel rejected|bad host line" $OUT/deskd.log'
exit $fail
