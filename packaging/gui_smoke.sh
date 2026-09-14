#!/bin/bash
# Opens the built app the way Finder does and has it press its own buttons,
# screenshotting as it goes. This is the only place the window itself is ever
# exercised, so it runs in CI on every push.
#
# Needs a logged-in GUI session, which GitHub's macOS runners have. Results
# are also emitted as ::notice:: / ::error:: annotations, which (unlike the
# log) can be read from the public API without signing in.
set -uo pipefail
cd "$(dirname "$0")/.."

APP="$PWD/dist/Snuffler.app"
SHOTS="$PWD/dist/screens"
mkdir -p "$SHOTS"
WORK=$(mktemp -d)
cp conformance/fixtures/ghosts.xlsx conformance/fixtures/emf-heavy.xlsm "$WORK/"

fail() {
    echo "::error title=GUI smoke test::$1"
    exit 1
}

shot() {
    # Screen recording can be refused on a runner; a missing picture is not
    # a failure of the app.
    screencapture -x "$SHOTS/$1.png" 2>/dev/null || echo "::warning title=GUI smoke test::screencapture unavailable for $1"
}

quit() {
    osascript -e 'quit app "Snuffler"' 2>/dev/null || true
    for _ in $(seq 20); do pgrep -x Snuffler >/dev/null || return 0; sleep 0.5; done
    pkill -x Snuffler || true
}

# 1. A plain double-click: the empty window.
open -n "$APP" || fail "open could not launch Snuffler.app"
sleep 5
pgrep -x Snuffler >/dev/null || fail "Snuffler quit or crashed right after a plain launch"
shot 1-empty
quit

# 2. Two workbooks handed over by Finder (application:openURLs:), then SNUFF
#    and, at xsmall, SQUISH -- clicked by the app itself through its self-test
#    hook, exactly as the buttons would be.
REPORT="$WORK/report.txt"
if ! open -n -a "$APP" \
        --env SNUFFLER_SELFTEST=bust,xsmall \
        --env SNUFFLER_SELFTEST_REPORT="$REPORT" \
        "$WORK/ghosts.xlsx" "$WORK/emf-heavy.xlsm"; then
    # `open --env` needs a recent macOS. Without it, start the binary
    # directly; files then arrive on the command line instead of from Finder.
    echo "::warning title=GUI smoke test::open --env failed; launching the binary directly"
    SNUFFLER_SELFTEST=bust,xsmall SNUFFLER_SELFTEST_REPORT="$REPORT" \
        "$APP/Contents/MacOS/Snuffler" "$WORK/ghosts.xlsx" "$WORK/emf-heavy.xlsm" &
fi
for _ in $(seq 120); do [ -s "$REPORT" ] && break; sleep 0.5; done
sleep 1
shot 2-done
running=$(pgrep -x Snuffler >/dev/null && echo yes || echo no)
quit

[ -s "$REPORT" ] || fail "no self-test report within 60 s (Snuffler still running: $running)"
echo "--- what the result panel said"
cat "$REPORT"
while IFS= read -r line; do echo "::notice title=Result panel::$line"; done < "$REPORT"

grep -q '^stage=Done$' "$REPORT" || fail "the window did not finish in the Done state"
# 306 ghosts in ghosts.xlsx plus 2 in emf-heavy.xlsm.
grep -q '308 ghost images removed' "$REPORT" || fail "unexpected ghost count in the result panel"

for out in "ghosts (cleaned).xlsx" "emf-heavy (cleaned).xlsm"; do
    python3 -c 'import sys, zipfile; z = zipfile.ZipFile(sys.argv[1]); assert z.testzip() is None; print("ok  ", sys.argv[1].rsplit("/", 1)[-1], len(z.namelist()), "parts")' "$WORK/$out" \
        || fail "cleaned file missing or unreadable: $out"
done
echo "::notice title=GUI smoke test::passed"
