# Changelog

## 0.3.1 (beta)

The October deep audit, plus fixes found while testing it on Ubuntu and CentOS.

### Security

- AI requests no longer follow redirects, so the API key header cannot be sent to a redirect target.
- The AI configuration prints the key as `<redacted>` in debug output.

### Changed

- AI shell suggestions stay in read-only review; copy is explicit and no suggestion enters a PTY. All provider redirects are refused, context excludes credentials, streamed proposals validate before review, and local transport is cancellable with session limits/unknown usage shown.
- Safe saves retain supported metadata and refuse hard-link splitting; filesystem conflict/publication checks strengthened.
- Formatting, discovery, search and clipboard adapters run off the interaction path with deadlines, cancellation or stale-result guards.
- Shared Unicode multi-cursor/rectangle paste, configurable indentation/EditorConfig, Save All/close batches, local history/privacy controls, trash restore, navigation, outline, linked snippets and discoverable explorer/binding commands.
- Pooled language servers, resolved completion/import edits, synchronized multi-file edits and scoped Problems.
- Small local Safe Configuration Change and Terminal Doctor workflows; no automatic deployment or shell execution.
- Version labels checked from Cargo; native candidate/artifact acceptance and provenance/signing hooks documented.
- File tree refreshes while visible, so files created from the built-in terminal appear without saving or reopening the tree.
- Scripts without a file extension are highlighted as Shell when their first line is a `#!` line for sh, bash, zsh, dash or ksh.
- Mellow exits when the shell or SSH session that started it ends, or when its terminal hangs up. Previously a session left behind after a disconnect kept spinning at full CPU.
- Tests no longer depend on the host umask, kernel timestamp resolution or Ctrl+C timing under load; the AI exclusions doc states the file-permission rule.
- Welcome start page redesigned: a rounded card, a RECENT section for recently opened files, and a rule under the title. The first-run help box no longer covers it.

See [audit tracker](docs/audit/TRACKER.md) for actual platforms, integration versions, deferred items and remaining release gates.

## 0.3.0 (beta)

An agent-style look, mouse support across overlays, and a set of editing features.

### Added

- **Go to symbol** (`Ctrl+Shift+O`) for Rust, Python and shell files, and a breadcrumb that ends with the function the cursor is in.
- **Recent files first** in Open file.
- **Commit message draft** (`Ctrl+G` in the commit box, needs AI).
- **Fix problem on this line with AI**, **Ask AI for a shell command** (typed into the terminal, not run), and **Undo last AI edit** with a session list of AI edits.
- **Merge conflicts:** keep ours, theirs or both, one undoable edit each; the status line names them inside a conflict.
- **Keymap presets:** `preset = vscode` and `preset = nano`.

### Changed

- **Mouse works in overlays:** click a row in the palette, Open file, Go to symbol, references, Git lists and problems; click dialog buttons; the wheel scrolls lists; a click outside a list closes it.
- **Recovery:** Esc decides later without losing a draft; Discard all clears several drafts; blank drafts are removed without a prompt.
- **First keystroke is kept:** any key on the welcome screen dismisses it and reaches the file.
- **Status colours follow what the app reports**, not the words in the message.
- Ctrl+Space in the built-in terminal goes to the shell; Ctrl+T leaves the terminal.
- The built-in terminal tells programs its real colour level (`TERM=xterm` on 16-colour terminals).
- Copies over 256 KB stay in Mellow when no system clipboard is available.
- ASCII fallback covers borders, markers and separators, for terminals without Unicode.
- Save As never replaces a file that appears while saving; saves written in place say so.
- The 16-colour light theme uses colours that read on white.
- Errors no longer mention an old version number.

### Also in this release

- The AI review panel opens with a bullet headline and a change count (`+2 added −0 removed`) above the diff.
- The "working" state shows a spinner (an ASCII fallback is used without Unicode symbols) and fits at 80×24.
- The command palette, quick open, references, quick fixes, problems, branches, history, changes and project search open with a `●` marker in their titles (`*` without Unicode).

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
