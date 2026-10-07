# FAQ

## Does Mellow run on Windows?

Not natively; the built-in terminal is Unix-only for now. Inside WSL, install the
Linux build.

## macOS says it can't check the app for malicious software

You downloaded the archive in a browser. The builds are not yet signed by Apple,
so macOS blocks browser downloads. Install with Homebrew or the
[install script](install.md) instead; neither triggers the warning.

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
place, so a crash mid-save never leaves half a file. If a file changed on disk
since you opened it, Mellow asks before overwriting. Files that are not UTF-8,
or larger than 100 MiB, are refused rather than mangled.

## Does Git in Mellow do anything risky?

It uses your system `git`. Pull is fast-forward only, push never forces, and
Revert never deletes untracked files. Fetch, pull, push and commit run in the
background with a progress line; "Cancel Git operation" in the palette stops
them.

## Does Mellow send my code anywhere?

Only if you set up [AI](ai.md) and ask it something; Mellow says exactly what it
will send first. Everything else works offline.

## Is it free?

Yes. Mellow is open source under the Apache-2.0 license.

## Where do I report a bug?

On [GitHub issues](https://github.com/ritru-labs/mellow/issues). For security
problems, follow the
[security policy](https://github.com/ritru-labs/mellow/blob/main/SECURITY.md).
