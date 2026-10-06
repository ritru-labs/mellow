# Mellow

**The calm terminal editor.** No modes, no manual: just start typing.

Mellow is an open-source, visual-first terminal text editor crafted by **Ritru Labs**. The goal is to keep what makes terminal editors indispensable — speed, portability, SSH friendliness and keyboard depth — while replacing the old learning curve with a calm, discoverable interface.

![Mellow in four themes](docs/images/themes.png)

## Release status

**Current version: 0.1.3 — early preview.** Mellow is a working native editor; it is new, so expect rough edges (see *Known limits*).

| Platform | Status |
| --- | --- |
| macOS (Apple silicon) | Supported; full native test gate |
| Linux x64 and ARM64 | Supported; full native test gate |
| Windows | Not supported (the terminal integration is Unix-only) |

The release gate goes beyond unit tests: the compiled `mellow` binary is launched under a real PTY, a file is edited and saved and verified on disk, a crash is simulated and unsaved text recovered, and terminal restoration is checked. It also runs locked format/check/clippy/tests, an optimized build, `cargo package` and a source-install smoke with `mellow --version`.

### Known limits

- Files must be UTF-8 and at most 100 MiB; larger or binary files are refused.
- Language servers and AI providers are supported but not yet qualified end-to-end against live servers and accounts.
- Project search reads saved files only and has size and result caps.
- Host terminals decide which shortcuts reach Mellow (for example, most macOS terminals keep `Cmd` keys for themselves).

## The product promise

```bash
mellow config.yaml
```

A first-time user should immediately understand how to type, move, save and quit. No insert mode. No required command vocabulary. Power shortcuts remain first-class, but the interface teaches itself.

## Product principles

1. **Visual quality is functionality.** 80×24 must feel intentional, not like a degraded desktop IDE.
2. **Zero required memorization.** Familiar keys and visible affordances work immediately.
3. **Keyboard depth without keyboard dependence.** Experts get shortcuts; beginners get discoverability.
4. **AI is optional.** Core editing remains complete offline. AI will be an interface layer, not the editor engine.
5. **Terminal-native.** Mellow must work locally, over SSH and without Electron or a GUI runtime.
6. **Commands are the engine contract.** Keyboard, mouse, palette and future AI intents resolve into the same editor actions.

## v0.1.0 — native editor foundation

Highlights in v0.1.0:

- open an existing UTF-8 file or create a new file path
- fullscreen alternate-screen TUI
- normal typing with no modal editing requirement
- arrows, smart Home (first non-blank, then column 0)/End, PageUp/PageDown
- `Ctrl+Left/Right` move by word (`Alt+Left/Right` on macOS terminals), `Ctrl+Backspace`/`Ctrl+Delete` delete a word, `Ctrl+Home/End` jump to file start/end; add `Shift` to select
- Enter, Backspace, Delete and Tab
- bracketed paste handling
- mouse click-to-place-cursor and wheel scrolling
- `Ctrl+S` save, `Ctrl+N` new file, Save As (`Ctrl+Shift+S` where the terminal can send it, or the palette) and `Ctrl+Q` safe quit
- `Ctrl+Z` undo and `Ctrl+Y` / `Ctrl+Shift+Z` redo; a run of typing undoes as one step (a space, a 1-second pause or any other edit starts the next)
- `Ctrl+P` searchable command palette with grapheme-aware editable input
- `Ctrl+O` Quick Open with bounded project discovery, fuzzy filtering, direct path entry, and workspace-scoped `Tab` path completion
- multi-buffer file tabs with mouse switching and per-file editor state
- `Ctrl+PgDn` / `Ctrl+PgUp` file switching (also `Ctrl+Tab` / `Ctrl+Shift+Tab` in terminals with the kitty keyboard protocol) and `Ctrl+W` safe tab close
- durable workspace session restore for persisted tabs, active file, cursor/scroll state and visual preferences
- `Ctrl+B` hierarchical project explorer with expand/collapse, keyboard/mouse navigation, safe file + directory creation, file/directory rename-move, confirmation-gated file delete, and empty-directory-only delete
- `Ctrl+\\` split editor with `F6` pane focus, pane-safe tab switching, side-by-side or top/bottom orientation, draggable 20–80% sizing on either axis, and persisted split layout
- `Ctrl+F` Find in file and `Ctrl+G` Go to line:column
- `Ctrl+H` Replace with live before→after preview, single-match replace, and one-transaction Replace All
- project-wide search (`Ctrl+F` pressed twice, or `Ctrl+Shift+F`) with case-sensitive, regex, and whole-word controls
- exact project-search navigation that opens the file and selects the matched Unicode/grapheme range
- embedded incremental Tree-sitter parsing for Rust, Python, JSON, Shell, YAML, and Terraform/HCL
- automatic LSP completion while typing (debounced, server trigger-aware, stale-response safe) with `Ctrl+Space` as a manual override (words from the file when no language server is installed), plus `F12` definition navigation when the matching language server is installed
- Problems surface (click "N problems" in the status line, `Ctrl+Shift+M`, or the palette) combining Tree-sitter syntax errors with LSP diagnostics
- diagnostic gutter markers plus parser/LSP/problem status feedback
- Git repository discovery, branch/status tracking, and background refresh using the system Git CLI
- Added / Modified / Deleted Git markers in the editor gutter
- Status-bar badge for the open file (New / Modified / Partly staged / Staged / Conflict) with one-click Stage or Unstage, plus unpushed/unpulled counts (`main ↑2 ↓1`) next to the branch
- Changes review (click the branch in the status line, `Ctrl+Shift+G`, or the palette) listing each change with its line, +/- counts and diff
- bounded hunk stage/unstage plus confirmation-gated unstaged hunk revert
- Git mutation safety that blocks operations against unsaved editor buffers and never deletes untracked files through Revert
- double-click word selection, triple-click logical-line selection, drag-selection autoscroll, and optional copy on select (off by default)
- `Ctrl+/` comment toggle, `Alt+Up/Down` line movement and `Alt+Shift+Down` line duplication
- automatic bracket/quote pairing with closing-delimiter skip behavior
- six themes: Dark, Light, High Contrast, Tokyo Night, Catppuccin Mocha and Gruvbox Dark, chosen in Settings, with Cycle Theme in the palette, or with `theme = tokyo-night` (`dark`, `light`, `high-contrast`, `catppuccin-mocha`, `gruvbox-dark`) in `~/.config/mellow/settings.conf`; the theme applies to every project
- toggleable whitespace markers and indentation guides
- `Alt+Z` real soft word wrap backed by a source-to-visual coordinate engine
- wrapped-line Up/Down/Page navigation, cursor visibility, selection, mouse hit-testing and drag autoscroll
- `F1` quick-help overlay
- one-time first-run onboarding with the core modeless shortcuts and discoverability paths
- persistent global Settings surface for theme, word wrap, whitespace, indent guides, and explorer default
- `Ctrl+K` Ask AI next to the cursor with explicit context disclosure, explanations, and review-before-apply edits; optional grey typing suggestions
- line numbers, current-line emphasis, new/modified/saved lifecycle state and status feedback
- TrueColor, ANSI-256 and baseline color fallbacks
- LF/CRLF preservation and UTF-8 BOM preservation
- grapheme-aware cursor operations and wide-character-aware rendering
- atomic same-directory saves
- file-identity/content-fingerprint conflict detection before overwrite
- periodic external-change polling that auto-reloads clean open files while preserving dirty buffers behind Save Conflict protection
- large-file reduced-intelligence mode (>5 MiB) that keeps core editing available while pausing Tree-sitter/LSP work
- explicit Save Conflict flow with Cancel, Save As and deliberate Overwrite
- symlink-safe saves for existing files
- owner-only dirty-buffer recovery journal with startup Restore/Discard
- real PTY-backed integrated shell using the configured `$SHELL`; `Ctrl+T` (or ``Ctrl+` `` where supported) moves between editor and terminal
- resizable terminal pane with editor/terminal focus isolation and multiple shell sessions
- terminal input routing where Ctrl+C reaches the PTY instead of editor Copy
- VT screen-model terminal rendering with cursor movement, alternate screen, SGR colors/attributes, bracketed paste, application-cursor keys, and SGR/xterm mouse forwarding
- terminal restoration on clean exit/panic and catchable signal shutdown routing

## Boundaries

The core editor is release-ready for the currently claimed native platforms, but Mellow deliberately keeps several boundaries explicit:

- **Platforms:** Linux and macOS are verified release targets. Windows remains a later ConPTY/platform-adapter milestone.
- **Syntax:** Tree-sitter parsing and query-based semantic highlighting cover Rust, Python, JSON, Shell, YAML, and Terraform/HCL. Unsupported captures fall back to the lexical renderer.
- **LSP:** language servers are external executables; Mellow does not bundle them. Multi-file WorkspaceEdits are reviewed as dirty buffers with coordinated all-or-nothing Undo/Redo rather than silently committed to disk.
- **Terminal:** Mellow uses a real PTY plus an in-memory VT screen model with cursor movement, alternate screen, SGR attributes, bracketed paste, application-cursor keys, and SGR mouse forwarding. Exhaustive xterm/legacy-protocol compatibility remains future depth.
- **Git:** normal workflows are built on the user's system Git. Pull is fast-forward-only, push is non-force, and conflict resolution is explicit/manual rather than hidden automation.
- **AI:** AI is optional and disabled until configured. Provider edits are always review-before-apply and never execute terminal, Git, filesystem, or tool commands directly.
- **Future depth:** additional language grammars, deeper terminal compatibility, broader AI provider/tool adapters, optional transactional multi-file disk-save/commit semantics, and Windows production support are post-v0.1.0 work.

## Optional AI assistant

AI stays off until you set it up, and Mellow only sends code when you ask. Press
`Ctrl+K` (or open Settings > AI assistant) and choose a provider:

- **Claude (Anthropic)**: native Messages API, default model `claude-opus-5-5`
- **OpenAI**, **Gemini** (Google), **Ollama** (runs locally, no key) or any **OpenAI-compatible** endpoint

Paste your key once. The setup is saved to `~/.config/mellow/ai.conf`, readable only
by you; `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `GEMINI_API_KEY` are used if you prefer not to save a
key. Environment variables still work and take precedence:

```bash
export MELLOW_AI_PROVIDER="claude"          # claude | openai | gemini | ollama | custom
export MELLOW_AI_ENDPOINT="https://api.anthropic.com/v1/messages"
export MELLOW_AI_MODEL="claude-opus-5-5"
export MELLOW_AI_API_KEY="..."              # optional for local/no-auth providers
export MELLOW_AI_INLINE=1                   # optional typing suggestions
```

Ask AI opens next to the cursor and states exactly what will be sent ("lines 13–16
of scripts/deploy.sh to Claude. Nothing else."). Tab offers Explain / Fix problems /
Write tests. With a selection, a suggested edit is shown as a diff and applied only
when you accept it, as one undoable edit. Optional typing suggestions (off by
default) show grey text after a pause at the end of a line you just typed; Tab
accepts. AI never executes terminal, Git, filesystem, or tool commands.

## Install and run

Mellow targets native Linux and macOS.

### Linux packages (any distribution)

`make linux-packages` builds static binaries (no system library requirements, so they run on old and new distributions alike) for x86_64 and aarch64 into `dist/linux/`:

```bash
# RHEL, Rocky, Alma, CentOS, Fedora
sudo dnf install ./mellow-<version>-1.x86_64.rpm      # or: sudo yum install ...
# Debian, Ubuntu
sudo apt install ./mellow_<version>_amd64.deb
# Anything else: unpack and put it on your PATH
tar -xzf mellow-<version>-linux-x86_64.tar.gz && sudo cp mellow-<version>-linux-x86_64/mellow /usr/local/bin/
```

Verify downloads against `SHA256SUMS`. A hosted yum/apt repository (so `yum install mellow` works without a file) comes with the public release.

### Homebrew (macOS and Linux)

Available once the repository is public:

```bash
brew install ritru-labs/mellow/mellow
```

The formula lives in [`packaging/homebrew/mellow.rb`](packaging/homebrew/mellow.rb) and builds from the tagged source. After tagging a release, run `scripts/update-homebrew-formula.sh vX.Y.Z` and copy the formula to `Formula/mellow.rb` in the `ritru-labs/homebrew-mellow` tap.

### From source

```bash
cargo install --locked --path .
mellow --version
mellow path/to/file
```

### Release artifacts

The release workflow is configured to produce platform/architecture-named binary archives plus SHA-256 checksums for `v*` tags. When a binary artifact is available, verify its checksum before extracting and running it.

For repository development, Docker remains the canonical reproducible loop. Release candidates additionally run native Ubuntu/macOS CI and the native PTY acceptance smoke, so terminal behavior is not inferred from Docker alone.

See [`docs/RELEASE_CHECKLIST.md`](docs/RELEASE_CHECKLIST.md) for the complete release gate.

## Docker-first development

Docker is the canonical local loop so development stays reproducible on macOS and Linux.

```bash
make verify
```

Runs:

```text
cargo fmt --check
cargo check --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Run the editor interactively:

```bash
make run FILE=fixtures/demo.rs
```

Unicode smoke test:

```bash
make run FILE=fixtures/unicode.txt
```

To keep your terminal's native mouse text selection instead of Mellow mouse capture:

```bash
docker compose run --rm mellow-dev cargo run -- fixtures/demo.rs --no-mouse
```

## Repository map

```text
src/                 Native editor engine + TUI
docs/                Product, architecture, design and test contracts
fixtures/             Manual/automated editor fixtures
scripts/              Verification helpers
.github/workflows/    CI safety net
```

## Design

See [`docs/DESIGN_CONTRACT.md`](docs/DESIGN_CONTRACT.md) for the visual and interaction rules.

## Contributing

Mellow is intended to be developed in small, reviewable steps. Read [`CONTRIBUTING.md`](CONTRIBUTING.md) and run `make verify` before pushing.

## License

Apache-2.0. See [`LICENSE`](LICENSE).

## Workspace comfort

`mellow .` opens the project on a start page (open, new file, search, terminal,
changed files) with the file tree beside it. The title bar holds the project,
branch, tabs and Ask AI / Open / Save / Quit; the row below shows where you are and
whether the file is saved. The file tree marks changed files with Git letters and
unsaved files with a dot. The status line uses plain words, every item is clickable,
and less important items step aside on narrow terminals instead of overlapping.

Shortcut labels follow your terminal. When it supports the kitty keyboard protocol
(kitty, WezTerm, Ghostty, foot and others) Mellow enables it and chords such as
`Ctrl+Shift+F` and `Ctrl+Tab` work; otherwise Help and the palette show the
fallbacks that do (`Ctrl+F` twice, `Ctrl+PgDn`, `Ctrl+T`). Set
`MELLOW_KEYBOARD_PROTOCOL=off` to keep the legacy encoding.

Copy on select is off by default, so a drag never replaces the clipboard. Turn it
on in Settings or set `copy_on_select = true` in the settings file.
Preferences are written only when changed explicitly; launch never rewrites them.
Clipboard feedback distinguishes system success, an unconfirmed terminal request,
and Mellow's internal fallback. Explicit Copy/Cut/Paste remain available either way,
and selecting text offers Copy / Cut / Ask AI in the status line for mouse users.

Quit starts with Cancel selected. Left/Right, Tab/Shift+Tab, Enter, Escape and the
visible mouse choices share the same safe flow. Each unsaved file is reviewed in
sequence. A failed save keeps edits and the application open; Retry Save, Save As
and return-to-editor choices remain available.

Mac Command shortcuts may be intercepted by the host terminal before Mellow sees
them. Use the advertised Ctrl bindings or visible actions; in terminal focus,
Ctrl+C interrupts the PTY. Terminal-native selection remains distinct from editor
selection (`--no-mouse` opts out of editor mouse capture).
