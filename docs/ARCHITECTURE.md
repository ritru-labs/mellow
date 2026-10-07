# Architecture and repository map

Mellow is a single Rust executable (`mellow`), using Crossterm for terminal
input, Ratatui for rendering and Ropey for text storage. It runs in the user's
terminal; it has no browser frontend or hosted backend. The website is a
separate MkDocs Material build from `docs/`.

## From input to a saved file

```text
CLI path → App startup → workspace, tabs, settings, recovery
                               │
keyboard / mouse / paste → commands and editor events
                               │
                         App + Buffer
                         │          │
                   terminal UI   local filesystem
                         │
          optional LSP, Git, shell and AI adapters
```

`main.rs` parses one optional file/directory path and `--no-mouse`, installs
cleanup handlers, starts the application and enters the terminal event loop.
`App` coordinates the active buffer, cursor, tabs, panes and overlays. A text
edit enters the buffer's undo history; saving writes the buffer to disk.

The editor is modeless for typing. Internal application modes represent UI
states such as search, settings or a confirmation dialog, not a Vim-style
insert/normal mode workflow.

## Where to read the implementation

| Source | Responsibility |
| --- | --- |
| `src/main.rs`, `src/brand.rs` | CLI, startup and shared product identity |
| `src/app.rs` | Event loop, editor state and command orchestration |
| `src/command.rs`, `src/keymap.rs`, `src/input.rs` | Action definitions, configurable key mapping and input handling |
| `src/buffer.rs`, `src/cursor.rs` | Rope text, positions, selections, undo/redo, file reads and safe saves |
| `src/ui.rs`, `src/visual.rs`, `src/theme.rs`, `src/terminal.rs` | Cell-based rendering, themes, terminal setup and cleanup |
| `src/workspace.rs`, `src/search.rs` | File discovery, fuzzy navigation, bounded text search and replacement |
| `src/settings.rs`, `src/session.rs`, `src/recovery.rs` | Preferences, project sessions and private crash journals |
| `src/syntax.rs`, `src/syntax_tree.rs` | Language detection and syntax parsing/highlighting |
| `src/lsp.rs`, `src/format.rs` | External language servers and command-line formatters |
| `src/git.rs`, `src/pty.rs` | System Git integration and Unix pseudoterminal shell sessions |
| `src/ai.rs`, `src/claude.rs` | Optional AI request, provider and setup support |
| `tests/`, `fixtures/` | Native terminal smoke tests and test inputs |
| `scripts/`, `packaging/`, `.github/workflows/` | Verification, installers, release packaging and CI |
| `docs/`, `mkdocs.yml` | This documentation site and navigation |

Browse the [source tree](https://github.com/ritru-labs/mellow/tree/main/src)
or follow the [development guide](development.md) to build it locally.

## Integration boundaries

**Text positions differ from terminal cells.** Tabs, combining characters and
wide glyphs need explicit conversions between byte, character, grapheme and
screen coordinates. The buffer does not use browser or DOM selection semantics.

**Commands are shared.** Keyboard mappings and palette actions resolve to the
same command vocabulary. User keybindings are implemented in
`keybindings.conf`; see the [keyboard reference](keys.md#custom-keybindings).

**External tools are optional.** Git uses the installed `git` executable; shells
run through a PTY; language servers communicate through LSP; format-on-save
commands read stdin and return text. Installing Mellow does not install those
tools. Network Git commands and AI calls can require network access even though
basic editing works offline.

**AI proposals do not execute tools.** Requests use the configured provider.
Selected-text changes require review and enter the ordinary undoable buffer
path. AI does not directly execute shell or Git actions. Optional inline
suggestions send nearby code after typing pauses; they are off by default.

## Storage and failure handling

Preferences and optional AI configuration use the config directory; sessions
and recovery journals use the state directory. Both honor XDG overrides; see
[storage locations](settings.md#where-mellow-keeps-things).

Saving normally writes a temporary file and replaces the destination, with
handling for symlinks, read-only files and filesystems where replacement is
unavailable. External modifications trigger a conflict prompt. Recovery
journals preserve unsaved text after interruption, but they are not backups or
version control.

## Current behavior versus future work

The [capability table](index.md#what-works-today) describes implemented features.
Older milestone lists mixed implemented work with aspirations; they should not
be used as a release checklist. Key configuration, Tree-sitter, LSP, Git and AI
adapters already exist in 0.2.1.

Native Windows support, autonomous agent/tool execution and comprehensive
hard-link, ACL, extended-attribute and locking hardening are not promised by
this release. Live LSP/provider compatibility also varies. Product and design
contracts describe intended experience; they are not guarantees of support.
See [limitations](faq.md) and the repository's
[changelog](https://github.com/ritru-labs/mellow/blob/main/CHANGELOG.md).
