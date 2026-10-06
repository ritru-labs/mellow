#!/bin/sh
# Install Mellow from its GitHub release on macOS or Linux.
#
#   curl -fsSL https://github.com/ritru-labs/mellow/releases/latest/download/install.sh | sh
#
# Settings (environment variables):
#   MELLOW_VERSION=0.2.0     install this version instead of the latest
#   MELLOW_INSTALL_DIR=DIR   install into DIR instead of ~/.local/bin
#   MELLOW_DOWNLOAD_URL=URL  download from a mirror holding the release files
set -eu

repo="https://github.com/ritru-labs/mellow"
install_dir="${MELLOW_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'mellow: %s\n' "$*"; }
fail() {
  printf 'mellow: %s\n' "$*" >&2
  exit 1
}

if [ -n "${MELLOW_DOWNLOAD_URL:-}" ]; then
  base="${MELLOW_DOWNLOAD_URL%/}"
elif [ -n "${MELLOW_VERSION:-}" ]; then
  base="$repo/releases/download/v${MELLOW_VERSION#v}"
else
  base="$repo/releases/latest/download"
fi

download() {
  if command -v curl >/dev/null 2>&1; then
    case "$1" in
      https://*) curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1" ;;
      *) curl -fsSL -o "$2" "$1" ;;
    esac
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    fail "needs curl or wget to download"
  fi
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d ' ' -f 1
  else
    fail "needs sha256sum or shasum to check the download"
  fi
}

case "$(uname -s)" in
  Darwin) os=apple-darwin ;;
  Linux) os=unknown-linux-musl ;;
  *) fail "$(uname -s) is not supported. Mellow runs on macOS and Linux (on Windows, use WSL)." ;;
esac

case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) fail "$(uname -m) processors are not supported yet." ;;
esac
# A shell running under Rosetta reports x86_64 on Apple silicon.
if [ "$os" = apple-darwin ] && [ "$arch" = x86_64 ] &&
  [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
  arch=aarch64
fi
target="$arch-$os"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

download "$base/SHA256SUMS" "$tmp/SHA256SUMS" ||
  fail "could not download the release from $base"
# The checksum list names the archive, and so tells us the version.
line="$(grep -E "  \\*?mellow-[0-9][^ ]*-$target\\.tar\\.gz\$" "$tmp/SHA256SUMS" | head -n 1)"
[ -n "$line" ] || fail "this release has no download for $target"
expected="${line%% *}"
archive="${line##* }"
archive="${archive#\*}"
name="${archive%.tar.gz}"

say "downloading $archive"
download "$base/$archive" "$tmp/$archive" || fail "could not download $archive"
[ "$(sha256 "$tmp/$archive")" = "$expected" ] ||
  fail "the download of $archive is damaged (checksum mismatch); nothing was installed"

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$install_dir"
# Copy then rename, so a running mellow is never left half-written.
cp "$tmp/$name/mellow" "$install_dir/.mellow.new"
chmod 755 "$install_dir/.mellow.new"
mv -f "$install_dir/.mellow.new" "$install_dir/mellow"

version="$("$install_dir/mellow" --version)" || fail "installed, but $install_dir/mellow did not run"
say "installed $version to $install_dir/mellow"

case ":$PATH:" in
  *":$install_dir:"*) say "run: mellow" ;;
  *)
    case "${SHELL:-}" in
      */zsh) rc="$HOME/.zshrc" ;;
      */bash) rc="$HOME/.bashrc" ;;
      *) rc="your shell's startup file" ;;
    esac
    say "$install_dir is not on your PATH yet. Add this line to $rc:"
    say "  export PATH=\"$install_dir:\$PATH\""
    say "then open a new terminal and run: mellow"
    ;;
esac
