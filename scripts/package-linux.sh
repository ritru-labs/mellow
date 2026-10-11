#!/usr/bin/env bash
# Build static Linux binaries and .tar.gz/.deb/.rpm packages into dist/linux.
#   scripts/package-linux.sh            # both x86_64 and aarch64
#   scripts/package-linux.sh x86_64     # one architecture
# x86_64 on an ARM Mac (or aarch64 on an x86 host) runs under Docker's
# emulation: useful for packaging, but not native release acceptance.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
[[ -z "$(git -C "$root" status --porcelain --untracked-files=all)" ]] || {
  echo 'Packaging requires a clean source checkout for exact-commit provenance' >&2; exit 1;
}
source_commit=$(git -C "$root" rev-parse HEAD)
archs=("${@:-x86_64 aarch64}")
read -r -a archs <<<"${archs[*]}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
# Linked worktrees reference their common Git directory by an absolute path.
# Expose only that metadata read-only; builds never mutate repository refs.
git_mount=()
if [[ -f "$root/.git" ]]; then
  git_common=$(cd "$root" && cd "$(git rev-parse --git-common-dir)" && pwd -P)
  git_mount=(-v "$git_common:$git_common:ro")
fi
out="$root/dist/linux"
mkdir -p "$out"

for arch in "${archs[@]}"; do
  case "$arch" in
    x86_64) platform=linux/amd64 deb_arch=amd64 ;;
    aarch64) platform=linux/arm64 deb_arch=arm64 ;;
    *) echo "unknown architecture: $arch" >&2; exit 2 ;;
  esac
  host_arch=$(uname -m)
  [[ "$host_arch" != arm64 ]] || host_arch=aarch64
  execution_kind=${MELLOW_EXECUTION_KIND:-native-container}
  if [[ "$host_arch" != "$arch" ]]; then
    execution_kind=emulated
  elif [[ "$(uname -s)" != Linux ]]; then
    execution_kind=linux-vm
  fi
  image="mellow-linux-package:$arch"
  docker build -q --platform "$platform" -t "$image" -f "$root/packaging/linux/Dockerfile" "$root/packaging/linux" >/dev/null

  docker run --rm --platform "$platform" \
    -v "$root:/workspace" \
    "${git_mount[@]}" \
    -v "mellow-package-target-$arch:/target" \
    -v "mellow-package-cargo-$arch:/usr/local/cargo/registry" \
    -e CARGO_TARGET_DIR=/target \
    -e ARCH="$arch" -e DEB_ARCH="$deb_arch" -e VERSION="$version" \
    -e MELLOW_PACKAGE_SOURCE_COMMIT="$source_commit" \
    -e MELLOW_EXECUTION_KIND="$execution_kind" \
    "$image" bash -euo pipefail -c '
      target="$ARCH-unknown-linux-musl"
      rustup target add "$target" >/dev/null
      cargo build --locked --release --target "$target"
      [[ "$(git rev-parse HEAD)" == "$MELLOW_PACKAGE_SOURCE_COMMIT" &&
         -z "$(git status --porcelain --untracked-files=all)" ]] || {
        echo "Source changed during packaging; candidate is not qualified" >&2; exit 1;
      }
      bin="/target/$target/release/mellow"
      file "$bin" | grep -qE "static(ally|-pie) linked" || { echo "not static: $(file "$bin")" >&2; exit 1; }
      "$bin" --version

      name="mellow-$VERSION-$target"
      stage="$(mktemp -d)"
      out=/workspace/dist/linux

      # Plain tarball
      mkdir -p "$stage/$name"
      cp "$bin" README.md LICENSE "$stage/$name/"
      {
        echo "source_commit=$MELLOW_PACKAGE_SOURCE_COMMIT"
        echo "target=$target"
        echo "execution_kind=$MELLOW_EXECUTION_KIND"
        rustc -vV
      } > "$stage/$name/BUILD.txt"
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
      # Test the extracted musl archive on this runner, then independently
      # install/remove both package formats in this disposable container.
      if [ "$MELLOW_EXECUTION_KIND" != emulated ]; then
      bash scripts/native-ci.sh artifact-install "$out/$name.tar.gz"
      dpkg --install "$out/mellow_${VERSION}_${DEB_ARCH}.deb"
      MELLOW_SMOKE_BINARY=/usr/bin/mellow cargo test --locked --test release_native_smoke -- --nocapture
      {
        echo "source_commit=$MELLOW_PACKAGE_SOURCE_COMMIT"
        echo "package_sha256=$(sha256sum "$out/mellow_${VERSION}_${DEB_ARCH}.deb" | awk "{print \$1}")"
        echo "binary_sha256=$(sha256sum /usr/bin/mellow | awk "{print \$1}")"
        echo "execution_kind=$MELLOW_EXECUTION_KIND"
        echo "install_pty=passed"
      } > "$out/mellow_${VERSION}_${DEB_ARCH}.deb.acceptance.txt"
      dpkg --remove mellow
      test ! -e /usr/bin/mellow
      echo "uninstall=passed" >> "$out/mellow_${VERSION}_${DEB_ARCH}.deb.acceptance.txt"
      rpm_package=$(find "$out" -maxdepth 1 -name "mellow-${VERSION}-*.${ARCH}.rpm" -print -quit)
      test -n "$rpm_package"
      rpm --install "$rpm_package"
      MELLOW_SMOKE_BINARY=/usr/bin/mellow cargo test --locked --test release_native_smoke -- --nocapture
      {
        echo "source_commit=$MELLOW_PACKAGE_SOURCE_COMMIT"
        echo "package_sha256=$(sha256sum "$rpm_package" | awk "{print \$1}")"
        echo "binary_sha256=$(sha256sum /usr/bin/mellow | awk "{print \$1}")"
        echo "execution_kind=$MELLOW_EXECUTION_KIND"
        echo "install_pty=passed"
      } > "$rpm_package.acceptance.txt"
      rpm --erase mellow
      test ! -e /usr/bin/mellow
      echo "uninstall=passed" >> "$rpm_package.acceptance.txt"
      else
        echo "Cross/emulated packages were built; native artifact acceptance was not run" >&2
      fi
      rm -rf "$stage"
      [[ "$(git rev-parse HEAD)" == "$MELLOW_PACKAGE_SOURCE_COMMIT" &&
         -z "$(git status --porcelain --untracked-files=all)" ]] || {
        echo "Source changed during packaging; candidate is not qualified" >&2; exit 1;
      }
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
