#!/usr/bin/env bash
# Build static Linux binaries and .tar.gz/.deb/.rpm packages into dist/linux.
#   scripts/package-linux.sh            # both x86_64 and aarch64
#   scripts/package-linux.sh x86_64     # one architecture
# x86_64 on an ARM Mac (or aarch64 on an x86 host) runs under Docker's
# emulation: slower, same result.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
archs=("${@:-x86_64 aarch64}")
read -r -a archs <<<"${archs[*]}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
out="$root/dist/linux"
mkdir -p "$out"

for arch in "${archs[@]}"; do
  case "$arch" in
    x86_64) platform=linux/amd64 deb_arch=amd64 ;;
    aarch64) platform=linux/arm64 deb_arch=arm64 ;;
    *) echo "unknown architecture: $arch" >&2; exit 2 ;;
  esac
  image="mellow-linux-package:$arch"
  docker build -q --platform "$platform" -t "$image" -f "$root/packaging/linux/Dockerfile" "$root/packaging/linux" >/dev/null

  docker run --rm --platform "$platform" \
    -v "$root:/workspace" \
    -v "mellow-package-target-$arch:/target" \
    -v "mellow-package-cargo-$arch:/usr/local/cargo/registry" \
    -e CARGO_TARGET_DIR=/target \
    -e ARCH="$arch" -e DEB_ARCH="$deb_arch" -e VERSION="$version" \
    "$image" bash -euo pipefail -c '
      target="$ARCH-unknown-linux-musl"
      rustup target add "$target" >/dev/null
      cargo build --locked --release --target "$target"
      bin="/target/$target/release/mellow"
      file "$bin" | grep -qE "static(ally|-pie) linked" || { echo "not static: $(file "$bin")" >&2; exit 1; }
      "$bin" --version

      name="mellow-$VERSION-$target"
      stage="$(mktemp -d)"
      out=/workspace/dist/linux

      # Plain tarball
      mkdir -p "$stage/$name"
      cp "$bin" README.md LICENSE "$stage/$name/"
      tar -C "$stage" -czf "$out/$name.tar.gz" "$name"

      # .deb
      deb="$stage/deb"
      mkdir -p "$deb/DEBIAN" "$deb/usr/bin" "$deb/usr/share/doc/mellow"
      install -m 0755 "$bin" "$deb/usr/bin/mellow"
      install -m 0644 LICENSE README.md "$deb/usr/share/doc/mellow/"
      cat >"$deb/DEBIAN/control" <<CONTROL
Package: mellow
Version: $VERSION
Architecture: $DEB_ARCH
Maintainer: Ritru Labs <noreply@ritru.com>
Section: editors
Priority: optional
Homepage: https://github.com/ritru-labs/mellow
Description: Modern, visual-first terminal text editor
 Mellow is a terminal editor with a calm, discoverable interface:
 tabs, splits, project search, Git, an integrated terminal and
 optional AI, all without a learning curve.
CONTROL
      dpkg-deb --root-owner-group --build "$deb" "$out/mellow_${VERSION}_${DEB_ARCH}.deb" >/dev/null

      # .rpm (binary is prebuilt and static, so no build requirements)
      rpmtop="$stage/rpm"
      mkdir -p "$rpmtop"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
      cp "$bin" LICENSE README.md "$rpmtop/SOURCES/"
      cat >"$rpmtop/SPECS/mellow.spec" <<SPEC
Name:           mellow
Version:        $VERSION
Release:        1
Summary:        Modern, visual-first terminal text editor
License:        Apache-2.0
URL:            https://github.com/ritru-labs/mellow
BuildArch:      $ARCH
AutoReqProv:    no

%description
Mellow is a terminal editor with a calm, discoverable interface: tabs,
splits, project search, Git, an integrated terminal and optional AI.

%install
install -D -m 0755 %{_sourcedir}/mellow %{buildroot}%{_bindir}/mellow
install -D -m 0644 %{_sourcedir}/LICENSE %{buildroot}%{_docdir}/mellow/LICENSE
install -D -m 0644 %{_sourcedir}/README.md %{buildroot}%{_docdir}/mellow/README.md

%files
%{_bindir}/mellow
%doc %{_docdir}/mellow/README.md
%license %{_docdir}/mellow/LICENSE
SPEC
      rpmbuild --quiet --define "_topdir $rpmtop" --target "$ARCH" -bb "$rpmtop/SPECS/mellow.spec"
      cp "$rpmtop"/RPMS/*/mellow-*.rpm "$out/"
      rm -rf "$stage"
    '
done

# Bare file names, so SHA256SUMS lines match release asset names exactly.
(
  cd "$out"
  if command -v sha256sum >/dev/null; then
    sha256sum -- *.tar.gz *.deb *.rpm
  else
    shasum -a 256 -- *.tar.gz *.deb *.rpm
  fi
) >"$out/SHA256SUMS"
ls -la "$out"
