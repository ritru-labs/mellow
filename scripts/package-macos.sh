#!/usr/bin/env bash
# Build macOS binaries and .tar.gz archives into dist/macos.
#   scripts/package-macos.sh                        # Apple silicon and Intel
#   scripts/package-macos.sh aarch64-apple-darwin   # one target
# Needs rustup for the cross target (Intel is cross-compiled on Apple silicon).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
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
  bin="$root/target/$target/release/mellow"
  lipo -archs "$bin" | grep -qx "$arch" || { echo "wrong architecture: $(lipo -archs "$bin")" >&2; exit 1; }
  if [[ "$(uname -m)" == "$arch" ]]; then
    [[ "$("$bin" --version)" == "mellow $version" ]]
  fi

  name="mellow-$version-$target"
  stage="$(mktemp -d)"
  mkdir -p "$stage/$name"
  cp "$bin" "$root/README.md" "$root/LICENSE" "$stage/$name/"
  tar -C "$stage" -czf "$out/$name.tar.gz" "$name"
  rm -rf "$stage"
done

ls -la "$out"
