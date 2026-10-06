# Mellow architecture

## Core flow

```text
keyboard / paste / mouse / command palette / future AI intent
                           │
                           ▼
                       Command
                           │
                           ▼
                          App
               ┌───────────┼────────────┐
               ▼           ▼            ▼
             Buffer      Cursor       Views
               │                        │
               ▼                        ▼
          filesystem                 renderer
```

Inputs do not own editing logic. They resolve to commands or editor events, while `App` coordinates state transitions. That is the flexibility boundary that later allows configurable shortcuts, richer mouse behavior, command palette actions and natural-language intents without duplicating editor behavior.

## Current modules

- `buffer.rs` — real filesystem bytes, Rope storage, Unicode edit positions, line endings, undo/redo and safe save
- `command.rs` — editor action vocabulary
- `keymap.rs` — Crossterm key events → commands
- `app.rs` — event loop, modes, state transitions, viewport and mouse mapping
- `ai.rs` — optional provider adapter, bounded request/response contract and provider HTTP isolation
- `terminal.rs` — raw mode, alternate screen, bracketed paste, mouse/focus capture and cleanup
- `ui.rs` — responsive cell-based rendering and overlays
- `theme.rs` — TrueColor / ANSI-256 / basic capability palette

## Boundaries we preserve

### Editor core does not know an AI provider

AI proposals now enter through `ai.rs`: the adapter can call an explicitly configured provider, but the editor core remains provider-agnostic. Only disclosed active-file/selection context is sent. Responses are read-only unless they contain a selected-text replacement, and that replacement still requires explicit review before entering the normal undoable buffer path. AI does not directly execute shell, Git, filesystem, or tool actions. Core editing stays fully usable offline.

### Keybindings are a mapping layer

The current defaults are not the engine. A future user keymap can remap actions without rewriting editor logic.

### Browser semantics do not leak into the buffer

The production buffer works with filesystem bytes, Unicode graphemes and terminal cells — not DOM selections or JavaScript string indexes.

### Text positions and screen cells are different

Tabs, combining marks and wide characters make a byte/char/grapheme index different from a terminal X coordinate. The renderer and buffer expose explicit conversions instead of assuming `string.len()` equals screen width.

### Saving must respect filesystem meaning

Mellow writes through an existing symlink target rather than replacing the link during an atomic save. More filesystem safety cases (hard links, ACLs, xattrs, locking) remain future hardening work.

## Planned milestones

```text
M0  native file editing + visual shell
M1  selection + clipboard adapters + find/replace
M2  config/keymaps + file picker/workspace navigation
M3  Tree-sitter syntax + large-file hardening
M4  fuzzy workspace explorer
M5  LSP adapter
M6  Git surfaces
M7  AI intent + inline diff proposals
M8  agent/tool adapters with explicit approval boundaries
```
