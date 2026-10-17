#!/bin/bash
# Rebuild and package from any working directory. All generated files stay in native/.
set -euo pipefail
native_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$native_root"
. scripts/env.sh
if [[ "$(uname -s)" != Darwin ]]; then
  echo 'Packaging requires macOS, Xcode Command Line Tools and the pinned Rust toolchain.' >&2
  exit 1
fi
cargo build --release --locked -p telecat-app
python3 scripts/notices.py

stage="$(mktemp -d "$native_root/.cache/package.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
bundle="$stage/Telecat.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources" "$stage/AppIcon.iconset"
cp target/release/telecat-native "$bundle/Contents/MacOS/telecat-native"
chmod 755 "$bundle/Contents/MacOS/telecat-native"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" app/assets/icon.png --out "$stage/AppIcon.iconset/icon_${size}x${size}.png" >/dev/null
  doubled=$((size * 2))
  sips -z "$doubled" "$doubled" app/assets/icon.png --out "$stage/AppIcon.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
# IconServices encoding can fail in a sandbox. ICNS supports PNG resources
# directly; use the same deterministic container writer in every environment.
python3 scripts/iconset-to-icns.py "$stage/AppIcon.iconset" "$bundle/Contents/Resources/AppIcon.icns"
cp app/Info.plist "$bundle/Contents/Info.plist"
printf 'APPL????' > "$bundle/Contents/PkgInfo"
cp THIRD-PARTY-NOTICES.txt "$bundle/Contents/Resources/THIRD-PARTY-NOTICES.txt"
plutil -lint "$bundle/Contents/Info.plist"
python3 scripts/check-bundle.py "$bundle"
codesign --force --sign - --identifier dev.telecat.native --timestamp=none "$bundle"
codesign --verify --deep --strict --verbose=2 "$bundle"
mkdir -p dist
# Replace only this generated product; no installs or user-profile changes.
rm -rf 'dist/Telecat.app'
mv "$bundle" 'dist/Telecat.app'
printf '\nBundle: %s/dist/Telecat.app\n' "$native_root"
