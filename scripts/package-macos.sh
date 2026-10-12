#!/usr/bin/env bash
# Build macOS binaries and .tar.gz archives into dist/macos.
#   scripts/package-macos.sh                        # Apple silicon and Intel
#   scripts/package-macos.sh aarch64-apple-darwin   # one target
# Needs rustup for the cross target (Intel is cross-compiled on Apple silicon).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
[[ -z "$(git -C "$root" status --porcelain --untracked-files=all)" ]] || {
  echo 'Packaging requires a clean source checkout for exact-commit provenance' >&2; exit 1;
}
source_commit=$(git -C "$root" rev-parse HEAD)
targets=("${@:-aarch64-apple-darwin x86_64-apple-darwin}")
read -r -a targets <<<"${targets[*]}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
out="$root/dist/macos"
mkdir -p "$out"
# Binaries run on macOS 11 and later, whatever SDK builds them.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"

for target in "${targets[@]}"; do
  case "$target" in
    aarch64-apple-darwin) arch=arm64 ;;
    x86_64-apple-darwin) arch=x86_64 ;;
    *) echo "unknown target: $target" >&2; exit 2 ;;
  esac
  if command -v rustup >/dev/null; then
    rustup target add "$target" >/dev/null
  fi
  cargo build --manifest-path "$root/Cargo.toml" --locked --release --target "$target"
  [[ "$(git -C "$root" rev-parse HEAD)" == "$source_commit" &&
     -z "$(git -C "$root" status --porcelain --untracked-files=all)" ]] || {
    echo 'Source changed during packaging; candidate is not qualified' >&2; exit 1;
  }
  bin="$root/target/$target/release/mellow"
  lipo -archs "$bin" | grep -qx "$arch" || { echo "wrong architecture: $(lipo -archs "$bin")" >&2; exit 1; }
  if [[ "$(uname -m)" == "$arch" ]]; then
    [[ "$("$bin" --version)" == "mellow $version" ]]
  fi

  if [[ -n "${MELLOW_MACOS_SIGN_IDENTITY:-}" ]]; then
    codesign --force --timestamp --options runtime --sign "$MELLOW_MACOS_SIGN_IDENTITY" "$bin"
    codesign --verify --strict --verbose=2 "$bin"
  elif [[ "${MELLOW_REQUIRE_SIGNING:-0}" == 1 ]]; then
    echo 'macOS signing is required; configure the Apple signing identity and keychain first' >&2
    exit 1
  fi

  name="mellow-$version-$target"
  stage="$(mktemp -d)"
  mkdir -p "$stage/$name"
  cp "$bin" "$root/README.md" "$root/LICENSE" "$stage/$name/"
  {
    echo "source_commit=$source_commit"
    echo "target=$target"
    echo "signing=${MELLOW_MACOS_SIGN_IDENTITY:+Apple-code-signed}"
    echo 'notarization=not-verified'
    rustc -vV
  } > "$stage/$name/BUILD.txt"
  tar -C "$stage" -czf "$out/$name.tar.gz" "$name"
  rm -rf "$stage"
done

[[ "$(git -C "$root" rev-parse HEAD)" == "$source_commit" &&
   -z "$(git -C "$root" status --porcelain --untracked-files=all)" ]] || {
  echo 'Source changed during packaging; candidate is not qualified' >&2; exit 1;
}
ls -la "$out"
