#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
bundle='dist/Telecat.app'
if [ ! -x "$bundle/Contents/MacOS/telecat-native" ]; then
  echo 'Run ./scripts/package.sh to build dist/Telecat.app first.' >&2
  exit 1
fi
mkdir -p .cache
TELECAT_USER_DATA=$(mktemp -d "$PWD/.cache/smoke.XXXXXX")
export TELECAT_USER_DATA TELECAT_SCENARIO=smoke
export TELECAT_NO_KEYCHAIN=1 TELECAT_TEST_DB_ONLY=1
export TMPDIR="$PWD/.cache/tmp"
mkdir -p "$TMPDIR"
printf 'PROFILE %s\n' "$TELECAT_USER_DATA"
exec "$bundle/Contents/MacOS/telecat-native"
