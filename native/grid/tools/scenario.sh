#!/bin/sh
set -eu
cd "$(dirname "$0")/../.."
. scripts/env.sh
mkdir -p grid/.profiles
TELECAT_USER_DATA=$(mktemp -d "$PWD/grid/.profiles/scenario.XXXXXX")
export TELECAT_USER_DATA
TELECAT_SCENARIO=${1:-grid-07}
export TELECAT_SCENARIO
printf 'PROFILE %s\n' "$TELECAT_USER_DATA"
exec ./target/release/examples/grid-scenario
