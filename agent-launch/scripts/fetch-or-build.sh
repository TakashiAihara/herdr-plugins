#!/bin/sh
# The [[build]] step Herdr runs after installing or updating the plugin.
#
# Fast path: download the prebuilt binary built from exactly this agent-launch/ tree for this
# platform, check its SHA-256, and put it at target/release/agent-launch, where the manifest points.
# The key is the git tree hash of agent-launch/ in this checkout, so the binary always matches the
# source that was installed; CI publishes one per tree that reaches main
# (.github/workflows/agent-launch-binaries.yml).
#
# Fallback, on any miss (no git, no binary for this tree or platform, a failed download or
# checksum, a binary that does not run here): build from source with cargo.
#
# AGENT_LAUNCH_ROOT / AGENT_LAUNCH_OUT / AGENT_LAUNCH_BASE_URL / AGENT_LAUNCH_KEY override the inputs
# so tests can run every path without the network.
set -u

repo="TakashiAihara/herdr-plugins"
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH='' cd -- "${AGENT_LAUNCH_ROOT:-$script_dir/..}" 2>/dev/null && pwd) || {
  echo "agent-launch: cannot enter ${AGENT_LAUNCH_ROOT:-$script_dir/..}" >&2
  exit 1
}
out="${AGENT_LAUNCH_OUT:-$root/target/release/agent-launch}"
base_url="${AGENT_LAUNCH_BASE_URL:-https://github.com/$repo/releases/download/agent-launch-binaries}"

have() { command -v "$1" >/dev/null 2>&1; }

build_from_source() {
  # Herdr started from a desktop launcher may not have the login shell's PATH.
  [ -n "${HOME:-}" ] && [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  if ! have cargo; then
    echo "agent-launch: no prebuilt binary and no cargo to build one. Install Rust from https://rustup.rs, then run: herdr plugin update" >&2
    exit 1
  fi
  # --target with cargo's own host triple: the install build is for this machine, whatever a
  # build.target in the user's Cargo config or CARGO_BUILD_TARGET says.
  host=$(cargo -vV 2>/dev/null | sed -n 's/^host: //p')
  [ -n "$host" ] || {
    echo "agent-launch: cargo -vV names no host triple to build for" >&2
    exit 1
  }
  # --target-dir keeps the build under this plugin's target/, whatever CARGO_TARGET_DIR says.
  (cd "$root" && cargo build --release --locked --target "$host" --target-dir "$root/target") || exit 1
  # A --target build lands under <target-dir>/<triple>/release, never where the manifest points.
  built="$root/target/$host/release/agent-launch"
  [ -x "$built" ] || {
    echo "agent-launch: cargo build finished without a runnable $built" >&2
    exit 1
  }
  { mkdir -p "$(dirname "$out")" && cp -f "$built" "$out"; } || {
    echo "agent-launch: cannot copy $built to $out" >&2
    exit 1
  }
  exit 0
}

fallback() {
  echo "agent-launch: $1; building from source." >&2
  [ -n "${tmp:-}" ] && rm -rf "$tmp"
  build_from_source
}

download() {
  if have curl; then curl -fsSL --max-time 60 -o "$2" "$1" 2>/dev/null
  elif have wget; then wget -q -T 60 -t 1 -O "$2" "$1" 2>/dev/null
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

key="${AGENT_LAUNCH_KEY:-}"
if [ -z "$key" ]; then
  have git || fallback "git is not available to identify this checkout"
  # The tree of this directory itself, wherever it sits in the checkout: it is the source the
  # binary must match, and CI names binaries by the same tree hash.
  key=$(git -C "$root" rev-parse HEAD:./ 2>/dev/null) || fallback "not a git checkout"
  # A linked working copy with edits is not the committed tree the binary was built from.
  # --untracked-files=all so a user's status.showUntrackedFiles=no does not hide new files.
  dirty=$(git -C "$root" status --porcelain --untracked-files=all -- . 2>/dev/null) || fallback "cannot read the git status"
  [ -z "$dirty" ] || fallback "agent-launch/ has uncommitted changes"
fi
key=$(printf '%s' "$key" | cut -c1-12)
asset="agent-launch-$key-$triple"

# Downloaded next to where it is installed, not under TMPDIR: the binary is run once before it is
# installed, and a temporary directory can be mounted noexec.
dest=$(dirname "$out")
# What an interrupted install left behind goes first. mktemp makes the directory for this run
# private (0700) under a name nobody could prepare.
rm -rf "$dest"/.fetch.* 2>/dev/null
mkdir -p "$dest" 2>/dev/null && tmp=$(mktemp -d "$dest/.fetch.XXXXXX" 2>/dev/null) ||
  fallback "cannot create a temporary directory in $dest"
download "$base_url/$asset" "$tmp/$asset" || fallback "no prebuilt binary $asset"
download "$base_url/$asset.sha256" "$tmp/$asset.sha256" || fallback "no checksum for $asset"
want=$(awk '{print $1}' "$tmp/$asset.sha256")
got=$(sha256_of "$tmp/$asset") || fallback "no sha256 tool"
[ -n "$want" ] && [ "$want" = "$got" ] || fallback "checksum mismatch for $asset"
chmod +x "$tmp/$asset"
case "$("$tmp/$asset" version 2>/dev/null)" in
  "agent-launch "*) ;;
  *) fallback "$asset does not run here" ;;
esac

mv -f "$tmp/$asset" "$out" || fallback "cannot install to $out"
rm -rf "$tmp"
echo "agent-launch: installed prebuilt $asset"
