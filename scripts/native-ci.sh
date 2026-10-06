#!/usr/bin/env bash
# Shared native gates for CI. No global installation is changed.
set -euo pipefail
cd "$(dirname "$0")/.."
mode=${1:-verify}
toolchain=$(sed -n 's/^channel = "\([^"]*\)"/\1/p' rust-toolchain.toml)
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) platform=macOS-ARM64; expected_host=aarch64-apple-darwin ;;
  Linux/x86_64) platform=Linux-X64; expected_host=x86_64-unknown-linux-gnu ;;
  Linux/aarch64) platform=Linux-ARM64; expected_host=aarch64-unknown-linux-gnu ;;
  *) echo "Unsupported native release host: $(uname -sm)" >&2; exit 1 ;;
esac
case "$mode/$platform" in
  archive/Linux-ARM64|archive-install/Linux-ARM64)
    echo 'Linux ARM64 development does not satisfy the Linux x64 release gate' >&2; exit 1 ;;
esac
if [[ "$(rustc --version | awk '{print $2}')" != "$toolchain" ]]; then
  echo "Required Rust $toolchain; found $(rustc --version)" >&2; exit 1
fi
host=$(rustc -vV | sed -n 's/^host: //p')
[[ "$host" == "$expected_host" ]] || { echo "Toolchain host mismatch: $host" >&2; exit 1; }
if [[ "$platform" == macOS-ARM64 ]]; then
  # Validate the agent's selected SDK against its linker. Some hosts retain an
  # older linker alongside a newer SDK. Use an installed compatible SDK only.
  probe=$(mktemp -d "${TMPDIR:-/tmp}/mellow-sdk-probe.XXXXXX")
  default_sdk=${SDKROOT:-$(xcrun --show-sdk-path)}
  sdk_parent=$(dirname "$default_sdk")
  compatible_sdk=''
  while IFS= read -r candidate; do
    if printf 'int main(void) { return 0; }\n' | \
      /usr/bin/cc -isysroot "$candidate" -x c - -o "$probe/check" 2> "$probe/linker.log"; then
      compatible_sdk=$candidate
      break
    fi
  done < <(printf '%s\n' "$default_sdk"; find "$sdk_parent" -maxdepth 1 -type d -name 'MacOSX*.sdk' | sort -r)
  [[ -n "$compatible_sdk" ]] || { cat "$probe/linker.log"; echo 'No compatible installed macOS SDK' >&2; exit 1; }
  export SDKROOT="$compatible_sdk"
  echo "macOS SDK: $SDKROOT"
fi
pty_smoke() {
  local log expected
  log=$(mktemp "${TMPDIR:-/tmp}/mellow-pty.XXXXXX")
  # Every smoke test must run and pass; count them so new tests are required too.
  expected=$(grep -c '^#\[test\]' tests/release_native_smoke.rs)
  MELLOW_SMOKE_BINARY="$1" cargo test --locked --test release_native_smoke \
    -- --nocapture | tee "$log"
  grep -q "test result: ok. $expected passed; 0 failed;" "$log" || {
    echo 'Required native PTY test did not execute successfully' >&2; exit 1;
  }
}
case "$mode" in
  verify)
    cargo fmt --all -- --check
    cargo check --locked --all-targets
    cargo clippy --locked --all-targets --all-features -- -D warnings
    cargo test --locked --all-targets
    ;;
  release) cargo build --locked --release ;;
  crate) cargo package --locked ;;
  install)
    install_root=$(mktemp -d "${TMPDIR:-/tmp}/mellow-source-install.XXXXXX")
    cargo install --locked --path . --root "$install_root"
    "$install_root/bin/mellow" --version
    pty_smoke "$install_root/bin/mellow"
    ;;
  archive)
    [[ -z "$(git status --porcelain --untracked-files=all)" ]] || { echo 'Archive requires a clean source checkout' >&2; exit 1; }
    artifact="mellow-$platform"
    mkdir -p "dist/$artifact"
    cp target/release/mellow README.md LICENSE "dist/$artifact/"
    {
      echo "source_commit=$(git rev-parse HEAD)"
      echo "canonical_repository=https://github.com/ritru-labs/mellow"
      echo "platform=$platform"
      echo "execution_kind=${MELLOW_EXECUTION_KIND:-native}"
      echo "toolchain=$toolchain"
      echo "host=$host"
      echo "sdk=${SDKROOT:-not-applicable}"
      echo "build_url=${BUILD_URL:-local}"
      echo 'build_result=package-created; acceptance requires successful install and PTY logs'
      rustc -vV
    } > "dist/$artifact/BUILD.txt"
    tar -C dist -czf "dist/$artifact.tar.gz" "$artifact"
    (cd dist && shasum -a 256 "$artifact.tar.gz" > "$artifact.tar.gz.sha256")
    ;;
  archive-install)
    artifact="mellow-$platform"
    (cd dist && shasum -a 256 -c "$artifact.tar.gz.sha256")
    install_root=$(mktemp -d "${TMPDIR:-/tmp}/mellow-archive-install.XXXXXX")
    tar -xzf "dist/$artifact.tar.gz" -C "$install_root"
    binary="$install_root/$artifact/mellow"
    version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
    [[ "$("$binary" --version)" == "mellow $version" ]]
    "$binary" --help
    file "$binary"
    pty_smoke "$binary"
    ;;
  *) echo "Unknown gate: $mode" >&2; exit 1 ;;
esac
