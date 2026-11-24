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
host=x86_64-unknown-linux-gnu
if [ "$1" = -vV ]; then
  [ -n "${FAKE_CARGO_NOHOST:-}" ] || echo "host: $host"
  exit 0
fi
echo "cargo $* in $(pwd) target=${CARGO_TARGET_DIR:-}" >> "$FAKE_CARGO_LOG"
# FAKE_CARGO_TRIPLE plays a build.target in the user's Cargo config, which --target overrides.
td= tg=${FAKE_CARGO_TRIPLE:-}
while [ $# -gt 0 ]; do
  [ "$1" = --target-dir ] && td=$2
  [ "$1" = --target ] && tg=$2
  shift
done
# Like cargo: without --target-dir the build goes to CARGO_TARGET_DIR, so a script that drops the
# flag shows up as files under it.
[ -z "$td" ] && td=${CARGO_TARGET_DIR:-}
# FAKE_CARGO_NOBIN is a build that writes nothing.
[ -n "${FAKE_CARGO_NOBIN:-}" ] && exit 0
d="$td/release"; [ -n "$tg" ] && d="$td/$tg/release"
# Only a build for the host is a binary that runs here.
body=built; [ -n "$tg" ] && [ "$tg" != "$host" ] && body=cross
[ -n "$td" ] && mkdir -p "$d" && echo "$body" > "$d/agent-launch" && chmod +x "$d/agent-launch"
# FAKE_CARGO_STALE leaves a newer binary for another target next to this one, as an earlier
# cross build would.
if [ -n "${FAKE_CARGO_STALE:-}" ]; then
  mkdir -p "$td/aarch64-unknown-linux-gnu/release"
  echo stale > "$td/aarch64-unknown-linux-gnu/release/agent-launch"
  chmod +x "$td/aarch64-unknown-linux-gnu/release/agent-launch"
fi
exit 0
FAKE
chmod +x "$tmp/bin/uname" "$tmp/bin/cargo"

asset() { # asset <key> <triple> <body>
  printf '#!/bin/sh\n%s\n' "$3" > "$tmp/rel/agent-launch-$1-$2"
  (cd "$tmp/rel" && { sha256sum "agent-launch-$1-$2" 2>/dev/null || shasum -a 256 "agent-launch-$1-$2"; } > "agent-launch-$1-$2.sha256")
}
asset abcdef123456 x86_64-unknown-linux-musl '[ "$1" = version ] && echo "agent-launch 0.0.0"'
asset abcdef123456 aarch64-apple-darwin 'exit 1'

fails=0
n=0
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1"; echo "  want: $3"; echo "  got:  $2"; fails=$((fails + 1)); fi
}
fetch() {
  n=$((n + 1))
  export FAKE_CARGO_LOG="$tmp/cargo$n.log" AGENT_LAUNCH_OUT="$tmp/out$n/agent-launch" AGENT_LAUNCH_ROOT="${ROOT:-$tmp/root$n}"
  out="$tmp/fetch$n.txt"
  mkdir -p "$AGENT_LAUNCH_ROOT"
  : > "$FAKE_CARGO_LOG"
  HOME="$tmp/home" PATH="${FETCH_PATH:-$tmp/bin:$PATH}" AGENT_LAUNCH_BASE_URL="file://$tmp/rel" sh "$script" > "$out" 2>&1
}
cargo_calls() { wc -l < "$FAKE_CARGO_LOG" | tr -d ' '; }

AGENT_LAUNCH_KEY=abcdef1234567890 fetch; rc=$?
check "a matching prebuilt binary is installed" \
  "$(echo "rc=$rc"; cat "$out"; "$AGENT_LAUNCH_OUT" version; cargo_calls)" \
  "rc=0
agent-launch: installed prebuilt agent-launch-abcdef123456-x86_64-unknown-linux-musl
agent-launch 0.0.0
0"

check "nothing but the binary is left in the install directory" "$(ls -A "$(dirname "$AGENT_LAUNCH_OUT")")" "agent-launch"

mkdir -p "$tmp/out$((n + 1))/.fetch" && echo junk > "$tmp/out$((n + 1))/.fetch/junk"
AGENT_LAUNCH_KEY=abcdef1234567890 fetch
check "what an interrupted install left behind is removed by the next one" \
  "$(cat "$out"; ls -A "$(dirname "$AGENT_LAUNCH_OUT")")" \
  "agent-launch: installed prebuilt agent-launch-abcdef123456-x86_64-unknown-linux-musl
agent-launch"

TMPDIR="$tmp/absent" AGENT_LAUNCH_KEY=abcdef1234567890 fetch
check "the download does not go through TMPDIR, which can be missing or noexec" "$(cat "$out")" \
  "agent-launch: installed prebuilt agent-launch-abcdef123456-x86_64-unknown-linux-musl"

# Without curl the download goes through wget: a PATH holding what the script needs and no curl,
# and a wget that records its arguments and copies the file:// URL.
mkdir -p "$tmp/nocurl"
for c in sh dirname cut awk sed chmod mkdir mv rm cp cat sha256sum shasum; do
  p=$(command -v "$c") && ln -s "$p" "$tmp/nocurl/$c"
done
ln -s "$tmp/bin/uname" "$tmp/bin/cargo" "$tmp/nocurl/"
cat > "$tmp/nocurl/wget" <<'FAKE'
#!/bin/sh
echo "wget $*" >> "$FAKE_WGET_LOG"
to= url=
while [ $# -gt 0 ]; do
  case "$1" in -O) to=$2; shift ;; -T) shift ;; -*) ;; *) url=$1 ;; esac
  shift
done
cp "${url#file://}" "$to"
FAKE
chmod +x "$tmp/nocurl/wget"
FETCH_PATH="$tmp/nocurl" FAKE_WGET_LOG="$tmp/wget.log" AGENT_LAUNCH_KEY=abcdef1234567890 fetch
check "without curl, wget downloads the pair and is told not to retry" \
  "$( (PATH="$tmp/nocurl"; command -v curl || echo no-curl); cat "$out"; grep -c -e ' --tries=1 ' "$tmp/wget.log")" \
  "no-curl
agent-launch: installed prebuilt agent-launch-abcdef123456-x86_64-unknown-linux-musl
2"

AGENT_LAUNCH_KEY=000000000000 fetch
check "an unknown tree builds from source and installs what it built" \
  "$(head -1 "$out"; cut -d' ' -f1-4 "$FAKE_CARGO_LOG"; cat "$AGENT_LAUNCH_OUT")" \
  "agent-launch: no prebuilt binary agent-launch-000000000000-x86_64-unknown-linux-musl; building from source.
cargo build --release --locked
built"

good="$tmp/rel/agent-launch-abcdef123456-x86_64-unknown-linux-musl.sha256"
cp "$good" "$tmp/good.sha256"
printf 'deadbeef  x\n' > "$good"
AGENT_LAUNCH_KEY=abcdef123456 fetch
check "a checksum mismatch builds from source and installs nothing" \
  "$(head -1 "$out"; [ -e "$AGENT_LAUNCH_OUT" ] && cat "$AGENT_LAUNCH_OUT"; cargo_calls)" \
  "agent-launch: checksum mismatch for agent-launch-abcdef123456-x86_64-unknown-linux-musl; building from source.
built
1"
cp "$tmp/good.sha256" "$good"

FAKE_OS=Darwin FAKE_ARCH=arm64 AGENT_LAUNCH_KEY=abcdef123456 fetch
check "a binary that does not run here builds from source and installs nothing" \
  "$(head -1 "$out"; [ -e "$AGENT_LAUNCH_OUT" ] && cat "$AGENT_LAUNCH_OUT"; cargo_calls)" \
  "agent-launch: agent-launch-abcdef123456-aarch64-apple-darwin does not run here; building from source.
built
1"
check "a download that is not installed is removed" "$(ls -A "$(dirname "$AGENT_LAUNCH_OUT")")" "agent-launch"

FAKE_OS=Linux FAKE_ARCH=riscv64 AGENT_LAUNCH_KEY=abcdef123456 fetch
check "an unsupported platform builds from source" "$(head -1 "$out"; cargo_calls; cat "$AGENT_LAUNCH_OUT")" \
  "agent-launch: no prebuilt binary for Linux/riscv64; building from source.
1
built"

fetch
check "outside a git checkout it builds from source" "$(head -1 "$out"; cargo_calls; cat "$AGENT_LAUNCH_OUT")" \
  "agent-launch: not a git checkout; building from source.
1
built"

mkdir -p "$tmp/repo/agent-launch" && echo x > "$tmp/repo/agent-launch/f" && echo target/ > "$tmp/repo/agent-launch/.gitignore" && echo y > "$tmp/repo/other"
git -C "$tmp/repo" init -q && git -C "$tmp/repo" add -A &&
  GIT_CONFIG_GLOBAL=/dev/null git -C "$tmp/repo" -c user.email=t@example.com -c user.name=t commit -qm t
tree=$(git -C "$tmp/repo" rev-parse HEAD:agent-launch | cut -c1-12)
asset "$tree" x86_64-unknown-linux-musl '[ "$1" = version ] && echo "agent-launch 0.0.0"'
ROOT="$tmp/repo/agent-launch" fetch
check "the key is the git tree hash of agent-launch/, read from inside the plugin directory" \
  "$(head -1 "$out")" "agent-launch: installed prebuilt agent-launch-$tree-x86_64-unknown-linux-musl"

echo z >> "$tmp/repo/agent-launch/f"
ROOT="$tmp/repo/agent-launch" fetch
check "a working copy with uncommitted changes builds from source instead of downloading" \
  "$(head -1 "$out"; cargo_calls; cat "$AGENT_LAUNCH_OUT")" "agent-launch: agent-launch/ has uncommitted changes; building from source.
1
built"
git -C "$tmp/repo" checkout -q -- agent-launch/f
echo new > "$tmp/repo/agent-launch/untracked"
printf '[status]\n\tshowUntrackedFiles = no\n' > "$tmp/gitconfig"
GIT_CONFIG_GLOBAL="$tmp/gitconfig" ROOT="$tmp/repo/agent-launch" fetch; rc=$?
check "an untracked file counts as a change even when git is told to hide them, and the source build installs the binary" \
  "$(echo "rc=$rc"; head -1 "$out"; cat "$AGENT_LAUNCH_OUT"; cargo_calls)" \
  "rc=0
agent-launch: agent-launch/ has uncommitted changes; building from source.
built
1"
rm "$tmp/repo/agent-launch/untracked"

pushd "$tmp/repo" > /dev/null || exit 1
ROOT=agent-launch AGENT_LAUNCH_KEY=000000000000 fetch; rc=$?
popd > /dev/null || exit 1
check "a relative AGENT_LAUNCH_ROOT is resolved before the build" \
  "$(echo "rc=$rc"; sed -e 's/ in .*//' -e "s|$tmp/repo|REPO|" "$FAKE_CARGO_LOG"; cat "$AGENT_LAUNCH_OUT")" \
  "rc=0
cargo build --release --locked --target x86_64-unknown-linux-gnu --target-dir REPO/agent-launch/target
built"
echo y > "$tmp/repo/other" && echo w >> "$tmp/repo/other"
ROOT="$tmp/repo/agent-launch" fetch
check "a change outside agent-launch/ does not matter" "$(head -1 "$out")" "agent-launch: installed prebuilt agent-launch-$tree-x86_64-unknown-linux-musl"

CARGO_TARGET_DIR="$tmp/elsewhere" AGENT_LAUNCH_KEY=000000000000 fetch; rc=$?
check "the source build pins the target dir and copies the binary to where the manifest points" \
  "$(echo "rc=$rc"; sed -e 's/ in .*//' -e "s|$AGENT_LAUNCH_ROOT|ROOT|" "$FAKE_CARGO_LOG"; cat "$AGENT_LAUNCH_OUT"; [ -e "$tmp/elsewhere" ] && echo moved)" \
  "rc=0
cargo build --release --locked --target x86_64-unknown-linux-gnu --target-dir ROOT/target
built"

n=$((n + 1))
export FAKE_CARGO_LOG="$tmp/cargo$n.log" AGENT_LAUNCH_OUT="$tmp/out$n/agent-launch" AGENT_LAUNCH_ROOT="$tmp/root$n"
mkdir -p "$AGENT_LAUNCH_ROOT"
env -u HOME PATH="$tmp/bin:$PATH" AGENT_LAUNCH_KEY=000000000000 AGENT_LAUNCH_BASE_URL="file://$tmp/rel" sh "$script" > "$tmp/fetch$n.txt" 2>&1; rc=$?
check "the source build does not need HOME" "rc=$rc $(cat "$AGENT_LAUNCH_OUT")" "rc=0 built"

FAKE_CARGO_TRIPLE=aarch64-unknown-linux-gnu AGENT_LAUNCH_KEY=000000000000 fetch; rc=$?
check "a cross build.target in the user's Cargo config is overridden: the installed binary is built for this host" \
  "rc=$rc $(cat "$AGENT_LAUNCH_OUT")" "rc=0 built"

FAKE_CARGO_NOHOST=1 AGENT_LAUNCH_KEY=000000000000 fetch; rc=$?
check "a cargo that names no host triple fails saying so and builds nothing" \
  "rc=$rc $(grep -c 'cargo -vV names no host triple' "$out") $(cargo_calls)" "rc=1 1 0"

FAKE_CARGO_STALE=1 AGENT_LAUNCH_KEY=000000000000 fetch; rc=$?
check "the binary built for this host is installed, not a newer one for another target" \
  "rc=$rc $(cat "$AGENT_LAUNCH_OUT")" "rc=0 built"

FAKE_CARGO_NOBIN=1 AGENT_LAUNCH_KEY=000000000000 fetch; rc=$?
check "a build that writes no binary fails naming it" \
  "rc=$rc $(grep -c 'cargo build finished without a runnable ' "$tmp/fetch$n.txt")" "rc=1 1"

# Copied into another repo under a different directory name, the key is still this tree's hash.
mkdir -p "$tmp/other/plugins" && command cp -R "$tmp/repo/agent-launch" "$tmp/other/plugins/renamed"
git -C "$tmp/other" init -q && git -C "$tmp/other" add -A &&
  GIT_CONFIG_GLOBAL=/dev/null git -C "$tmp/other" -c user.name=t -c user.email=t@t commit -qm init
ROOT="$tmp/other/plugins/renamed" fetch
check "the key is the tree of the plugin directory itself, wherever it sits" "$(head -1 "$out")" "agent-launch: installed prebuilt agent-launch-$tree-x86_64-unknown-linux-musl"

printf '#!/bin/sh\nexit 1\n' > "$tmp/bin/cargo"
AGENT_LAUNCH_KEY=000000000000 fetch; rc=$?
check "no prebuilt binary and a failing cargo fails" "rc=$rc" "rc=1"

if [ "$fails" -gt 0 ]; then echo "$fails failed"; exit 1; fi
echo "all passed"
