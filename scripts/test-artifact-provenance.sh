#!/usr/bin/env bash
# Reject a same-version, correctly named archive built from another source/target.
set -euo pipefail
cd "$(dirname "$0")/.."
binary=${1:-target/release/mellow}
[[ -x "$binary" ]] || { echo 'Build the release executable before the provenance regression' >&2; exit 1; }
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) target=aarch64-apple-darwin ;;
  Darwin/x86_64) target=x86_64-apple-darwin ;;
  Linux/aarch64) target=aarch64-unknown-linux-musl ;;
  Linux/x86_64) target=x86_64-unknown-linux-musl ;;
  *) echo 'Unsupported fixture host' >&2; exit 1 ;;
esac
version=$(sed -n 's/^version = "\([^" ]*\)"/\1/p' Cargo.toml | head -1)
fixture=$(mktemp -d "${TMPDIR:-/tmp}/mellow-provenance-regression.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
name="mellow-$version-$target"
mkdir -p "$fixture/stage/$name"
cp "$binary" "$fixture/stage/$name/mellow"
assert_refused() {
  tar -C "$fixture/stage" -czf "$fixture/$name.tar.gz" "$name"
  if bash scripts/native-ci.sh artifact-install "$fixture/$name.tar.gz" > "$fixture/result.log" 2>&1; then
    cat "$fixture/result.log" >&2
    echo 'Incorrect archive provenance was accepted' >&2; exit 1
  fi
  grep -q "$1" "$fixture/result.log" || { cat "$fixture/result.log" >&2; exit 1; }
}
printf 'source_commit=%s\ntarget=%s\n' 0000000000000000000000000000000000000000 "$target" > "$fixture/stage/$name/BUILD.txt"
assert_refused 'Archive source commit differs'
printf 'source_commit=%s\ntarget=%s\n' "$(git rev-parse HEAD)" incorrect-native-target > "$fixture/stage/$name/BUILD.txt"
assert_refused 'Archive build target differs'
echo 'Artifact provenance regressions passed: stale source and incorrect native target refused.'
