#!/bin/sh
# The [[build]] step Herdr runs after installing or updating the plugin.
#
# Fast path: download the prebuilt binary built from exactly this pane-picker/ tree for this
# platform, check its SHA-256, and put it at target/release/pane-picker, where the manifest points.
# The key is the git tree hash of pane-picker/ in this checkout, so the binary always matches the
# source that was installed; CI publishes one per tree that reaches main
# (.github/workflows/pane-picker-binaries.yml).
#
# Fallback, on any miss (no git, no binary for this tree or platform, a failed download or
# checksum, a binary that does not run here): build from source with cargo.
#
# PANE_PICKER_ROOT / PANE_PICKER_OUT / PANE_PICKER_BASE_URL / PANE_PICKER_KEY override the inputs
# so tests can run every path without the network.
set -u

repo="TakashiAihara/herdr-plugins"
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root="${PANE_PICKER_ROOT:-$script_dir/..}"
out="${PANE_PICKER_OUT:-$root/target/release/pane-picker}"
base_url="${PANE_PICKER_BASE_URL:-https://github.com/$repo/releases/download/pane-picker-binaries}"

have() { command -v "$1" >/dev/null 2>&1; }

build_from_source() {
  # Herdr started from a desktop launcher may not have the login shell's PATH.
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  if ! have cargo; then
    echo "pane-picker: no prebuilt binary and no cargo to build one. Install Rust from https://rustup.rs, then run: herdr plugin update" >&2
    exit 1
  fi
  cd "$root" && exec cargo build --release --locked
}

fallback() {
  echo "pane-picker: $1; building from source." >&2
  [ -n "${tmp:-}" ] && rm -rf "$tmp"
  build_from_source
}

download() {
  if have curl; then curl -fsSL --max-time 60 -o "$2" "$1" 2>/dev/null
  elif have wget; then wget -q -T 60 -O "$2" "$1" 2>/dev/null
  else return 127; fi
}

sha256_of() {
  if have sha256sum; then sha256sum "$1" | awk '{print $1}'
  elif have shasum; then shasum -a 256 "$1" | awk '{print $1}'
  else return 127; fi
}

case "$(uname -s 2>/dev/null)/$(uname -m 2>/dev/null)" in
  Darwin/arm64|Darwin/aarch64) triple=aarch64-apple-darwin ;;
  Darwin/x86_64) triple=x86_64-apple-darwin ;;
  Linux/x86_64|Linux/amd64) triple=x86_64-unknown-linux-musl ;;
  Linux/aarch64|Linux/arm64) triple=aarch64-unknown-linux-musl ;;
  *) fallback "no prebuilt binary for $(uname -s)/$(uname -m)" ;;
esac

key="${PANE_PICKER_KEY:-}"
if [ -z "$key" ]; then
  have git || fallback "git is not available to identify this checkout"
  key=$(git -C "$root" rev-parse HEAD:pane-picker 2>/dev/null) || fallback "not a git checkout"
fi
key=$(printf '%s' "$key" | cut -c1-12)
asset="pane-picker-$key-$triple"

tmp=$(mktemp -d 2>/dev/null) || fallback "cannot create a temporary directory"
download "$base_url/$asset" "$tmp/$asset" || fallback "no prebuilt binary $asset"
download "$base_url/$asset.sha256" "$tmp/$asset.sha256" || fallback "no checksum for $asset"
want=$(awk '{print $1}' "$tmp/$asset.sha256")
got=$(sha256_of "$tmp/$asset") || fallback "no sha256 tool"
[ -n "$want" ] && [ "$want" = "$got" ] || fallback "checksum mismatch for $asset"
chmod +x "$tmp/$asset"
[ "$("$tmp/$asset" enter w0:p0 2>/dev/null)" = accept ] || fallback "$asset does not run here"

mkdir -p "$(dirname "$out")" && mv -f "$tmp/$asset" "$out" || fallback "cannot install to $out"
rm -rf "$tmp"
echo "pane-picker: installed prebuilt $asset"
