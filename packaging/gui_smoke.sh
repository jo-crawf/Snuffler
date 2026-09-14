#!/bin/bash
# Opens the built app the way Finder does and has it press its own buttons,
# screenshotting as it goes. This is the only place the window itself is ever
# exercised, so it runs in CI on every push.
#
# Needs a logged-in GUI session, which GitHub's macOS runners have.
set -euo pipefail
cd "$(dirname "$0")/.."

APP="$PWD/dist/Snuffler.app"
SHOTS="$PWD/dist/screens"
mkdir -p "$SHOTS"
WORK=$(mktemp -d)
cp conformance/fixtures/ghosts.xlsx conformance/fixtures/emf-heavy.xlsm "$WORK/"

shot() {
    # Screen recording can be refused on a runner; a missing picture is not
    # a failure of the app.
    screencapture -x "$SHOTS/$1.png" 2>/dev/null || echo "screencapture unavailable for $1"
}

quit() {
    osascript -e 'quit app "Snuffler"' 2>/dev/null || true
    for _ in $(seq 20); do pgrep -x Snuffler >/dev/null || return 0; sleep 0.5; done
    pkill -x Snuffler || true
}

# 1. A plain double-click: the empty window.
open -n "$APP"
sleep 5
pgrep -x Snuffler >/dev/null || { echo "Snuffler did not stay running"; exit 1; }
shot 1-empty
quit

# 2. Two workbooks handed over by Finder (application:openURLs:), then BUST
#    and, at xsmall, SQUISH -- clicked by the app itself through its self-test
#    hook, exactly as the buttons would be.
REPORT="$WORK/report.txt"
open -n -a "$APP" \
    --env SNUFFLER_SELFTEST=bust,xsmall \
    --env SNUFFLER_SELFTEST_REPORT="$REPORT" \
    "$WORK/ghosts.xlsx" "$WORK/emf-heavy.xlsm"
for _ in $(seq 120); do [ -s "$REPORT" ] && break; sleep 0.5; done
sleep 1
shot 2-done
quit

echo "--- what the result panel said"
cat "$REPORT" || { echo "no self-test report: the window never finished"; exit 1; }
grep -q '^stage=Done$' "$REPORT"
grep -q '306 ghost images removed' "$REPORT"

for out in "ghosts (cleaned).xlsx" "emf-heavy (cleaned).xlsm"; do
    python3 -c 'import sys, zipfile; z = zipfile.ZipFile(sys.argv[1]); assert z.testzip() is None; print("ok  ", sys.argv[1].rsplit("/", 1)[-1], len(z.namelist()), "parts")' "$WORK/$out"
done
