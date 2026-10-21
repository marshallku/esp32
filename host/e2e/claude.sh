#!/bin/bash
# Claude Code approval e2e: `deskd hook` ↔ daemon ↔ panel, answered by
# injected taps. Covers allow/deny/terminal, the no-Allow cases, the arming
# window, queueing, hook timeouts and a missing daemon.
source "$(dirname "$0")/lib.sh"
export XDG_CONFIG_HOME=$HERE/fixtures/deck

ask() { printf '{"session_id":"t","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"%s"},"cwd":"%s"}' "$1" "${CWD:-/home/u/dev/esp32}"; }
hook() { ask "$1" | DESKD_HOOK_WAIT_SECS=${WAIT:-20} "$BIN" hook > "$OUT/$2.out"; echo $? > "$OUT/$2.code"; }
hooks=()
bg() { hook "$@" & hooks+=($!); }
await_hooks() { for h in "${hooks[@]}"; do wait $h; done; hooks=(); }
behavior() { python3 -c "import json,sys; t=open(sys.argv[1]).read().strip(); print(json.loads(t)['hookSpecificOutput']['decision']['behavior'] if t else 'none')" "$OUT/$1.out"; }
# Three-button row (Deny, Terminal, Allow) and the two-button row (Deny, Terminal).
DENY="83 429"; TERM3="240 429"; ALLOW="397 429"; RIGHT2="357 429"
ptap() { inject "{\"g\":\"tap\",\"x\":$1,\"y\":$2}"; }

check "deskd up" start_deskd

bg "ls -la" allow; sleep 0.8; ptap $ALLOW; await_hooks
check "Allow → behavior allow" '[ "$(behavior allow)" = allow ]'
bg "rm -rf target" deny; sleep 0.8; ptap $DENY; await_hooks
check "Deny → behavior deny" '[ "$(behavior deny)" = deny ]'
bg "git push" term; sleep 0.8; ptap $TERM3; await_hooks
check "Terminal → no output, exit 0" '[ "$(behavior term)" = none ] && [ "$(cat $OUT/term.code)" = 0 ]'

LONG=$(python3 -c "print('echo ' + 'x'*900)")
bg "$LONG" long; sleep 0.8; ptap $RIGHT2; await_hooks
check "overlong command offers no Allow" '[ "$(behavior long)" = none ]'

NASTY=$(python3 -c 'print(("q\\\\ é " * 600))')
CWD=/x bg "$NASTY" nasty; sleep 1.2; ptap $RIGHT2; await_hooks
check "oversized non-ASCII request is shown and answerable" '[ "$(behavior nasty)" = none ] && grep -A1 "Bash in /x$" $OUT/deskd.log | grep -q "answered with button 1"'
check "link survived the oversized request" '! grep -qE "session ended|no ack" $OUT/deskd.log'

bg "echo early" early; sleep 0.05; ptap $ALLOW; sleep 0.8
check "tap inside the arming window is ignored" '[ ! -s $OUT/early.out ]'
ptap $ALLOW; await_hooks
check "a fresh tap after arming answers" '[ "$(behavior early)" = allow ]'

bg "echo first" q1; sleep 0.3; bg "echo second" q2; sleep 0.8
ptap $ALLOW; sleep 0.8; ptap $DENY; await_hooks
check "two concurrent requests are answered in order" '[ "$(behavior q1)" = allow ] && [ "$(behavior q2)" = deny ]'

WAIT=2 hook "echo slow" slow; sleep 0.5
check "hook gives up after its wait with no output" '[ "$(behavior slow)" = none ] && [ "$(cat $OUT/slow.code)" = 0 ]'
bg "echo after" after; sleep 0.8; ptap $ALLOW; await_hooks
check "timed-out prompt was taken down; the next one answers" '[ "$(behavior after)" = allow ]'
# allow, deny, term, long, nasty, early, q1, q2, after — the early tap inside
# the arming window and the timed-out prompt must not add answers.
check "exactly one answer per answered prompt" '[ "$(grep -c "answered with" $OUT/deskd.log)" = 9 ]'

stop_deskd
hook "echo nodaemon" nod
check "no daemon → no output, exit 0" '[ "$(behavior nod)" = none ] && [ "$(cat $OUT/nod.code)" = 0 ]'
exit $fail
