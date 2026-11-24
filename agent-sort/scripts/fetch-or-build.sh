#!/bin/sh
# The [[build]] step Herdr runs after installing or updating the plugin.
#
# Fast path: download the prebuilt binary built from exactly this agent-sort/ tree for this
# platform, check its SHA-256, and put it at target/release/agent-sort, where the manifest points.
# The key is the git tree hash of agent-sort/ in this checkout, so the binary always matches the
# source that was installed; CI publishes one per tree that reaches main
# (.github/workflows/agent-sort-binaries.yml).
#
# Fallback, on any miss (no git, no binary for this tree or platform, a failed download or
# checksum, a binary that does not run here): build from source with cargo.
#
# AGENT_SORT_ROOT / AGENT_SORT_OUT / AGENT_SORT_BASE_URL / AGENT_SORT_KEY override the inputs
# so tests can run every path without the network.
set -u

repo="TakashiAihara/herdr-plugins"
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH='' cd -- "${AGENT_SORT_ROOT:-$script_dir/..}" 2>/dev/null && pwd) || {
  echo "agent-sort: cannot enter ${AGENT_SORT_ROOT:-$script_dir/..}" >&2
  exit 1
}
out="${AGENT_SORT_OUT:-$root/target/release/agent-sort}"
base_url="${AGENT_SORT_BASE_URL:-https://github.com/$repo/releases/download/agent-sort-binaries}"

have() { command -v "$1" >/dev/null 2>&1; }

build_from_source() {
  # Herdr started from a desktop launcher may not have the login shell's PATH.
  [ -n "${HOME:-}" ] && [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  if ! have cargo; then
    echo "agent-sort: no prebuilt binary and no cargo to build one. Install Rust from https://rustup.rs, then run: herdr plugin update" >&2
    exit 1
  fi
  # --target with cargo's own host triple: the install build is for this machine, whatever a
  # build.target in the user's Cargo config or CARGO_BUILD_TARGET says.
  host=$(cargo -vV 2>/dev/null | sed -n 's/^host: //p')
  [ -n "$host" ] || {
    echo "agent-sort: cargo -vV names no host triple to build for" >&2
    exit 1
  }
  # --target-dir keeps the build under this plugin's target/, whatever CARGO_TARGET_DIR says.
  (cd "$root" && cargo build --release --locked --target "$host" --target-dir "$root/target") || exit 1
  # A --target build lands under <target-dir>/<triple>/release, never where the manifest points.
  built="$root/target/$host/release/agent-sort"
  [ -x "$built" ] || {
    echo "agent-sort: cargo build finished without a runnable $built" >&2
    exit 1
  }
  { mkdir -p "$(dirname "$out")" && cp -f "$built" "$out"; } || {
    echo "agent-sort: cannot copy $built to $out" >&2
    exit 1
  }
  exit 0
}

fallback() {
  echo "agent-sort: $1; building from source." >&2
  [ -n "${tmp:-}" ] && rm -rf "$tmp"
  build_from_source
}

download() {
  if have curl; then curl -fsSL --max-time 60 -o "$2" "$1" 2>/dev/null
  elif have wget; then wget -q -T 60 --tries=1 -O "$2" "$1" 2>/dev/null
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

key="${AGENT_SORT_KEY:-}"
if [ -z "$key" ]; then
  have git || fallback "git is not available to identify this checkout"
  # The tree of this directory itself, wherever it sits in the checkout: it is the source the
  # binary must match, and CI names binaries by the same tree hash.
  key=$(git -C "$root" rev-parse HEAD:./ 2>/dev/null) || fallback "not a git checkout"
  # A linked working copy with edits is not the committed tree the binary was built from.
  # --untracked-files=all so a user's status.showUntrackedFiles=no does not hide new files.
  dirty=$(git -C "$root" status --porcelain --untracked-files=all -- . 2>/dev/null) || fallback "cannot read the git status"
  [ -z "$dirty" ] || fallback "agent-sort/ has uncommitted changes"
fi
key=$(printf '%s' "$key" | cut -c1-12)
asset="agent-sort-$key-$triple"

# Downloaded next to where it is installed, not under TMPDIR: the binary is run once before it is
# installed, and a temporary directory can be mounted noexec.
dest=$(dirname "$out")
# One fixed name: what an interrupted install left behind is reused and removed by the next
# install that downloads.
# NOTE: two installs of this plugin at once share it and can overwrite each other's download.
tmp="$dest/.fetch"
mkdir -p "$tmp" 2>/dev/null || fallback "cannot create $tmp"
download "$base_url/$asset" "$tmp/$asset" || fallback "no prebuilt binary $asset"
download "$base_url/$asset.sha256" "$tmp/$asset.sha256" || fallback "no checksum for $asset"
want=$(awk '{print $1}' "$tmp/$asset.sha256")
got=$(sha256_of "$tmp/$asset") || fallback "no sha256 tool"
[ -n "$want" ] && [ "$want" = "$got" ] || fallback "checksum mismatch for $asset"
chmod +x "$tmp/$asset"
[ "$("$tmp/$asset" probe 2>/dev/null)" = ok ] || fallback "$asset does not run here"

mv -f "$tmp/$asset" "$out" || fallback "cannot install to $out"
rm -rf "$tmp"
echo "agent-sort: installed prebuilt $asset"
