# Settings

Most people change settings in the Settings screen (`Ctrl+P`, then "Open settings").
They are saved to a plain text file you can also edit:

```text
~/.config/mellow/settings.conf
```

(`$XDG_CONFIG_HOME/mellow/settings.conf` if you set `XDG_CONFIG_HOME`.)

## Example

```ini
theme = tokyo-night
word_wrap = true
copy_on_select = false
```

## All settings

| Setting | Default | Meaning |
| --- | --- | --- |
| `theme` | `dark` | One of the [themes](themes.md) |
| `word_wrap` | `false` | Wrap long lines on screen (`Alt+Z` toggles) |
| `show_whitespace` | `false` | Show spaces and tabs |
| `show_indent_guides` | `true` | Show indentation guides |
| `explorer_visible` | `false` | Show the file tree when Mellow starts |
| `auto_completion` | `true` | Suggest completions while typing |
| `copy_on_select` | `false` | Copy to the clipboard when you select with the mouse |

Lines starting with `#` are comments. An unknown setting or theme name is
reported with its line number. Mellow writes the file only when you change a
setting; starting Mellow never rewrites it.

## Environment variables

| Variable | Effect |
| --- | --- |
| `MELLOW_SETTINGS=/path/file.conf` | Use another settings file |
| `MELLOW_KEYBOARD_PROTOCOL=off` | Don't enable the kitty keyboard protocol |
| `MELLOW_LSP_RUST`, `MELLOW_LSP_PYTHON`, … | Use another [language server](language-servers.md) program |

## Command-line options

| Option | Effect |
| --- | --- |
| `--no-mouse` | Leave the mouse to your terminal, so its own text selection works |
| `--version` | Print the version |
| `--help` | Show all options |

## Where Mellow keeps things

| Folder | Contents |
| --- | --- |
| `~/.config/mellow` | `settings.conf`, and `ai.conf` if you set up AI |
| `~/.local/state/mellow` | Open tabs per project, and recovery copies of unsaved edits |
