# Troubleshooting and limitations

## Does Mellow run on Windows?

Not natively; the built-in terminal is Unix-only for now. Inside WSL, install the
Linux build.

## macOS says it can't check the app for malicious software

You downloaded the archive in a browser. The builds are not yet signed by Apple,
so macOS may block browser downloads. Install with Homebrew or the
[install script](install.md) instead. Do not disable system security protections to run an unverified download.

## A shortcut does nothing

Your terminal may keep that key for itself (on macOS, most terminals keep `Cmd`
keys). `F1` and the command palette (`Ctrl+P`) show only keys your terminal can
send, and every action is in the palette.

## I want my terminal's own text selection back

Start Mellow with `mellow --no-mouse`, and your terminal handles the mouse.

## Where did my unsaved changes go after a crash?

Open the same file (or folder) again and choose **[R] Restore**. Recovery copies
are kept, readable only by you, in `~/.local/state/mellow`.

## Can Mellow damage my files?

Saves are atomic: the new text is written to a temporary file and moved into
place, to reduce the risk of partial writes. Filesystem-specific fallbacks exist, so
this is not a universal durability guarantee or a replacement for backups. If a file changed on disk
since you opened it, Mellow asks before overwriting. Files that are not UTF-8,
or larger than 100 MiB, are refused rather than mangled.

## Does Git in Mellow do anything risky?

It uses your system `git`. Pull is fast-forward only, push never forces, and
Revert never deletes untracked files. Fetch, pull, push and commit run in the
background with a progress line; "Cancel Git operation" in the palette stops
them.

## Does Mellow send my code anywhere?

[AI requests](ai.md) send the disclosed context to the configured provider.
Opt-in inline suggestions also send nearby code after typing pauses. Git
fetch/pull/push and installed language-server processes can use the network.
Core file editing works offline.

## Is it free?

Yes. Mellow is open source under the Apache-2.0 license.

## Where do I report a bug?

On [GitHub issues](https://github.com/ritru-labs/mellow/issues). For security
problems, follow the
[security policy](https://github.com/ritru-labs/mellow/blob/main/SECURITY.md).

## `mellow`: command not found

Check where you installed the binary with `command -v mellow`. The release
installer defaults to `~/.local/bin`; Cargo normally uses `~/.cargo/bin`.
For the release installer's default, try this in your current shell:

```bash
export PATH="$HOME/.local/bin:$PATH"
mellow --version
```

If that works, add the same export to the appropriate startup file for your
shell. If several versions are installed, `command -v mellow` shows which one
runs. Avoid reinstalling repeatedly before checking PATH.

## Completion or go to definition is missing

Install the matching [language server](language-servers.md), ensure its
executable is on the PATH inherited by Mellow, and reopen the file. Use
**Language server status** or **Restart language server** in the palette.
Server capabilities vary; syntax highlighting alone does not imply an LSP
server is running. Files over 5 MiB do not use language servers.

## Format on save did not change my file

It is off by default. Enable it in [Settings](settings.md#format-on-save),
install a supported formatter and read the status line after saving. A missing,
failed or timed-out formatter leaves the original text saved. **Format document**
is a separate language-server command and may not be supported by your server.

## Project search missed a match

Check the workspace folder, Git ignore rules and the search summary. The search
uses unsaved text for discovered open files and disk content for the others.
It skips binary, unreadable and over-2-MiB files and stops at its scan/result
limits. Save a newly created file so it can be discovered. See
[project search behavior](usage.md#find-files-and-text-across-a-project).

## Git pull or push failed

Mellow uses system Git and your existing repository setup. Check `git status`
and `git remote -v` in your shell. Authentication prompts are disabled in the
editor's Git subprocesses; resolve authentication through your normal shell
workflow. A diverged branch cannot be pulled with the editor's fast-forward-only
operation. Review the repository in your shell rather than assuming Mellow
will merge or force-push it.

## What files and platforms are supported?

Files must be UTF-8 and at most 100 MiB. Files over 5 MiB open with reduced
functionality, without parsing or language servers. Native Windows is not
supported; WSL uses the Linux build. Unix file metadata edge cases such as hard
links, ACLs and extended attributes still need further hardening.

When reporting a problem, include the Mellow version, OS/architecture, terminal
name, command used and a small synthetic reproduction. Remove credentials and
private file contents. Live language-server and AI compatibility is not
exhaustively tested, so include the server/provider and version when relevant.
