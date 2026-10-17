# Source from native/: all toolchain, Cargo, and build writes stay inside native/.
if [ ! -f Cargo.toml ] || [ ! -f rust-toolchain.toml ]; then
  printf '%s\n' 'Run: cd native; source scripts/env.sh' >&2
  return 1
fi
export CARGO_HOME="$PWD/.cache/cargo"
export RUSTUP_HOME="$PWD/.cache/rustup"
export TMPDIR="$PWD/.cache/tmp"
mkdir -p "$TMPDIR"
export CARGO_BUILD_JOBS=4  # keep memory use reasonable on 8 GB Macs
