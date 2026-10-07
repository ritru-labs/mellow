# Mellow

**The calm terminal editor.** No modes, no manual: just start typing.

Mellow is an open-source terminal text editor crafted by **Ritru Labs**. It keeps
what makes terminal editors useful (speed, SSH, the keyboard) and drops the
learning curve: familiar keys, a mouse that works, and every command one search
away.

![Mellow in four themes](images/themes.png)

## Install

```bash
curl -fsSL https://github.com/ritru-labs/mellow/releases/latest/download/install.sh | sh
```

or with Homebrew:

```bash
brew install ritru-labs/tap/mellow
```

More ways, including `.deb` and `.rpm` packages: [Install](install.md).

## Start

```bash
mellow notes.txt     # open or create a file
mellow .             # open a project folder
```

Type. `Ctrl+S` saves, `Ctrl+Q` quits, `Ctrl+P` finds any command, and `F1`
shows the keys. [Getting started](getting-started.md) covers the first minute.

## What you get

- No modes: typing types, and the keys you already know work.
- Mouse support: click, drag, double-click, scroll.
- Six themes, a project file tree, tabs and split panes.
- Find and replace, in one file or the whole project.
- Smart indentation as you type, and optional format on save with your
  language's formatter (rustfmt, ruff, shfmt, prettier...).
- A Git panel: changes, staging, commit, branches, pull and push.
- A built-in terminal.
- Language servers for completion, problems and go to definition.
- Optional AI that shows every change for review before applying it.
- Crash recovery: unsaved work comes back after a crash.

Mellow 0.2.1 is a beta. It runs on macOS and Linux.
