# Mellow

**The calm terminal editor.** No modes, no manual: just start typing.

Mellow is an open-source terminal text editor, crafted by **Ritru Labs**. It
keeps what makes terminal editors useful (speed, SSH, the keyboard) and drops the
learning curve: familiar keys, a mouse that works, and every command one search
away.

![Mellow in four themes](docs/images/themes.png)

**Version 0.2.1, beta.** Runs on macOS and Linux. Documentation:
**[ritru-labs.github.io/mellow](https://ritru-labs.github.io/mellow/)**

## Install

macOS and Linux:

```bash
curl -fsSL https://github.com/ritru-labs/mellow/releases/latest/download/install.sh | sh
```

The script picks the right build, checks its checksum and installs `mellow` to
`~/.local/bin`.

With Homebrew (macOS and Linux):

```bash
brew install ritru-labs/tap/mellow
```

On macOS, use one of these two rather than downloading in a browser: the builds
are not yet signed by Apple, so macOS blocks browser downloads.

With Cargo (Rust 1.90 or newer):

```bash
cargo install mellow --locked
```

Debian/Ubuntu `.deb`, Fedora/RHEL `.rpm`, plain archives and building from
source: see [Install](https://ritru-labs.github.io/mellow/install/).

Windows is not supported; inside WSL, use the Linux install.

## The first minute

```bash
mellow notes.txt     # open or create a file
mellow .             # open a project folder
```

| To | Press |
| --- | --- |
| Type | just type; there are no modes |
| Save | `Ctrl+S` |
| Quit | `Ctrl+Q` (asks about unsaved files first) |
| Find any command | `Ctrl+P`, then type what you want |
| See the keys | `F1` |
| Undo | `Ctrl+Z` |
| Open a file | `Ctrl+O` |
| Show the file tree | `Ctrl+B` |
| Terminal | `Ctrl+T` (again to come back) |

Your terminal decides which keys reach Mellow; `F1` and `Ctrl+P` show only the
ones yours can send. All keys: [Keys](https://ritru-labs.github.io/mellow/keys/).

## What you get

- No modes: typing types, and the keys you already know work.
- Mouse support: click, drag, double-click, scroll.
- Six themes, a project file tree, tabs and split panes.
- Find and replace in a file or across the project.
- Smart indentation as you type, and optional format on save with your
  language's formatter (rustfmt, ruff, shfmt, prettier...).
- A Git panel: changes, staging, commit, branches, pull and push, all running in
  the background.
- A built-in terminal.
- Language servers for completion, problems and go to definition.
- Optional AI that shows every change for review before applying it.
- Crash recovery: unsaved work comes back after a crash.

## Settings

Change settings in the Settings screen (`Ctrl+P`, then "Open settings"), or edit
`~/.config/mellow/settings.conf`:

```ini
theme = tokyo-night
word_wrap = true
format_on_save = true   # tidy with rustfmt, ruff, shfmt, prettier... on save
```

Themes: `dark`, `light`, `high-contrast`, `tokyo-night`, `catppuccin-mocha`,
`gruvbox-dark`. Everything else:
[Settings](https://ritru-labs.github.io/mellow/settings/).

## Known limits

- Files must be UTF-8 and at most 100 MiB.
- Language servers and AI providers are supported but not yet tested end to end
  against every live server and account.
- Project search reads saved files only.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
```

The tests include real-terminal (PTY) checks of the built binary: edit, save,
crash and recover. `make verify` runs the same checks in Docker. Releases are
built by `.github/workflows/release.yml` when a `vX.Y.Z` tag is pushed; see
[`CHANGELOG.md`](CHANGELOG.md).

## Links

- Documentation: [ritru-labs.github.io/mellow](https://ritru-labs.github.io/mellow/)
- [Contributing](CONTRIBUTING.md) · [Security policy](SECURITY.md) ·
  [Code of conduct](CODE_OF_CONDUCT.md)
- License: [Apache-2.0](LICENSE)
