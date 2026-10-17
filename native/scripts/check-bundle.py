#!/usr/bin/env python3
"""Fail packaging on missing resources, wrong identity or a non-system dynamic dependency."""
import pathlib
import plistlib
import subprocess
import sys

bundle = pathlib.Path(sys.argv[1])
contents = bundle / "Contents"
info = plistlib.loads((contents / "Info.plist").read_bytes())
assert bundle.name == "Telecat.app"
assert info["CFBundleName"] == "Telecat"
assert info["CFBundleDisplayName"] == "Telecat"
# These internal names stay stable to preserve the installed app's identity.
assert info["CFBundleIdentifier"] == "dev.telecat.native"
assert info["CFBundleExecutable"] == "telecat-native"
assert info["CFBundleShortVersionString"] == "0.1.0"
binary = contents / "MacOS" / info["CFBundleExecutable"]
assert binary.stat().st_size > 1_000_000
assert subprocess.check_output(["lipo", "-archs", str(binary)], text=True).strip() == "arm64"
icon = (contents / "Resources" / info["CFBundleIconFile"]).read_bytes()
assert icon[:4] == b"icns" and int.from_bytes(icon[4:8], "big") == len(icon)
assert (contents / "Resources/THIRD-PARTY-NOTICES.txt").stat().st_size > 1000
libraries = subprocess.check_output(["otool", "-L", str(binary)], text=True)
print(libraries, end="")
for entry in libraries.splitlines()[1:]:
    path = entry.strip().split(" (", 1)[0]
    assert path.startswith(("/System/Library/", "/usr/lib/")), f"Non-system library: {path}"
load_commands = subprocess.check_output(["otool", "-l", str(binary)], text=True)
assert "LC_RPATH" not in load_commands, "Unexpected external runtime search path"
print("Telecat bundle name, stable identity, arm64, icon, notices and system-only dynamic libraries: OK")
