# Changelog

## 0.2.0 (beta)

First release you can install with one command.

### Install

- Prebuilt binaries for macOS (Apple silicon and Intel) and Linux (x86_64
  and ARM64, static, so they run on any distribution).
- `install.sh` downloads the right binary, checks its SHA-256 checksum and
  installs it to `~/.local/bin`.
- Homebrew tap, `.deb` and `.rpm` packages.

### Changed

- Git fetch, pull, push and commit run in the background. A slow network,
  an ssh wait or a long pre-commit hook no longer freezes the editor; the
  status bar shows progress and typing keeps working.
- New command **Cancel Git operation** stops a running fetch, pull, push or
  commit, including the hooks and ssh it started.
- A pull that finds edits made during its fetch stops instead of reloading
  over them.
- A failed commit keeps its message for the retry.

### Fixed

- The test suite and checks now pass on macOS.

## 0.1.3

First public preview.
