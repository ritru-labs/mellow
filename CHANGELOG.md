# Changelog

## 0.2.1 (beta)

Format on save for every language, smart Enter, and a faster crash journal.

### Added

- **Format on save** for every language: turn on "Format on save" in Settings
  (or `format_on_save = true`) and saving runs the file's standard formatter,
  such as rustfmt, ruff, shfmt, prettier, gofmt or terraform fmt. Off by
  default. A missing or failing formatter never blocks the save, and one
  `Ctrl+Z` undoes the formatting. `MELLOW_FORMAT_<LANGUAGE>` picks another one.
- **Smart Enter**: a new line keeps the current indentation and steps in after
  `{`, `(` or `[` (and `:` in Python and YAML); between brackets the closer
  moves to its own line.

### Changed

- The crash-recovery journal is written on a background thread, at most
  about once a second while you type, instead of being synced to disk on
  every keystroke. A slow disk no longer slows typing. Saving, closing and
  quitting still wait for it, so no stale draft is left behind.

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
- Tests that failed now and then on busy machines (a language-server timing
  check, the crash-recovery terminal tests and the terminal close prompt)
  are reliable.

## 0.1.3

First public preview.
