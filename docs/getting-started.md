# Getting started

## Open something

```bash
mellow notes.txt     # open a file, or create it on first save
mellow .             # open a project folder on its start page
mellow               # the current folder, with the files you had open there
```

## The first minute

| To | Press |
| --- | --- |
| Type | just type; there are no modes |
| Save | `Ctrl+S` |
| Quit | `Ctrl+Q` (it asks about unsaved files first) |
| Find any command | `Ctrl+P`, then type what you want, such as "theme" |
| See the keys | `F1` |
| Undo | `Ctrl+Z` |
| Open another file | `Ctrl+O` |
| Show the file tree | `Ctrl+B` |
| Open the terminal | `Ctrl+T` (again to come back) |

The mouse works too: click to move, drag to select, double-click a word, scroll
with the wheel. Every item in the status line at the bottom is clickable.

## Shortcuts depend on your terminal

Your terminal decides which key combinations reach Mellow. Mellow shows only the
keys yours can send, so `F1` and the command palette are always accurate. On
macOS, most terminals keep `Cmd` keys for themselves; use the `Ctrl` keys.

Terminals that support the kitty keyboard protocol (kitty, WezTerm, Ghostty,
foot and others) can send extra chords such as `Ctrl+Shift+F` and `Ctrl+Tab`.
See [Keys](keys.md).

## If something crashes

Mellow keeps a private recovery copy of unsaved edits. If your terminal, SSH
session or Mellow itself goes away, open the same file again and Mellow offers
**[R] Restore** or **[D] Discard journal**.

## Next

- [Keys](keys.md): the full list
- [Themes](themes.md) and [Settings](settings.md)
- [Language servers](language-servers.md) for completion and go to definition
- [AI](ai.md), if you want it
