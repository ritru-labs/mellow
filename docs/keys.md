# Keys

`F1` shows the keys your terminal can send, and `Ctrl+P` finds every command by
name, so you never need to memorise this page.

## Everyday

| Key | Action |
| --- | --- |
| `Ctrl+S` | Save |
| `Ctrl+O` | Open a file (fuzzy search; type a path to create one) |
| `Ctrl+N` | New file |
| `Ctrl+F` | Find in file (press twice: all files) |
| `Ctrl+P` | Every command |
| `Ctrl+B` | Show or hide the file tree |
| `Ctrl+T` | Terminal (again: back to the editor) |
| `Ctrl+K` | Ask AI |
| `Ctrl+Z` | Undo |
| `Ctrl+W` | Close file |
| `Ctrl+Q` | Quit |
| `F1` | Help |

## More

| Key | Action |
| --- | --- |
| `Ctrl+Y` | Redo (also `Ctrl+Shift+Z`) |
| `Ctrl+H` | Replace, with a preview |
| `Ctrl+G` | Go to line (and column) |
| `Ctrl+PgDn` / `Ctrl+PgUp` | Next / previous file |
| `Ctrl+/` | Comment line |
| `Alt+Up` / `Alt+Down` | Move line |
| `Alt+Shift+Down` | Duplicate line |
| `Ctrl+\` | Split editor; `F6` moves between panes |
| `F12` | Go to definition (needs a [language server](language-servers.md)) |
| `Ctrl+Space` | Suggestions |
| `Alt+Z` | Word wrap |

## Moving and selecting

| Key | Action |
| --- | --- |
| Arrows, `PgUp`, `PgDn` | Move |
| `Home` / `End` | Line start (first non-blank, then column 0) / end |
| `Ctrl+Left` / `Ctrl+Right` | By word (`Alt+Left` / `Alt+Right` on macOS terminals) |
| `Ctrl+Home` / `Ctrl+End` | File start / end |
| `Ctrl+Backspace` / `Ctrl+Delete` | Delete a word |
| add `Shift` | Select while moving |

## Extra chords

These need a terminal with the kitty keyboard protocol (kitty, WezTerm, Ghostty,
foot and others):

| Key | Action |
| --- | --- |
| `Ctrl+Shift+F` | Find in all files |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous file |
| `Ctrl+Shift+S` | Save As (otherwise use the palette) |
| `Ctrl+Shift+G` | Git changes |
| `Ctrl+Shift+M` | Problems |

## In the terminal

Inside the built-in terminal, keys go to the shell: `Ctrl+C` interrupts the
running program instead of copying. `Ctrl+T` returns to the editor.
