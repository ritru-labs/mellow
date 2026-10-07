# Make your first edit

This walkthrough needs only an [installed Mellow binary](install.md) and an
interactive terminal. You do not need Git, a language server or an AI account.
Use a new practice folder so you can experiment without changing a project.

## 1. Open a new file

Run these commands in your shell:

```bash
mellow --version
mkdir mellow-practice
cd mellow-practice
mellow notes.txt
```

The version command should print `mellow` followed by the installed version.
The commands on this site describe **0.2.1 beta**. If `mellow` is not found,
follow the [PATH troubleshooting steps](faq.md#mellow-command-not-found).
If the practice folder already exists, choose another name.

`notes.txt` does not need to exist: Mellow opens a new buffer and creates the
file when you save. Dismiss the first-run help screen if shown. Type:

```text
Hello from Mellow.
Next: open a project folder.
```

Typing inserts text immediately. Use arrow keys to move, `Shift` with arrows
to select, and `Ctrl+Z` to undo. The mouse can position the cursor or select text.

## 2. Save and verify

Press **`Ctrl+S`** to write `notes.txt`, then **`Ctrl+Q`** to return to your shell.
If you have unsaved changes, Mellow asks what to do before quitting.

```bash
cat notes.txt
```

You should see the two lines you typed. This is the complete basic workflow:
open, edit, save, quit. A missing parent directory or a directory you cannot
write to can prevent saving; read the status message before quitting.

## 3. Open the folder

```bash
mellow .
```

Mellow opens the project folder and can restore its previous tabs. Press
`Ctrl+B` to show the file tree, or `Ctrl+O` and type `notes` to find the file.
Press `Enter` to open a selected picker result.

A file argument opens that file; a directory argument opens a workspace.
With no argument, `mellow` uses the current directory. The CLI accepts one
optional path, not a list of files; use the picker or tabs for more files.

## 4. Discover commands

Press **`Ctrl+P`**, type `settings`, and select **Open settings** to change a
setting such as your theme. Press `Esc` to leave an overlay and return to editing.
`F1` opens help. These are the main keys to learn:

| To | Press |
| --- | --- |
| Save / quit | `Ctrl+S` / `Ctrl+Q` |
| Undo / redo | `Ctrl+Z` / `Ctrl+Y` |
| Open a file / close a tab | `Ctrl+O` / `Ctrl+W` |
| Find text in this file | `Ctrl+F` |
| Find a command | `Ctrl+P` |
| Show help / file tree | `F1` / `Ctrl+B` |
| Open the shell / return to the editor | `Ctrl+T` |

## Terminal keys are part of the setup

On macOS, use **Control**, not Command. The terminal may intercept a shortcut,
and some combinations require enhanced keyboard reporting. Mellow adapts its
help and palette to the keyboard protocol it detects; terminal-level bindings
can still take precedence. Use the command palette if a shortcut does not arrive.

For the terminal's own mouse selection, start with `mellow --no-mouse`.
See the [keyboard reference](keys.md) for all defaults and custom bindings.

## Continue with a real task

[Everyday workflows](usage.md) walks through editing configuration, project
search, split panes, a built-in shell and Git review. Add
[language servers](language-servers.md) or [format on save](settings.md#format-on-save)
only when you need them. If a session is interrupted, see
[crash recovery](faq.md#where-did-my-unsaved-changes-go-after-a-crash).
