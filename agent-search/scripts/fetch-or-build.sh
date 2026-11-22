#!/bin/sh
# The [[build]] step Herdr runs after installing or updating the plugin.
#
# Fast path: download the prebuilt binary built from exactly this agent-search/ tree for this
# platform, check its SHA-256, and put it at target/release/agent-search, where the manifest points.
# The key is the git tree hash of agent-search/ in this checkout, so the binary always matches the
# source that was installed; CI publishes one per tree that reaches main
# (.github/workflows/agent-search-binaries.yml).
#
# Fallback, on any miss (no git, no binary for this tree or platform, a failed download or
# checksum, a binary that does not run here): build from source with cargo.
#
# AGENT_SEARCH_ROOT / AGENT_SEARCH_OUT / AGENT_SEARCH_BASE_URL / AGENT_SEARCH_KEY override the inputs
# so tests can run every path without the network.
set -u

repo="TakashiAihara/herdr-plugins"
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH='' cd -- "${AGENT_SEARCH_ROOT:-$script_dir/..}" 2>/dev/null && pwd) || {
  echo "agent-search: cannot enter ${AGENT_SEARCH_ROOT:-$script_dir/..}" >&2
  exit 1
}
out="${AGENT_SEARCH_OUT:-$root/target/release/agent-search}"
base_url="${AGENT_SEARCH_BASE_URL:-https://github.com/$repo/releases/download/agent-search-binaries}"

have() { command -v "$1" >/dev/null 2>&1; }

build_from_source() {
  # Herdr started from a desktop launcher may not have the login shell's PATH.
  [ -n "${HOME:-}" ] && [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  if ! have cargo; then
    echo "agent-search: no prebuilt binary and no cargo to build one. Install Rust from https://rustup.rs, then run: herdr plugin update" >&2
    exit 1
  fi
  # --target-dir pins the output next to the manifest's command path, whatever CARGO_TARGET_DIR says.
  # The binary's path comes from cargo's own report of this build: a build.target in the user's
  # Cargo config (or CARGO_BUILD_TARGET) puts it under target/<triple>/release, and after a later
  # cross build a binary for another target can be newer than this one, which a build with nothing
  # to do does not rewrite.
  report="$root/target/.agent-search-build.json"
  mkdir -p "$root/target" &&
    (cd "$root" && cargo build --release --locked --target-dir "$root/target" \
      --message-format=json-render-diagnostics > "$report") || exit 1
  built=$(sed -n 's/.*"executable":"\([^"]*\/agent-search\)".*/\1/p' "$report" | tail -n 1)
  [ -n "$built" ] && [ -x "$built" ] || {
    echo "agent-search: cargo build finished but its report ($report) names no runnable agent-search binary" >&2
    exit 1
  }
  # cargo may spell the path differently from $out (a symlink in $root) while naming the same file.
  if [ "$built" != "$out" ] && ! [ "$built" -ef "$out" ]; then
    { mkdir -p "$(dirname "$out")" && cp -f "$built" "$out"; } || {
      echo "agent-search: cannot copy $built to $out" >&2
      exit 1
    }
  fi
  exit 0
}

fallback() {
  echo "agent-search: $1; building from source." >&2
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

key="${AGENT_SEARCH_KEY:-}"
if [ -z "$key" ]; then
  have git || fallback "git is not available to identify this checkout"
  # The tree of this directory itself, wherever it sits in the checkout: it is the source the
  # binary must match, and CI names binaries by the same tree hash.
  key=$(git -C "$root" rev-parse HEAD:./ 2>/dev/null) || fallback "not a git checkout"
  # A linked working copy with edits is not the committed tree the binary was built from.
  # --untracked-files=all so a user's status.showUntrackedFiles=no does not hide new files.
  dirty=$(git -C "$root" status --porcelain --untracked-files=all -- . 2>/dev/null) || fallback "cannot read the git status"
  [ -z "$dirty" ] || fallback "agent-search/ has uncommitted changes"
fi
key=$(printf '%s' "$key" | cut -c1-12)
asset="agent-search-$key-$triple"

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
echo "agent-search: installed prebuilt $asset"
