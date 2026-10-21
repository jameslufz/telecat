#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
scenario=${1:-01-welcome-empty}
case "$scenario" in
  smoke|01-welcome-empty|02-picker-mysql|20-welcome-list|03-form-mysql|50-form-testing|30-form-pg|04-ws-mysql|05-query-current|06-query-all-rowselected|07-table-data|08-table-filter|09-structure|10-index|21-safe-prompt|22-pg-table|23-pg-structure|24-pg-index|51-data-staged|52-filter|53-structure-staged|54-index-staged|55-new-table|56-drop-truncate|11-two-tables|57-running|58-empty-table|59-pg-empty-table|95-rail|a1-preview|a2-switched|a3-shortcuts|c1-lock|d4-reconnecting|d5-disconnected|perf-1000) ;;
  *) printf 'Unknown scenario: %s\n' "$scenario" >&2; exit 2 ;;
esac
mkdir -p .cache/tmp
TELECAT_USER_DATA=$(mktemp -d "$PWD/.cache/scenario.XXXXXX")
export TELECAT_USER_DATA
export TELECAT_SCENARIO="$scenario" TELECAT_NO_KEYCHAIN=1 TELECAT_TEST_DB_ONLY=1
# Test windows open on this display (GPUI top-left global coordinates) without taking focus.
export TELECAT_TEST_SCREEN="${TELECAT_TEST_SCREEN:-168,-1080,1920,1080}"
export TMPDIR="$PWD/.cache/tmp"
printf 'PROFILE %s\n' "$TELECAT_USER_DATA"
exec ./target/release/telecat-native
