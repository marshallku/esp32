#!/bin/bash
# OBS e2e against the running OBS: only scene switching and Mic/Aux mute are
# exercised, and both are put back the way they were found. Recording and
# streaming are never pressed — those are covered by unit tests only.
source "$(dirname "$0")/lib.sh"
export XDG_CONFIG_HOME=$HERE/fixtures/obs
obs() { python3 -I "$HERE/obs_probe.py" | grep "^$1"; }
mic() { obs "input: 'Mic/Aux'" | awk '{print $NF}'; }
program() { obs "program scene" | cut -d' ' -f3-; }

orig_scene=$(program)
orig_mic=$(mic)
[ -n "$orig_scene" ] || { echo "OBS is not running"; exit 1; }
expect_muted=$([ "$orig_mic" = muted ] && echo True || echo False)
echo "found OBS on '$orig_scene', Mic/Aux $orig_mic"

check "deskd up" start_deskd
check "deskd identified with OBS" 'grep -q "OBS: connected" $OUT/deskd.log'
check "initial scene state from GetCurrentProgramScene" '[ "$(state "obs-scene:$orig_scene")" = True ]'
check "initial mute state from GetInputMute" '[ "$(state obs-muted:Mic/Aux)" = $expect_muted ]'
check "recording reported off" '[ "$(state obs-recording)" = False ]'

walk_to page
tap $KEY1
check "pressing Scene 3 switches OBS" '[ "$(program)" = "Scene 3" ]'
check "scene event moved the key state" '[ "$(state "obs-scene:Scene 3")" = True ] && [ "$(state "obs-scene:Scene 4")" = False ]'
tap $KEY0
check "pressing Scene 4 switches back" '[ "$(program)" = "Scene 4" ]'
tap $KEY2
check "mute key toggles the OBS input" '[ "$(mic)" != "$orig_mic" ]'
tap $KEY2
check "second press restores the input" '[ "$(mic)" = "$orig_mic" ]'
check "no panel errors" '! grep -q "panel rejected" $OUT/deskd.log'
check "OBS left on its original scene" '[ "$(program)" = "$orig_scene" ]'
exit $fail
