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
| `format_on_save` | `false` | Tidy the file with its language's formatter when you save ([details](#format-on-save)) |

Lines starting with `#` are comments. An unknown setting or theme name is
reported with its line number. Mellow writes the file only when you change a
setting; starting Mellow never rewrites it.

## Environment variables

| Variable | Effect |
| --- | --- |
| `MELLOW_SETTINGS=/path/file.conf` | Use another settings file |
| `MELLOW_KEYMAP=/path/keybindings.conf` | Use another [keybindings file](keys.md#custom-keybindings) |
| `MELLOW_KEYBOARD_PROTOCOL=off` | Don't enable the kitty keyboard protocol |
| `MELLOW_LSP_RUST`, `MELLOW_LSP_PYTHON`, … | Use another [language server](language-servers.md) program |
| `MELLOW_FORMAT_PYTHON`, `MELLOW_FORMAT_SHELL`, … | Use another [formatter](#format-on-save) command |

## Format on save

Turn it on in Settings ("Format on save") or with `format_on_save = true`.
When you save, Mellow runs the file's standard formatter and saves the tidied
text. It never guesses indentation itself: in Python or YAML, indentation is
part of the meaning, so only the language's own formatter may change it.

| Language | Formatter (first one installed is used) |
| --- | --- |
| Rust | `rustfmt` |
| Python | `ruff format`, then `black` |
| Shell | `shfmt` |
| YAML | `prettier`, then `yamlfmt` |
| JSON | `prettier`, then `jq` |
| TOML | `taplo` |
| Terraform | `terraform fmt` |
| Go | `gofmt` |
| JavaScript, TypeScript, CSS, HTML, Markdown | `prettier` |
| C, C++, Java | `clang-format` |

- Install the formatters you want with your usual tools, for example
  `brew install ruff shfmt prettier`. Mellow does not bundle them.
- The formatter runs in the file's folder, so project settings such as
  `rustfmt.toml`, `pyproject.toml` or `.prettierrc` apply.
- Formatting is one edit: `Ctrl+Z` after saving shows your text as typed.
- If the formatter is missing, fails (for example on a syntax error) or takes
  over 10 seconds, the file is saved exactly as typed and the status line says
  why.
- Windows (CRLF) line endings are kept.
- To use another formatter, set `MELLOW_FORMAT_<LANGUAGE>` to a command that
  reads the file on stdin and prints the result; `{path}` is replaced by the
  file's path. For example:

    ```bash
    export MELLOW_FORMAT_PYTHON="black -q --stdin-filename {path} -"
    ```

While you type, Enter keeps the current indentation and indents one more
level after `{`, `(` or `[` (and after `:` in Python and YAML), in every file.

## Command-line options

| Option | Effect |
| --- | --- |
| `--no-mouse` | Leave the mouse to your terminal, so its own text selection works |
| `--version` | Print the version |
| `--help` | Show all options |

## Where Mellow keeps things

| Folder | Contents |
| --- | --- |
| `~/.config/mellow` | `settings.conf`, optional `keybindings.conf`, and `ai.conf` if you set up AI |
| `~/.local/state/mellow` | Open tabs per project, and recovery copies of unsaved edits |

Set `XDG_CONFIG_HOME` or `XDG_STATE_HOME` to change the corresponding base
folder; Mellow appends `/mellow`. `MELLOW_SETTINGS` overrides only the settings
file, not AI configuration, keybindings or recovery storage. Shell environment
changes apply to newly launched Mellow processes.

Recovery data may contain unsaved text. Keep it until you have recovered or
saved the work you need; deleting it is not a reset that preserves drafts.
