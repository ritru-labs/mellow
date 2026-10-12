# Audit candidate workflows

These controls arrived in 0.3.1 with the October audit (`audit/mellow-20261011`).
0.3.0 does not include them all. Core editing remains offline,
modeless and usable at 80×24; AI remains optional. The
[57-item tracker](audit/TRACKER.md) records verification and deferred scope.

## Trust and execution

Opening a project starts restricted. Edit, search, save and recover normally.
**Trust workspace execution** in the palette reviews permission for installed
language servers, formatters and named tasks. Enter grants it for this session
and workspace; **Restrict workspace execution** revokes it and cancels running
optional integrations. `--trust` grants it explicitly at launch. Resolved files
outside that root, including symlink aliases, do not inherit execution trust.
Open another project separately to trust that project. Trusted tools are ordinary
unsandboxed local processes and may load configuration/plugins or use the network.
A manually opened shell runs commands you submit yourself.

Configure `.mellow/tasks.json` with command argument arrays:

```json
{"validate": ["python3", "-m", "json.tool"]}
```

**Run named task** displays the exact command and asks for an explicit Enter
on it. It never implicitly saves or deploys. Output is bounded and sanitized;
recognized existing `file:line[:column]` locations are selectable. Escape
cancels the process group. Limits cover process input, exit and output pipes.
Command configuration is bounded to 64 KiB; control characters are refused.

## Everyday editing

**Save All** first lists dirty tabs, then uses normal per-file Save As/conflict
handling. A conflict or cancelled dialog stops the remaining queue; already
saved files stay saved. It is not a filesystem transaction across files.
**Close all** and **Close other tabs** reuse dirty-file confirmation, preserving
cancelled work. **Navigation back/forward** returns to file and cursor locations
from supported jumps. **Compare with saved disk version** shows a bounded,
read-only line comparison; it changes neither version.

**Select next occurrence** / **Select all occurrences** edit matching text
(up to 500 ranges). Bracketed paste and native/internal clipboard paste share
multi-cursor, occurrence and rectangle operations. A multiline rectangle paste
distributes its rows; each operation undoes together. Native clipboard adapters
run in the background with a two-second bound. A changed document, selection
or focus discards late paste; paste again at the intended position. A copy
retains Mellow's internal text even if the terminal/native clipboard is unavailable.
OSC52 delivery depends on terminal/tmux/SSH policy and is reported unconfirmed.

Indentation uses supported `.editorconfig` settings, then sampled file inference,
then `indent_width` (default four). Supported values are space/tab style and
widths 1–8, including parent/root rules and common star/question/brace patterns;
this is not the full EditorConfig specification. Click the status width or use
**Change indentation width** to cycle 2/4/8 for this buffer. **Use tabs or spaces**
is available in the palette. Enter, Tab/outdent, guides and visual tab columns
use the effective width. External formatters retain their own project rules.

**Insert snippet template** offers small local shell, Terraform, YAML and linked
variable templates. Tab/Shift+Tab advances numbered placeholders; editing or
pasting into a linked stop updates its partners. Escape ends the session.
Unsupported nested/variable/choice syntax and ambiguous multiple edit targets
are refused. This is a small local template feature; LSP snippet capability is
not advertised.

Explorer file/folder creation, rename/move and deletion are searchable commands.
Unavailable actions explain prerequisites. Delete moves an item into private
trash; **Restore deleted file / trash** reviews a restore and never overwrites
new work. Cross-filesystem moves fail while retaining the original. Trash is
persistent and has no automatic purge; remove unwanted private entries manually
only after reviewing what you no longer need. Open files must be closed before
rename/delete. **Keyboard bindings / edit keys** lists effective bindings and
opens their actual configuration file for ordinary editing; restart reloads it.

## Checkpoints and recovery

Local history is separate from crash journals. Named eligible files receive
checkpoints before/after saves, accepted AI edits, restores and file operations.
**Local history / restore checkpoint** restores a selected version as an
unsaved, undoable edit; review it before Save. The final row explicitly forgets
all checkpoints for that file, leaving the file and recovery journal intact.
Set `history_enabled = false` to stop future recording. Existing checkpoints
remain until forgotten or pruned.

Private history excludes secret-like paths/content and files over 2 MiB. It
keeps at most 20 checkpoints and 20 MiB per file, 100 MiB globally and 30 days,
pruning when recording. A failed checkpoint never makes core editing unavailable.
Permissions are private; history and recovery are plaintext, not encrypted
vaults or substitutes for backups. Recovery continues to protect eligible
unsaved work even when history is disabled.

## Search and language workflows

Project discovery/search run in cancellable workers. Query generations prevent
late results replacing newer queries; the result summary records scope,
filters, skipped files and truncation. Named unsaved open files participate.
Use `.mellow/search.json` for optional include/exclude globs and explicit bounds:

```json
{
  "include": ["*.rs", "*.yaml"],
  "exclude": ["generated/*"],
  "max_file_bytes": 2097152,
  "max_total_bytes": 67108864,
  "max_results": 2000,
  "max_files": 5000
}
```

Filters match relative paths; `*` matches any substring and `?` one character.
Unknown/malformed configuration fails visibly. A partial scan is not evidence
of absence. Whole-buffer replacement preview remains supported; richer
multi-file replacement preview is explicitly deferred.

Trusted language services stay warm in a bounded pool (up to eight), synchronize
open documents and aggregate scoped Problems. Completion resolves supported
items and applies primary/supporting import edits atomically; stale or malformed
edits apply neither. Provider completion commands are retained but not executed.
Workspace edits are reviewed and coordinate undo/redo while their buffers remain
unchanged. Server capabilities and startup indexing vary. See the
[real Python/TypeScript results](audit/language-verification.md), including the
tested TypeScript installation pair and missing packaged language-UI qualification.

## Safe Configuration Change

On a named JSON/YAML file, edit normally, then select **Safe configuration change**.
The local structural review compares saved disk text with your candidate and
redacts secret-like values. Duplicate/unsupported ambiguous syntax fails visibly.
Built-in validation only checks the supported structure; it cannot certify your
service's semantics. No command, save or deployment runs automatically.

Optionally configure `.mellow/validators.json` with a language argv array:

```json
{"JSON": ["python3", "-m", "json.tool"]}
```

Trust the workspace, review the exact validator command and explicitly press
Enter to run it with the candidate on stdin. A successful result for the same
revision offers a separate explicit Save action. Failure, cancellation, changed
text or unreadable/malformed validator configuration never offers a validated
save. With no external validator configured, the view clearly offers only a
built-in structural-check save. Ordinary conflict handling and eligible pre-save
checkpoints apply. Rollback restores a checkpoint into the buffer, then needs
review and Save. This tool does not deploy or control services.

## Terminal Doctor

**Terminal Doctor / key recorder** reports delivered keyboard protocol, color
tier, paste events, configured key action and clipboard route. Press a troublesome
key to see the decoded event. Paste is counted without recording its contents.
`C` explicitly copies a report; Escape returns. It omits file paths and pasted
text. Crossterm parses raw input; the Doctor records delivered events rather than
claiming a raw-byte capture. SSH/tmux forwarding, actual glyph/font shaping and
screen-reader usability need native evaluation; the Doctor does not infer those
from a local PTY or contact any service.
