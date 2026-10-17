#!/bin/bash
# Build a drag-install image without mounting it or installing anything.
set -euo pipefail
native_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$native_root"
if [[ $# -gt 1 ]] || [[ $# -eq 1 && "$1" != --no-build ]]; then
  echo 'Usage: ./scripts/dmg.sh [--no-build]' >&2
  exit 2
fi
if [[ "$(uname -s)" != Darwin ]]; then
  echo 'DMG packaging requires macOS.' >&2
  exit 1
fi
if [[ "${1:-}" != --no-build ]]; then
  "$native_root/scripts/package.sh"
fi

bundle="$native_root/dist/Telecat.app"
if [[ ! -d "$bundle" ]]; then
  echo 'Missing dist/Telecat.app; run ./scripts/package.sh or omit --no-build.' >&2
  exit 1
fi
python3 scripts/check-bundle.py "$bundle"
codesign --verify --deep --strict --verbose=2 "$bundle"
version="$(python3 - "$bundle/Contents/Info.plist" <<'PY'
import plistlib
import re
import sys

with open(sys.argv[1], 'rb') as file:
    version = plistlib.load(file)['CFBundleShortVersionString']
if not isinstance(version, str) or not re.fullmatch(r'[0-9]+(?:\.[0-9]+){0,2}', version):
    sys.exit('Invalid CFBundleShortVersionString for the DMG filename')
print(version)
PY
)"

mkdir -p "$native_root/.cache" "$native_root/dist"
stage="$(mktemp -d "$native_root/.cache/dmg.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
export TMPDIR="$stage/tmp"
mkdir -p "$TMPDIR" "$stage/volume"
ditto "$bundle" "$stage/volume/Telecat.app"
ln -s /Applications "$stage/volume/Applications"
image_name="Telecat-$version-arm64.dmg"
hdiutil create -srcfolder "$stage/volume" -volname Telecat -fs HFS+ \
  -format UDZO "$stage/$image_name"
hdiutil verify "$stage/$image_name"
# Only replace this version's generated image after creation and verification succeed.
mv -f "$stage/$image_name" "$native_root/dist/$image_name"
printf '\nDMG: %s/dist/%s\n' "$native_root" "$image_name"
