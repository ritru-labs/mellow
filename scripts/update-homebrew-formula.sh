#!/usr/bin/env bash
# Point packaging/homebrew/mellow.rb at a release tag and its tarball checksum.
# Usage: scripts/update-homebrew-formula.sh v0.1.2
#
# Downloads the same URL Homebrew uses, so the repository must be public.
set -euo pipefail

tag="${1:?usage: $0 vX.Y.Z}"
formula="$(dirname "$0")/../packaging/homebrew/mellow.rb"
url="https://github.com/ritru-labs/mellow/archive/refs/tags/$tag.tar.gz"

sha="$(curl -fsSL "$url" | shasum -a 256 | cut -d' ' -f1)"

sed -i.bak \
  -e "s|archive/refs/tags/v[0-9][0-9.]*\.tar\.gz|archive/refs/tags/$tag.tar.gz|" \
  -e "s|sha256 \"[0-9a-f]\{64\}\"|sha256 \"$sha\"|" \
  "$formula"
rm -f "$formula.bak"
echo "Updated $formula to $tag ($sha)"
