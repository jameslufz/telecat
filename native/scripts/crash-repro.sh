#!/bin/bash
# Touch Bar / KVO crash repro. Builds the harness into
# .cache/, runs the pre-fix teardown order (expected to abort with NSRangeException) and
# the fixed order (expected to survive). Needs a Mac with a Touch Bar; takes focus briefly.
set -u
native_root="$(cd "$(dirname "$0")/.." && pwd)"
out="$native_root/.cache/crash-repro"
mkdir -p "$out"
clang -fobjc-arc "$native_root/scripts/crash-repro/touchbar_kvo.m" -framework Cocoa -o "$out/touchbar_kvo"
# macOS only lets an app activate itself cooperatively; bring the harness to the front.
run() {
  "$out/touchbar_kvo" "$1" "$2" >"$out/$1.log" 2>&1 &
  local pid=$!
  for _ in 1 2 3 4 5 6; do
    sleep 0.5
    osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $pid) to true" >/dev/null 2>&1 || true
  done
  wait "$pid"
}
run old 3
old=$?
run fixed 10
fixed=$?
echo "old teardown:   exit $old  ($(grep -m1 -o "Terminating app due to uncaught exception '[A-Za-z]*'" "$out/old.log" || grep -m1 survived "$out/old.log"))"
echo "fixed teardown: exit $fixed ($(grep -m1 survived "$out/fixed.log" || grep -m1 -o "Terminating app[^,]*" "$out/fixed.log"))"
if grep -q "before teardown: no" "$out/old.log"; then
  echo "inconclusive: AppKit's Touch Bar finder did not observe the window (no Touch Bar, or it is off/idle)"
fi
[ "$fixed" -eq 0 ]
