#!/bin/sh
set -eu
cd "$(dirname "$0")/../.."
. scripts/env.sh
mkdir -p editor/.profiles
TELECAT_USER_DATA=$(mktemp -d "$PWD/editor/.profiles/scenario.XXXXXX")
export TELECAT_USER_DATA
export TELECAT_SCENARIO="${1:-editor-05}"
printf 'PROFILE %s\n' "$TELECAT_USER_DATA"
exec target/release/examples/telecat-editor-scenario
