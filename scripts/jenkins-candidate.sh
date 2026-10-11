#!/usr/bin/env bash
# Exact-source local Jenkins acceptance. Never installs globally or publishes.
set -euo pipefail
cd "$(dirname "$0")/.."
mode=${1:-acceptance}
[[ "$mode" == acceptance || "$mode" == preflight ]] || { echo 'Expected acceptance or preflight' >&2; exit 2; }
mkdir -p dist/jenkins-evidence
evidence="$PWD/dist/jenkins-evidence"
started=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
finish() {
  result=$?
  {
    echo "source_commit=${SOURCE_SHA:-missing}"
    echo "target=${EXPECTED_TARGET:-missing}"
    echo "mode=$mode"
    echo "started=$started"
    echo "finished=$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    echo "exit_code=$result"
    echo "build_url=${BUILD_URL:-local}"
    if [[ $result == 0 && "$mode" == acceptance ]]; then
      echo 'candidate_acceptance=passed'
    elif [[ $result == 0 ]]; then
      echo 'preflight=passed; candidate_acceptance=not-run'
    else
      echo 'candidate_acceptance=failed'
    fi
  } > "$evidence/result.txt"
}
trap finish EXIT
exec > >(tee -a "$evidence/console.log") 2>&1
[[ "${SOURCE_SHA:-}" =~ ^[0-9a-f]{40}$ ]] || { echo 'SOURCE_SHA must be an immutable 40-character commit ID' >&2; exit 1; }
[[ "$(git rev-parse HEAD)" == "$SOURCE_SHA" ]] || { echo 'Checkout differs from requested SOURCE_SHA' >&2; exit 1; }
[[ -z "$(git status --porcelain --untracked-files=all)" ]] || { echo 'Acceptance requires a clean immutable checkout' >&2; exit 1; }
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) target=aarch64-apple-darwin; host=aarch64-apple-darwin; package_arch=aarch64 ;;
  Darwin/x86_64) target=x86_64-apple-darwin; host=x86_64-apple-darwin; package_arch=x86_64 ;;
  Linux/aarch64) target=aarch64-unknown-linux-musl; host=aarch64-unknown-linux-gnu; package_arch=aarch64 ;;
  Linux/x86_64) target=x86_64-unknown-linux-musl; host=x86_64-unknown-linux-gnu; package_arch=x86_64 ;;
  *) echo "Unsupported native host: $(uname -sm)" >&2; exit 1 ;;
esac
[[ "${EXPECTED_TARGET:-}" == "$target" ]] || { echo "Requested ${EXPECTED_TARGET:-missing} differs from actual native target $target" >&2; exit 1; }
[[ -n "${RUST_TOOLCHAIN_BIN:-}" && -x "$RUST_TOOLCHAIN_BIN/rustc" && -x "$RUST_TOOLCHAIN_BIN/cargo" ]] || { echo 'Provide the existing native Rust 1.90.0 bin directory' >&2; exit 1; }
export PATH="$RUST_TOOLCHAIN_BIN:/opt/homebrew/bin:/usr/local/bin:$PATH"
[[ "$(rustc --version | awk '{print $2}')" == 1.90.0 ]] || { echo 'Rust 1.90.0 required' >&2; exit 1; }
[[ "$(rustc -vV | sed -n 's/^host: //p')" == "$host" ]] || { echo 'Toolchain does not match the native host' >&2; exit 1; }
export MELLOW_EXECUTION_KIND=native
unset MELLOW_MACOS_SIGN_IDENTITY MELLOW_REQUIRE_SIGNING
{
  echo "source_commit=$SOURCE_SHA"
  echo "target=$target"
  echo "execution_kind=native"
  echo "build_url=${BUILD_URL:-local}"
  uname -sm
  rustc -vV
  echo "jenkinsfile_sha256=$(shasum -a 256 Jenkinsfile | awk '{print $1}')"
  echo "helper_sha256=$(shasum -a 256 scripts/jenkins-candidate.sh | awk '{print $1}')"
  # Python preserves an inherited ignored SIGINT/SIGQUIT. Record the launcher
  # boundary without changing the editor's or controller's signal state.
  python3 - <<'PY'
import signal
blocked = signal.pthread_sigmask(signal.SIG_BLOCK, [])
for name in ('SIGINT', 'SIGQUIT'):
    sig = getattr(signal, name)
    disposition = 'ignored' if signal.getsignal(sig) == signal.SIG_IGN else 'not-ignored'
    print(f'launcher_{name}={disposition}; blocked={sig in blocked}')
PY
} | tee "$evidence/host.txt"
if [[ "$mode" == preflight ]]; then exit 0; fi
if [[ -n "${TARGET_CACHE_BASE:-}" ]]; then
  [[ "$TARGET_CACHE_BASE" == /* ]] || { echo 'Compiler cache must use an absolute agent-local path' >&2; exit 1; }
  cache="$TARGET_CACHE_BASE/$target-rust-1.90.0"
  mkdir -p "$cache"
  [[ -O "$cache" ]] || { echo 'Compiler cache must be owned by the build agent user' >&2; exit 1; }
  [[ ! -e target && ! -L target ]] || { echo 'Fresh checkout already has target contents; refusing to overwrite' >&2; exit 1; }
  ln -s "$cache" target
  export CARGO_TARGET_DIR="$PWD/target"
  echo "compiler_cache=$cache" >> "$evidence/host.txt"
fi
gate() {
  local name=$1
  shift
  "$@" 2>&1 | tee "$evidence/$name.log"
  echo "$name=passed" >> "$evidence/gates.txt"
}
gate preflight-regressions bash scripts/jenkins-preflight-test.sh
gate verify bash scripts/native-ci.sh verify
gate release bash scripts/native-ci.sh release
gate crate bash scripts/native-ci.sh crate
gate isolated-install bash scripts/native-ci.sh install
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
if [[ "$target" == *apple-darwin ]]; then
  # Select a linker-compatible installed SDK exactly as the native CI script
  # does, because its exported SDK is scoped to each child shell.
  probe=$(mktemp -d "${TMPDIR:-/tmp}/mellow-jenkins-sdk.XXXXXX")
  default_sdk=${SDKROOT:-$(xcrun --show-sdk-path)}
  sdk_parent=$(dirname "$default_sdk")
  compatible_sdk=''
  while IFS= read -r sdk; do
    if printf 'int main(void) { return 0; }\n' | /usr/bin/cc -isysroot "$sdk" -x c - -o "$probe/check" 2> "$probe/linker.log"; then
      compatible_sdk=$sdk
      break
    fi
  done < <(printf '%s\n' "$default_sdk"; find "$sdk_parent" -maxdepth 1 -type d -name 'MacOSX*.sdk' | sort -r)
  [[ -n "$compatible_sdk" ]] || { cat "$probe/linker.log"; echo 'No compatible installed macOS SDK' >&2; exit 1; }
  export SDKROOT="$compatible_sdk"
  gate package bash scripts/package-macos.sh "$target"
  archive="dist/macos/mellow-$version-$target.tar.gz"
  gate extracted-archive-pty bash scripts/native-ci.sh artifact-install "$archive"
else
  export MELLOW_EXECUTION_KIND=native-container
  gate linux-native-packages bash scripts/package-linux.sh "$package_arch"
  archive="dist/linux/mellow-$version-$target.tar.gz"
fi
grep -qx 'acceptance=passed' "$archive.acceptance.txt"
grep -qx "source_commit=$SOURCE_SHA" "$archive.acceptance.txt"
grep -qx "archive_sha256=$(shasum -a 256 "$archive" | awk '{print $1}')" "$archive.acceptance.txt"
package_receipts() {
  local execution=$1 package
  local packages=(dist/linux/*.deb dist/linux/*.rpm)
  [[ ${#packages[@]} == 2 ]] || { echo 'Expected exactly one deb and one rpm candidate' >&2; exit 1; }
  for package in "${packages[@]}"; do
    grep -qx 'install_pty=passed' "$package.acceptance.txt"
    grep -qx 'uninstall=passed' "$package.acceptance.txt"
    grep -qx "execution_kind=$execution" "$package.acceptance.txt"
    grep -qx "source_commit=$SOURCE_SHA" "$package.acceptance.txt"
    grep -qx "package_sha256=$(shasum -a 256 "$package" | awk '{print $1}')" "$package.acceptance.txt"
  done
}
if [[ "$target" == *linux-musl ]]; then package_receipts native-container; fi
if [[ "${RUN_LINUX_VM_PACKAGES:-false}" == true ]]; then
  [[ "$target" == aarch64-apple-darwin ]] || { echo 'Linux VM package option requires the native ARM64 Mac' >&2; exit 1; }
  export MELLOW_EXECUTION_KIND=linux-vm
  gate linux-vm-packages bash scripts/package-linux.sh aarch64
  grep -qx 'execution_kind=linux-vm' "dist/linux/mellow-$version-aarch64-unknown-linux-musl.tar.gz.acceptance.txt"
  grep -qx "source_commit=$SOURCE_SHA" "dist/linux/mellow-$version-aarch64-unknown-linux-musl.tar.gz.acceptance.txt"
  package_receipts linux-vm
  echo 'linux_vm_packages=passed; native_linux_acceptance=not-established' >> "$evidence/gates.txt"
fi
[[ "$(git rev-parse HEAD)" == "$SOURCE_SHA" && -z "$(git status --porcelain --untracked-files=all)" ]] || { echo 'Source changed during acceptance' >&2; exit 1; }
package_dirs=()
for package_dir in dist/macos dist/linux; do
  if [[ -d "$package_dir" ]]; then package_dirs+=("$package_dir"); fi
done
find "${package_dirs[@]}" -type f \( -name '*.tar.gz' -o -name '*.deb' -o -name '*.rpm' \) -exec shasum -a 256 {} \; > "$evidence/artifact-sha256.txt"
[[ -s "$evidence/artifact-sha256.txt" ]] || { echo 'No candidate artifact digest receipts produced' >&2; exit 1; }
