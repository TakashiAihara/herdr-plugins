#!/usr/bin/env bash
# Runs scripts/fetch-or-build.sh against a local file:// release, a fake uname and a fake cargo.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
script="$here/../scripts/fetch-or-build.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/bin" "$tmp/rel" "$tmp/home"
cat > "$tmp/bin/uname" <<'FAKE'
#!/bin/sh
case "$1" in -s) echo "${FAKE_OS:-Linux}" ;; -m) echo "${FAKE_ARCH:-x86_64}" ;; esac
FAKE
cat > "$tmp/bin/cargo" <<'FAKE'
#!/bin/sh
echo "cargo $* in $(pwd) target=${CARGO_TARGET_DIR:-}" >> "$FAKE_CARGO_LOG"
td=
while [ $# -gt 0 ]; do [ "$1" = --target-dir ] && td=$2; shift; done
[ -n "$td" ] && mkdir -p "$td/release" && echo built > "$td/release/agent-search"
exit 0
FAKE
chmod +x "$tmp/bin/uname" "$tmp/bin/cargo"

asset() { # asset <key> <triple> <body>
  printf '#!/bin/sh\n%s\n' "$3" > "$tmp/rel/agent-search-$1-$2"
  (cd "$tmp/rel" && { sha256sum "agent-search-$1-$2" 2>/dev/null || shasum -a 256 "agent-search-$1-$2"; } > "agent-search-$1-$2.sha256")
}
asset abcdef123456 x86_64-unknown-linux-musl '[ "$1" = enter ] && echo accept'
asset abcdef123456 aarch64-apple-darwin 'exit 1'

fails=0
n=0
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1"; echo "  want: $3"; echo "  got:  $2"; fails=$((fails + 1)); fi
}
fetch() {
  n=$((n + 1))
  export FAKE_CARGO_LOG="$tmp/cargo$n.log" AGENT_SEARCH_OUT="$tmp/out$n/agent-search" AGENT_SEARCH_ROOT="${ROOT:-$tmp/root$n}"
  out="$tmp/fetch$n.txt"
  mkdir -p "$AGENT_SEARCH_ROOT"
  : > "$FAKE_CARGO_LOG"
  HOME="$tmp/home" PATH="$tmp/bin:$PATH" AGENT_SEARCH_BASE_URL="file://$tmp/rel" sh "$script" > "$out" 2>&1
}
cargo_calls() { wc -l < "$FAKE_CARGO_LOG" | tr -d ' '; }

AGENT_SEARCH_KEY=abcdef1234567890 fetch; rc=$?
check "a matching prebuilt binary is installed" \
  "$(echo "rc=$rc"; cat "$out"; "$AGENT_SEARCH_OUT" enter x; cargo_calls)" \
  "rc=0
agent-search: installed prebuilt agent-search-abcdef123456-x86_64-unknown-linux-musl
accept
0"

AGENT_SEARCH_KEY=000000000000 fetch
check "an unknown tree builds from source" \
  "$(head -1 "$out"; cut -d' ' -f1-4 "$FAKE_CARGO_LOG")" \
  "agent-search: no prebuilt binary agent-search-000000000000-x86_64-unknown-linux-musl; building from source.
cargo build --release --locked"

good="$tmp/rel/agent-search-abcdef123456-x86_64-unknown-linux-musl.sha256"
cp "$good" "$tmp/good.sha256"
printf 'deadbeef  x\n' > "$good"
AGENT_SEARCH_KEY=abcdef123456 fetch
check "a checksum mismatch builds from source and installs nothing" \
  "$(head -1 "$out"; [ -e "$AGENT_SEARCH_OUT" ] && cat "$AGENT_SEARCH_OUT"; cargo_calls)" \
  "agent-search: checksum mismatch for agent-search-abcdef123456-x86_64-unknown-linux-musl; building from source.
built
1"
cp "$tmp/good.sha256" "$good"

FAKE_OS=Darwin FAKE_ARCH=arm64 AGENT_SEARCH_KEY=abcdef123456 fetch
check "a binary that does not run here builds from source and installs nothing" \
  "$(head -1 "$out"; [ -e "$AGENT_SEARCH_OUT" ] && cat "$AGENT_SEARCH_OUT"; cargo_calls)" \
  "agent-search: agent-search-abcdef123456-aarch64-apple-darwin does not run here; building from source.
built
1"

FAKE_OS=Linux FAKE_ARCH=riscv64 AGENT_SEARCH_KEY=abcdef123456 fetch
check "an unsupported platform builds from source" "$(head -1 "$out"; cargo_calls)" \
  "agent-search: no prebuilt binary for Linux/riscv64; building from source.
1"

fetch
check "outside a git checkout it builds from source" "$(head -1 "$out"; cargo_calls)" \
  "agent-search: not a git checkout; building from source.
1"

mkdir -p "$tmp/repo/agent-search" && echo x > "$tmp/repo/agent-search/f" && echo target/ > "$tmp/repo/agent-search/.gitignore" && echo y > "$tmp/repo/other"
git -C "$tmp/repo" init -q && git -C "$tmp/repo" add -A &&
  GIT_CONFIG_GLOBAL=/dev/null git -C "$tmp/repo" -c user.email=t@example.com -c user.name=t commit -qm t
tree=$(git -C "$tmp/repo" rev-parse HEAD:agent-search | cut -c1-12)
asset "$tree" x86_64-unknown-linux-musl '[ "$1" = enter ] && echo accept'
ROOT="$tmp/repo/agent-search" fetch
check "the key is the git tree hash of agent-search/, read from inside the plugin directory" \
  "$(head -1 "$out")" "agent-search: installed prebuilt agent-search-$tree-x86_64-unknown-linux-musl"

echo z >> "$tmp/repo/agent-search/f"
ROOT="$tmp/repo/agent-search" fetch
check "a working copy with uncommitted changes builds from source instead of downloading" \
  "$(head -1 "$out"; cargo_calls)" "agent-search: agent-search/ has uncommitted changes; building from source.
1"
git -C "$tmp/repo" checkout -q -- agent-search/f
echo new > "$tmp/repo/agent-search/untracked"
ROOT="$tmp/repo/agent-search" fetch
check "an untracked file counts as a change too" "$(head -1 "$out")" "agent-search: agent-search/ has uncommitted changes; building from source."
rm "$tmp/repo/agent-search/untracked"
echo y > "$tmp/repo/other" && echo w >> "$tmp/repo/other"
ROOT="$tmp/repo/agent-search" fetch
check "a change outside agent-search/ does not matter" "$(head -1 "$out")" "agent-search: installed prebuilt agent-search-$tree-x86_64-unknown-linux-musl"

CARGO_TARGET_DIR="$tmp/elsewhere" AGENT_SEARCH_KEY=000000000000 fetch; rc=$?
check "the source build pins the target dir and copies the binary to where the manifest points" \
  "$(echo "rc=$rc"; sed -e 's/ in .*//' -e "s|$AGENT_SEARCH_ROOT|ROOT|" "$FAKE_CARGO_LOG"; cat "$AGENT_SEARCH_OUT"; [ -e "$tmp/elsewhere" ] && echo moved)" \
  "rc=0
cargo build --release --locked --target-dir ROOT/target
built"

n=$((n + 1))
export FAKE_CARGO_LOG="$tmp/cargo$n.log" AGENT_SEARCH_OUT="$tmp/out$n/agent-search" AGENT_SEARCH_ROOT="$tmp/root$n"
mkdir -p "$AGENT_SEARCH_ROOT"
env -u HOME PATH="$tmp/bin:$PATH" AGENT_SEARCH_KEY=000000000000 AGENT_SEARCH_BASE_URL="file://$tmp/rel" sh "$script" > "$tmp/fetch$n.txt" 2>&1; rc=$?
check "the source build does not need HOME" "rc=$rc $(cat "$AGENT_SEARCH_OUT")" "rc=0 built"

printf '#!/bin/sh\nexit 1\n' > "$tmp/bin/cargo"
AGENT_SEARCH_KEY=000000000000 fetch; rc=$?
check "no prebuilt binary and a failing cargo fails" "rc=$rc" "rc=1"

if [ "$fails" -gt 0 ]; then echo "$fails failed"; exit 1; fi
echo "all passed"
