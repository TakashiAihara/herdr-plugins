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
# FAKE_CARGO_TRIPLE plays a build.target in the user's Cargo config; FAKE_CARGO_NOBIN a build that
# writes nothing.
[ -n "${FAKE_CARGO_NOBIN:-}" ] && exit 0
d="$td/release"; [ -n "${FAKE_CARGO_TRIPLE:-}" ] && d="$td/$FAKE_CARGO_TRIPLE/release"
[ -n "$td" ] && mkdir -p "$d" && echo built > "$d/agent-search" && chmod +x "$d/agent-search"
# FAKE_CARGO_STALE leaves a newer binary for another target next to this one, as an earlier
# cross build would; only the reported path is this build's.
if [ -n "${FAKE_CARGO_STALE:-}" ]; then
  mkdir -p "$td/aarch64-unknown-linux-gnu/release"
  echo stale > "$td/aarch64-unknown-linux-gnu/release/agent-search"
  chmod +x "$td/aarch64-unknown-linux-gnu/release/agent-search"
fi
# FAKE_CARGO_PHYSICAL reports the path with symlinks resolved, as cargo may.
[ -n "${FAKE_CARGO_PHYSICAL:-}" ] && d=$(cd "$d" && pwd -P)
printf '{"reason":"compiler-artifact","executable":"%s"}\n' "$d/agent-search"
printf '{"reason":"build-finished","success":true}\n'
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
printf '[status]\n\tshowUntrackedFiles = no\n' > "$tmp/gitconfig"
GIT_CONFIG_GLOBAL="$tmp/gitconfig" ROOT="$tmp/repo/agent-search" fetch; rc=$?
check "an untracked file counts as a change even when git is told to hide them, and the source build installs the binary" \
  "$(echo "rc=$rc"; head -1 "$out"; cat "$AGENT_SEARCH_OUT"; cargo_calls)" \
  "rc=0
agent-search: agent-search/ has uncommitted changes; building from source.
built
1"
rm "$tmp/repo/agent-search/untracked"

pushd "$tmp/repo" > /dev/null || exit 1
ROOT=agent-search AGENT_SEARCH_KEY=000000000000 fetch; rc=$?
popd > /dev/null || exit 1
check "a relative AGENT_SEARCH_ROOT is resolved before the build" \
  "$(echo "rc=$rc"; sed -e 's/ in .*//' -e "s|$tmp/repo|REPO|" "$FAKE_CARGO_LOG"; cat "$AGENT_SEARCH_OUT")" \
  "rc=0
cargo build --release --locked --target-dir REPO/agent-search/target --message-format=json-render-diagnostics
built"
echo y > "$tmp/repo/other" && echo w >> "$tmp/repo/other"
ROOT="$tmp/repo/agent-search" fetch
check "a change outside agent-search/ does not matter" "$(head -1 "$out")" "agent-search: installed prebuilt agent-search-$tree-x86_64-unknown-linux-musl"

CARGO_TARGET_DIR="$tmp/elsewhere" AGENT_SEARCH_KEY=000000000000 fetch; rc=$?
check "the source build pins the target dir and copies the binary to where the manifest points" \
  "$(echo "rc=$rc"; sed -e 's/ in .*//' -e "s|$AGENT_SEARCH_ROOT|ROOT|" "$FAKE_CARGO_LOG"; cat "$AGENT_SEARCH_OUT"; [ -e "$tmp/elsewhere" ] && echo moved)" \
  "rc=0
cargo build --release --locked --target-dir ROOT/target --message-format=json-render-diagnostics
built"

n=$((n + 1))
export FAKE_CARGO_LOG="$tmp/cargo$n.log" AGENT_SEARCH_OUT="$tmp/out$n/agent-search" AGENT_SEARCH_ROOT="$tmp/root$n"
mkdir -p "$AGENT_SEARCH_ROOT"
env -u HOME PATH="$tmp/bin:$PATH" AGENT_SEARCH_KEY=000000000000 AGENT_SEARCH_BASE_URL="file://$tmp/rel" sh "$script" > "$tmp/fetch$n.txt" 2>&1; rc=$?
check "the source build does not need HOME" "rc=$rc $(cat "$AGENT_SEARCH_OUT")" "rc=0 built"

FAKE_CARGO_TRIPLE=x86_64-unknown-linux-gnu AGENT_SEARCH_KEY=000000000000 fetch; rc=$?
check "a build.target in the user's Cargo config still installs the binary where the manifest points" \
  "rc=$rc $(cat "$AGENT_SEARCH_OUT")" "rc=0 built"

FAKE_CARGO_STALE=1 AGENT_SEARCH_KEY=000000000000 fetch; rc=$?
check "the binary this build reported is installed, not a newer one for another target" \
  "rc=$rc $(cat "$AGENT_SEARCH_OUT")" "rc=0 built"

# The plugin reached through a symlink, installing to its default path: cargo's resolved spelling of
# that path names the same file, which is not copied onto itself.
n=$((n + 1))
mkdir -p "$tmp/real$n" && ln -s "$tmp/real$n" "$tmp/link$n"
env -u AGENT_SEARCH_OUT FAKE_CARGO_LOG="$tmp/cargo$n.log" AGENT_SEARCH_ROOT="$tmp/link$n" FAKE_CARGO_PHYSICAL=1 \
  PATH="$tmp/bin:$PATH" AGENT_SEARCH_KEY=000000000000 AGENT_SEARCH_BASE_URL="file://$tmp/rel" \
  sh "$script" > "$tmp/fetch$n.txt" 2>&1; rc=$?
check "a path cargo spells differently but that names the same file is left in place" \
  "rc=$rc $(cat "$tmp/real$n/target/release/agent-search" 2>/dev/null)" "rc=0 built"

FAKE_CARGO_NOBIN=1 AGENT_SEARCH_KEY=000000000000 fetch; rc=$?
check "a build that writes no binary fails naming it" \
  "rc=$rc $(grep -c 'names no runnable agent-search binary' "$tmp/fetch$n.txt")" "rc=1 1"

# Copied into another repo under a different directory name, the key is still this tree's hash.
mkdir -p "$tmp/other/plugins" && command cp -R "$tmp/repo/agent-search" "$tmp/other/plugins/renamed"
git -C "$tmp/other" init -q && git -C "$tmp/other" add -A &&
  GIT_CONFIG_GLOBAL=/dev/null git -C "$tmp/other" -c user.name=t -c user.email=t@t commit -qm init
ROOT="$tmp/other/plugins/renamed" fetch
check "the key is the tree of the plugin directory itself, wherever it sits" "$(head -1 "$out")" "agent-search: installed prebuilt agent-search-$tree-x86_64-unknown-linux-musl"

printf '#!/bin/sh\nexit 1\n' > "$tmp/bin/cargo"
AGENT_SEARCH_KEY=000000000000 fetch; rc=$?
check "no prebuilt binary and a failing cargo fails" "rc=$rc" "rc=1"

if [ "$fails" -gt 0 ]; then echo "$fails failed"; exit 1; fi
echo "all passed"
