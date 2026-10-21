#!/bin/bash
# Per-app profiles e2e. The config is written at run time so one profile can
# name the window that has focus right now — that checks deskd reads the
# current focus from Hyprland at startup. Every later switch is injected over
# the control socket (`focus`), so the desktop's real focus is never touched.
source "$(dirname "$0")/lib.sh"
current=$(hyprctl activewindow -j | python3 -c 'import json,sys; print(json.load(sys.stdin).get("class",""))')
[ -n "$current" ] || { echo "no focused window to build the fixture from"; exit 1; }
mkdir -p "$OUT/xdg/deskd"
export XDG_CONFIG_HOME=$OUT/xdg
cat > "$OUT/xdg/deskd/config.toml" <<TOML
[[page]]
name = "default"
[[page.key]]
label = "Default"
action = { type = "shell", cmd = "date > \"\$E2E_OUT/pressed-default\"" }

[[app]]
name = "Current"
class = ["$current"]
[[app.page]]
name = "c"
[[app.page.key]]
label = "Current"
action = { type = "shell", cmd = "true" }

[[app]]
name = "Fake"
class = ["e2e.fake"]
[[app.page]]
name = "one"
[[app.page.key]]
label = "Fake A"
action = { type = "shell", cmd = "date > \"\$E2E_OUT/pressed-fake\"" }
[[app.page.key]]
label = "Next"
action = { type = "page", index = 1 }
[[app.page]]
name = "two"
[[app.page.key]]
label = "Fake B"
action = { type = "shell", cmd = "true" }
TOML
profile() { ctl '{"cmd":"status"}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["profile"] or "default")'; }
focus() { ctl "{\"cmd\":\"focus\",\"class\":\"$1\"}" >/dev/null; sleep 0.6; }

check "deskd up" start_deskd
check "the focused window ($current) selects its profile at startup" '[ "$(profile)" = Current ]'
walk_to page

focus e2e.fake
check "focus on another app switches the profile" '[ "$(profile)" = Fake ]'
tap $KEY0
check "keys run the app profile's actions" '[ -s $OUT/pressed-fake ]'
tap $KEY1
check "app profile pages navigate" '[ "$(screen)" = p1 ]'

focus E2E.FAKE
check "class matching ignores case (no switch)" '[ "$(profile)" = Fake ] && [ "$(screen)" = p1 ]'
focus some.other.app
check "an app without a profile gets the default" '[ "$(profile)" = default ]'
check "a profile switch opens page 0" '[ "$(screen)" = p0 ]'
tap $KEY0
check "the default profile's keys are back" '[ -s $OUT/pressed-default ]'

# A page action and a profile switch back to back: the switch must still
# open page 0, even though the page action is queued ahead of it.
focus e2e.fake
inject '{"g":"tap","x":240,"y":118}'
ctl '{"cmd":"focus","class":"some.other.app"}' >/dev/null
sleep 1
check "a page action racing a profile switch still ends on page 0" '[ "$(profile)" = default ] && [ "$(screen)" = p0 ]'

# A switch while already on page 0 sends a goto that changes nothing; it
# must not linger and drag a later switch off another screen.
focus e2e.fake
walk_to stats
focus some.other.app
check "a profile switch leaves a non-deck screen alone" '[ "$(screen)" = stats ]'
walk_to page

ctl '{"cmd":"reload"}' >/dev/null
focus e2e.fake
ctl '{"cmd":"reload"}' >/dev/null; sleep 0.6
check "a config reload keeps the focused app's profile" '[ "$(profile)" = Fake ]'
check "no panel errors" '! grep -qE "panel rejected|bad host line" $OUT/deskd.log'
exit $fail
