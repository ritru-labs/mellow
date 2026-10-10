# Keys

`F1` shows the keys your terminal can send, and `Ctrl+P` finds every command by
name, so you never need to memorise this page. A terminal can still intercept its own
shortcuts before Mellow receives them.

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
| `Ctrl+Shift+O` | Go to symbol: jump to a function, type or other definition (Rust, Python, Shell) |

## Without the kitty protocol

Terminals such as Terminal.app do not send the `Shift` chords above: `Ctrl+Shift+O`
arrives as plain `Ctrl+O`, which opens a file. Everything in those tables is
still in **Ctrl+P**, so the palette is the reliable path. Mellow shows
the keys your terminal can send in `F1`.

## In dialogs

Lists (the palette, Open file, Go to symbol, references and Git lists) take
clicks and the mouse wheel. Confirmations and text prompts have buttons, such as
`[Enter] Commit` and `[Esc] Cancel`, and `Esc` closes any dialog.

In the commit box, `Ctrl+G` drafts a commit message from your staged changes
with AI. The draft goes into the box for you to read; nothing is committed until
you press `Enter`.

## In the terminal

Inside the built-in terminal, keys go to the shell: `Ctrl+C` interrupts the
running program instead of copying. `Ctrl+T` returns to the editor.

## Custom keybindings

Create `~/.config/mellow/keybindings.conf` (or use
`$XDG_CONFIG_HOME/mellow/keybindings.conf`). Each line maps a command ID to one
or more comma-separated keys. For example, keep `Ctrl+S` and add `F2` for save:

```ini
file.save = Ctrl+S, F2
```

Use **Reload keybindings** in the palette after editing, or restart Mellow.
`MELLOW_KEYMAP=/path/keybindings.conf` selects another file. Set a command to
`none` to remove its bindings. Conflicting bindings, unknown command IDs and
unsupported chords are reported as errors; fix the file instead of assuming
the override took effect. The full command IDs are defined in
[`src/command.rs`](https://github.com/ritru-labs/mellow/blob/main/src/command.rs).

## Keymap presets

The keymap file can start from a preset. Add one line, and your own
`command.id = binding` lines still win over it:

```ini
preset = vscode   # Ctrl+P opens files; Ctrl+Shift+P or F1 opens commands
```

`default` keeps Mellow's own keys. `vscode` gives Ctrl+P for files and
Ctrl+Shift+P or F1 for commands. `nano` keeps nano's habits: Ctrl+O saves,
Ctrl+X quits, Ctrl+W searches, Ctrl+K cuts, Ctrl+U pastes and Ctrl+R opens a
file. Close tab moves to Alt+W and Ask AI to Alt+K to make room. Many terminals
send Ctrl+Shift+P as plain Ctrl+P, so with `vscode` use F1 for commands.
