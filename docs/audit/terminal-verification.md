# Native terminal workflow verification

Branch: `audit/mellow-20261011`. Starting commit:
`9e42e17e8307d0171525bf325efbaf8d85178383`. These results describe the working
candidate, including uncommitted changes; the starting commit alone does not
identify the tested candidate. The implementation tracker records the final
commit and distribution identities.

Executed on native macOS 26.6.2 (25G83), ARM64, using Rust 1.90.0 through a
command-scoped PATH. The harness launches the executable through a real Unix
PTY, negotiates a legacy terminal protocol, parses displayed cells, sends
keyboard/mouse/bracketed-paste events, and checks files and subprocess markers.
It exercises 80×24 and 120×34 layouts. Child environments and home directories
are isolated per process; the harness does not mutate global environment
variables while tests run concurrently.

## Executed candidate checks

```sh
env PATH=/Users/rukmini/.rustup/toolchains/1.90.0-aarch64-apple-darwin/bin:$PATH \
  cargo test --locked --test release_native_smoke -- --nocapture
```

Result: **24 passed, zero failed, zero ignored**, 16.47 seconds. Log:
`/private/tmp/mellow-native-smoke-stabilized24.log`. With no
`MELLOW_SMOKE_BINARY` override, this run uses Cargo's freshly built executable,
not an extracted release archive. The same harness accepts an explicit archive
executable through that override for native artifact acceptance.

| Workflow / audit boundary | Evidence and acceptance exercised |
| --- | --- |
| Core editing and terminal recovery | Open, edit, save, quit and terminal-mode restoration; crash recovery of Unicode content and every unnamed draft. |
| Discoverability and compact layout | First welcome key reaches the editor; palette and open-file views close with Escape; unsaved quit offers discard; mouse selection runs a filtered palette command at 120×34; legacy Ctrl+Shift+O behavior is explicit. |
| F05 / M03: paste consistency | Emoji and combining-character text pasted at two cursors through both bracketed terminal paste and the clipboard command; exact saved bytes, one undo for the whole edit, and redo for all cursors. Alt+Shift mouse rectangle paste distributes distinct rows and one undo restores both rows. |
| M05 / M02: snippets and history | Linked placeholder receives Unicode bracketed paste, Tab advances to the next placeholder, final caret is correct, and exact bytes save. After restart, the pre-save checkpoint restores through an undoable edit without writing disk until requested. |
| F04: responsive formatting | A two-second local formatter runs while the command palette responds in under one second. A concurrent edit makes the formatter result stale; cancellation leaves the file unchanged. |
| F04: inherited subprocess pipes | Direct formatter child exits but a descendant retains pipes. The ten-second total deadline reports failure and completes Save without accepting partial output; the killed descendant cannot create its delayed marker. |
| Clipboard responsiveness | A deliberately slow clipboard adapter runs while the palette responds in under 600 ms. A subsequent user edit invalidates the pending paste; the saved file contains only the user's edit. |
| F01: shell review | A normal marker-writing suggestion remains read-only while the shell already contains a partially typed command. Copy and Enter-to-close never execute it. A hostile control-byte suggestion creates no marker and reports rejection. |
| F02: credential boundary | A local Claude-compatible endpoint redirects to a second local listener. The candidate sends no request to the redirect destination; the dummy API key is never forwarded. |
| Ordinary AI editing | Context disclosure appears before submission. Two local requests contain exactly the selected text, filename and language, excluding an unrelated file. Reviewing and rejecting writes nothing; accepting a multiline Unicode proposal requires an explicit decision. One undo restores all original bytes, and redo restores all proposed bytes. The dummy credential never appears in terminal output. |
| AI cancellation | An actual SSE preview arrives before Escape. Cancellation closes the provider socket; the fixture attempts a syntactically complete late proposal. A subsequent Unicode user edit saves and remains unchanged, the review never reopens, and one undo restores the original edit target. |
| M28: workspace trust | Trusting the workspace permits core editing and saving of an explicitly opened outside Python file and an in-workspace symlink to another outside Python file, but launches neither the dummy LSP nor formatter. Opening an internal Python file then launches both tools as positive controls for the fixture and granted trust. |
| M01 / M07: multiple files | Two dirty files are reviewed and saved through Save All. Go To Line records navigation; Back and Forward restore the expected line/column. |
| I01: Safe Configuration Change | JSON review shows a semantic path and redacts a synthetic token; viewing the review does not save. Duplicate keys refuse the review without disk changes. |
| I02: Terminal Doctor | The view records decoded key and bracketed-paste events but omits the synthetic paste contents from the captured terminal output. |
| M27: CLI | Multiple file arguments open two tabs, `--goto file:2:2` reaches the exact caret, `--wait` remains attached until exit, and edits save to the correct file. Piped Unicode stdin becomes an interactive unnamed draft and Save As preserves the exact bytes. |

The pipe workflow found a native Darwin issue during integration: reopening
the `/dev/tty` alias left Crossterm's event-source initialization spinning.
Reopening the concrete device from `ttyname(stdout)` before installing stdin
passes the real-PTY regression. The test verifies interaction and saving after
reading the pipe, rather than only checking that stdin bytes were collected.

## Published 0.3.0 red reproductions

The two security regressions were also run against the verified published
native ARM64 executable at
`/private/tmp/mellow-release-audit-baseline/mellow-0.3.0-aarch64-apple-darwin/mellow`,
selected with `MELLOW_SMOKE_BINARY`. These runs failed on decisive effects,
rather than merely on a missing candidate UI message:

| Regression | Published executable result | Candidate result |
| --- | --- | --- |
| `release_binary_rejects_hostile_ai_shell_before_terminal_input` | RED: synthetic marker exists before a separate user action. Log `/private/tmp/mellow-release-030-shell-decisive-red.log`. | GREEN: no marker; control-byte response rejected. |
| `release_binary_claude_redirect_never_contacts_key_target` | RED: redirect destination received a request with `synthetic API key forwarded=true`. Log `/private/tmp/mellow-release-030-redirect-decisive-red.log`. | GREEN: redirect target receives zero requests. |

Fixtures use local mock HTTP providers, temporary files, synthetic paste text,
and a dummy API key. No live provider or real credential is involved.

## Owned module regressions

Focused native Rust 1.90.0 source-unit checks also passed:

| Suite | Result | Additional adverse cases |
| --- | --- | --- |
| `history::tests` | 8 passed, zero failed | Checkpoints survive reopening; bounds and retention; credential-path aliases and secret assignments are excluded; the macOS system temporary-path alias works but user symlinks are refused; forgetting one file preserves another; unsafe/mismatched checkpoint records prevent deletion. |
| `config_change::tests` | 7 passed, zero failed | Empty map-key path is explicit; nondecimal YAML numeric forms refuse review instead of being misclassified as strings; quoted values remain valid strings; semantic comparison and redaction. |
| `snippets::tests` | 7 passed, zero failed | Linked multiline Unicode paste preserves CRLF; the next placeholder follows changed ranges; an edit boundary inside a combining-character grapheme refuses mutation. |
| `pty::tests` | 10 passed, zero failed | Foreground jobs, Ctrl+C, resize, terminal modes, scrollback, child descriptor isolation; a continuously writing real shell yields after at most 64 KiB per event-loop read. |

Commands used `cargo test --locked --bin mellow <module>::tests -- --nocapture`
with the same pinned-toolchain PATH. These focused runs precede the final
candidate-wide gates recorded elsewhere; they are not a substitute for those
gates.

Final gate investigation found a real terminal-read budget overshoot: when
earlier PTY reads were partial, a final full 4096-byte read could exceed the
remaining 64 KiB budget. The reader now caps that final read to the remaining
budget. The unchanged continuous-output regression passed independently on
native macOS (0.51 seconds) and the Docker Linux VM (0.14 seconds), retaining
both its strict byte budget and one-second input-turn limit. Logs:
`/private/tmp/mellow-native-pty-budget-final.log` and
`/private/tmp/mellow-linux-pty-budget-final.log`. Linux VM evidence is not native
macOS evidence.

The Quick Open trust fixture now waits for the final query to render and retries
Enter only while the picker remains open, bounded to five seconds. This
exercises the existing explicit retry required when Enter arrives during
asynchronous discovery; it does not shorten or bypass the canonical trust
boundary or positive execution controls. Local HTTP fixtures explicitly clear
inherited nonblocking mode on accepted sockets before bounded reads, preventing
a BSD accept/write race from masquerading as a provider failure.

## Limits and remaining native gates

The PTY is real, but the terminal display is parsed in software. This evidence
does not establish Terminal.app, iTerm2 or Ghostty rendering, actual emoji or
Indic glyph shaping, OS accessibility-tree exposure, VoiceOver usability,
physical mouse behavior, or an SSH/tmux session. The Unicode cases establish
editing/recovery byte integrity and grapheme-aware operations, not visual
shaping. No live screen-reader evaluation was performed.

Clipboard fixtures supply local `pbcopy`/`pbpaste` and Wayland-style adapter
programs through the child PATH. They establish app routing, bounded process
behavior and editing semantics; they do not establish the host system
clipboard's reliability. Fixtures are warmed before launch so native execution
validation of newly created scripts is not mistaken for editor latency.

Language-server completion/import/navigation/diagnostics, project-search
workflows, multi-file AI review, coordinated conflict undo, and actual validator
execution have separate source/protocol evidence. This terminal suite does not
claim full PTY coverage of those workflows. AI transport tests here are mocks,
not live-provider qualification. Native macOS Intel, Linux target archives,
package installation/uninstallation, signing/notarization and provenance
require the distribution gates in the release verification record.

## Launcher signal boundary

The first actual Jenkins run failed foreground Ctrl+C. Isolated tests reproduced
ignored INT/QUIT inheritance and a blocked signal mask before the fix. PTY
children now reset these dispositions and clear their mask before exec using
async-signal-safe calls; the editor process is unchanged. The regression verifies
foreground ownership, disables input echo, checks an actual recovery file marker
and split output, verifies shell survival and preserves the parent signal state.
Native macOS and Linux GNU focused suites passed 11/11; the old echoed-input
marker test alone would not establish recovery. RED/GREEN logs are retained.
