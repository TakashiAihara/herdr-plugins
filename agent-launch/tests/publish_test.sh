#!/usr/bin/env bash
# Runs the publish step of ../../.github/workflows/agent-launch-binaries.yml against a fake `gh`:
# a binary and its .sha256 must upload as one pair, and a complete pair is never replaced.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
yml="$here/../../.github/workflows/agent-launch-binaries.yml"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# The publish step's `run: |` is the last one in the workflow; drop its 10-space step indent.
awk '/^        run: \|$/ { buf = ""; next } { buf = buf $0 "\n" } END { printf "%s", buf }' "$yml" \
  | sed 's/^          //' > "$tmp/publish.sh"
[ -s "$tmp/publish.sh" ] || { echo "FAIL could not find the publish run block in $yml"; exit 1; }

# The release is $FAKE_RELEASE: each asset is a `name<TAB>state` line in `state` plus its content
# file under `files/`. `gh release view --json assets --jq ...` answers `name state` lines when
# the query asks for the state, bare names when it asks only for names.
cat > "$tmp/gh" <<'FAKE'
#!/usr/bin/env bash
set -u
state="$FAKE_RELEASE/state"
files="$FAKE_RELEASE/files"
case "$1 ${2:-}" in
  "release view")
    [ -d "$FAKE_RELEASE" ] || exit 1
    [ -f "$state" ] || exit 0
    case " $* " in
      *state*) awk -F'\t' '{print $1 " " $2}' "$state" ;;
      *) awk -F'\t' '{print $1}' "$state" ;;
    esac
    exit 0 ;;
  "release create")
    mkdir -p "$files"; : > "$state"
    # FAKE_CREATE_RACE: another run created the release first, so this create fails.
    [ -n "${FAKE_CREATE_RACE:-}" ] && exit 1
    exit 0 ;;
  "release upload")
    mkdir -p "$files"; [ -f "$state" ] || : > "$state"
    shift 3 # release upload <tag>
    while [ $# -gt 0 ]; do
      case "$1" in
        --repo) shift 2 ;;
        *)
          n=$(basename "$1")
          cp "$1" "$files/$n" || exit 1
          awk -F'\t' -v n="$n" '$1 != n' "$state" > "$state.new" && mv "$state.new" "$state"
          printf '%s\t%s\n' "$n" uploaded >> "$state"
          shift ;;
      esac
    done
    exit 0 ;;
  "release delete-asset")
    [ -f "$state" ] || : > "$state"
    shift 3 # release delete-asset <tag>
    name=
    while [ $# -gt 0 ]; do
      case "$1" in
        --repo) shift 2 ;;
        -y|--yes) shift ;;
        *) name=$1; shift ;;
      esac
    done
    [ -n "$name" ] && [ -f "$files/$name" ] || exit 1
    awk -F'\t' -v n="$name" '$1 != n' "$state" > "$state.new" && mv "$state.new" "$state"
    rm -f "$files/$name"
    exit 0 ;;
esac
echo "fake gh: unexpected: $*" >&2
exit 1
FAKE
chmod +x "$tmp/gh"

export FAKE_RELEASE="$tmp/release"
mkdir -p "$FAKE_RELEASE/files" "$tmp/run/dist"
cd "$tmp/run" || exit 1
base="agent-launch-abc123-x86_64-unknown-linux-musl"
printf 'new-binary\n' > "dist/$base"
printf 'new-sha\n' > "dist/$base.sha256"

fails=0
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1"; echo "  want: $3"; echo "  got:  $2"; fails=$((fails + 1)); fi
}
# The shell GitHub Actions runs a `run:` step with.
publish() { GITHUB_REPOSITORY=t/r GH_TOKEN=x PATH="$tmp:$PATH" bash --noprofile --norc -eo pipefail "$tmp/publish.sh"; }
# Content checks go through this: a publish that fails adds a line, so a failure that happens to
# leave the assets as expected still fails the check.
publish_ok() { publish >/dev/null || echo "publish failed: rc=$?"; }
state() { tr '\t' ' ' < "$FAKE_RELEASE/state"; }
asset() { cat "$FAKE_RELEASE/files/$1"; }

check "a missing pair is uploaded together" \
  "$(publish_ok; state; asset "$base"; asset "$base.sha256")" \
  "$base uploaded
$base.sha256 uploaded
new-binary
new-sha"

printf '%s\tuploaded\n%s\tuploaded\n' "$base" "$base.sha256" > "$FAKE_RELEASE/state"
printf 'old-binary\n' > "$FAKE_RELEASE/files/$base"
printf 'old-sha\n' > "$FAKE_RELEASE/files/$base.sha256"
check "a complete pair is kept, never replaced" \
  "$(publish; asset "$base"; asset "$base.sha256")" \
  "keeping the existing $base pair
old-binary
old-sha"

printf '%s\tuploaded\n' "$base" > "$FAKE_RELEASE/state"
printf 'old-binary\n' > "$FAKE_RELEASE/files/$base"
rm -f "$FAKE_RELEASE/files/$base.sha256"
check "a pair missing its checksum is uploaded again as a pair" \
  "$(publish_ok; asset "$base"; asset "$base.sha256")" \
  "new-binary
new-sha"

printf '%s\tstarter\n%s\tuploaded\n' "$base" "$base.sha256" > "$FAKE_RELEASE/state"
printf 'old-binary\n' > "$FAKE_RELEASE/files/$base"
printf 'old-sha\n' > "$FAKE_RELEASE/files/$base.sha256"
check "a pair whose asset never finished uploading is uploaded again" \
  "$(publish_ok; asset "$base"; asset "$base.sha256")" \
  "new-binary
new-sha"

rm -f "$FAKE_RELEASE/files/$base"
printf '%s\tuploaded\n' "$base.sha256" > "$FAKE_RELEASE/state"
check "a lone checksum is replaced by the pair" \
  "$(publish_ok; asset "$base"; asset "$base.sha256")" \
  "new-binary
new-sha"

# A complete pair listed first, then more than a pipe buffer of other assets: a `grep -q` fed by a
# pipe would stop early, the writer would die of SIGPIPE, and pipefail would report a miss.
{ printf '%s\tuploaded\n%s\tuploaded\n' "$base" "$base.sha256"
  for i in $(seq 1 3000); do printf 'agent-launch-%040d-x86_64-unknown-linux-musl\tuploaded\n' "$i"; done
} > "$FAKE_RELEASE/state"
printf 'old-binary\n' > "$FAKE_RELEASE/files/$base"
printf 'old-sha\n' > "$FAKE_RELEASE/files/$base.sha256"
check "a complete pair is kept when the asset list is longer than a pipe buffer" \
  "$(publish_ok; asset "$base"; asset "$base.sha256")" \
  "old-binary
old-sha"

rm -rf "$FAKE_RELEASE"
check "the release is created when missing" \
  "$(publish_ok; state)" \
  "$base uploaded
$base.sha256 uploaded"

rm -rf "$FAKE_RELEASE"
check "losing the race to create the release still publishes the pair" \
  "$(FAKE_CREATE_RACE=1 publish_ok; state)" \
  "$base uploaded
$base.sha256 uploaded"

if [ "$fails" -gt 0 ]; then echo "$fails failed"; exit 1; fi
echo "all passed"
