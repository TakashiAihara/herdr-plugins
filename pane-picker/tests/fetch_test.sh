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
echo "cargo $* in $(pwd)" >> "$FAKE_CARGO_LOG"
FAKE
chmod +x "$tmp/bin/uname" "$tmp/bin/cargo"

asset() { # asset <key> <triple> <body>
  printf '#!/bin/sh\n%s\n' "$3" > "$tmp/rel/pane-picker-$1-$2"
  (cd "$tmp/rel" && { sha256sum "pane-picker-$1-$2" 2>/dev/null || shasum -a 256 "pane-picker-$1-$2"; } > "pane-picker-$1-$2.sha256")
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
  export FAKE_CARGO_LOG="$tmp/cargo$n.log" PANE_PICKER_OUT="$tmp/out$n/pane-picker" PANE_PICKER_ROOT="${ROOT:-$tmp/root$n}"
  out="$tmp/fetch$n.txt"
  mkdir -p "$PANE_PICKER_ROOT"
  : > "$FAKE_CARGO_LOG"
  HOME="$tmp/home" PATH="$tmp/bin:$PATH" PANE_PICKER_BASE_URL="file://$tmp/rel" sh "$script" > "$out" 2>&1
}
cargo_calls() { wc -l < "$FAKE_CARGO_LOG" | tr -d ' '; }

PANE_PICKER_KEY=abcdef1234567890 fetch; rc=$?
check "a matching prebuilt binary is installed" \
  "$(echo "rc=$rc"; cat "$out"; "$PANE_PICKER_OUT" enter x; cargo_calls)" \
  "rc=0
pane-picker: installed prebuilt pane-picker-abcdef123456-x86_64-unknown-linux-musl
accept
0"

PANE_PICKER_KEY=000000000000 fetch
check "an unknown tree builds from source" \
  "$(head -1 "$out"; cut -d' ' -f1-4 "$FAKE_CARGO_LOG")" \
  "pane-picker: no prebuilt binary pane-picker-000000000000-x86_64-unknown-linux-musl; building from source.
cargo build --release --locked"

good="$tmp/rel/pane-picker-abcdef123456-x86_64-unknown-linux-musl.sha256"
cp "$good" "$tmp/good.sha256"
printf 'deadbeef  x\n' > "$good"
PANE_PICKER_KEY=abcdef123456 fetch
check "a checksum mismatch builds from source and installs nothing" \
  "$(head -1 "$out"; [ -e "$PANE_PICKER_OUT" ] && echo installed; cargo_calls)" \
  "pane-picker: checksum mismatch for pane-picker-abcdef123456-x86_64-unknown-linux-musl; building from source.
1"
cp "$tmp/good.sha256" "$good"

FAKE_OS=Darwin FAKE_ARCH=arm64 PANE_PICKER_KEY=abcdef123456 fetch
check "a binary that does not run here builds from source and installs nothing" \
  "$(head -1 "$out"; [ -e "$PANE_PICKER_OUT" ] && echo installed; cargo_calls)" \
  "pane-picker: pane-picker-abcdef123456-aarch64-apple-darwin does not run here; building from source.
1"

FAKE_OS=Linux FAKE_ARCH=riscv64 PANE_PICKER_KEY=abcdef123456 fetch
check "an unsupported platform builds from source" "$(head -1 "$out"; cargo_calls)" \
  "pane-picker: no prebuilt binary for Linux/riscv64; building from source.
1"

fetch
check "outside a git checkout it builds from source" "$(head -1 "$out"; cargo_calls)" \
  "pane-picker: not a git checkout; building from source.
1"

mkdir -p "$tmp/repo/pane-picker" && echo x > "$tmp/repo/pane-picker/f" && echo y > "$tmp/repo/other"
git -C "$tmp/repo" init -q && git -C "$tmp/repo" add -A &&
  GIT_CONFIG_GLOBAL=/dev/null git -C "$tmp/repo" -c user.email=t@example.com -c user.name=t commit -qm t
tree=$(git -C "$tmp/repo" rev-parse HEAD:pane-picker | cut -c1-12)
asset "$tree" x86_64-unknown-linux-musl '[ "$1" = enter ] && echo accept'
ROOT="$tmp/repo/pane-picker" fetch
check "the key is the git tree hash of pane-picker/, read from inside the plugin directory" \
  "$(head -1 "$out")" "pane-picker: installed prebuilt pane-picker-$tree-x86_64-unknown-linux-musl"

printf '#!/bin/sh\nexit 1\n' > "$tmp/bin/cargo"
PANE_PICKER_KEY=000000000000 fetch; rc=$?
check "no prebuilt binary and a failing cargo fails" "rc=$rc" "rc=1"

if [ "$fails" -gt 0 ]; then echo "$fails failed"; exit 1; fi
echo "all passed"
