# Audit candidate delivery

Branch: `audit/mellow-20261011`. Starting main:
`9e42e17e8307d0171525bf325efbaf8d85178383`.
Implementation commit: `e324b7f0e64080f8c911ae41230e99ebb40313a9`.
PR: [#11](https://github.com/ritru-labs/mellow/pull/11), authored by `binnukyadari`.
Packaging commit: `ff6eb86`; static Linux/provenance fix: `917238f`.
Jenkins migration: `53b6736`; final workflow corrections: `cd5e747`.
Jenkins job: [mellow-audit-acceptance](http://127.0.0.1:8082/job/mellow-audit-acceptance/).
The build receipt identifies its exact accepted source SHA; PR #11 links the
executed result. A configured job or validated Jenkinsfile alone is not a pass.
All commit/PR writes use `binnukyadari`; no merge, tag, release, tap or deployment.

The [57-item tracker](TRACKER.md) preserves every finding, capability and idea.
Stage A safety, Stage B/C editing/integrations and small Stage D configuration
review/Terminal Doctor versions are implemented with meaningful regressions.
Later/Defer capabilities and expanded multi-file replacement remain deferred.

## Executed acceptance

| Gate | Result | Scope |
| --- | --- | --- |
| Pinned Rust formatting, check and all-target/all-feature Clippy with warnings denied | Passed | Native macOS ARM64 and Linux ARM64 Docker VM |
| Native source tests | 531 passed, 0 failed, 3 explicit ignores | macOS 26.6.2 ARM64, Rust 1.90.0 |
| Native real-PTY suite | 24 passed, 0 failed/ignored | Cargo executable, 80×24/120×34, synthetic files/providers |
| Linux source tests | 528 passed, 0 failed, 4 explicit ignores | Docker Linux VM ARM64, Rust 1.90.0 |
| Linux real-PTY suite | 24 passed, 0 failed/ignored | Linux VM Cargo executable, separate from macOS |
| Explicit real-language gates | 3 passed, 0 failed/ignored | Pyright 1.1.414 and TS language server 6.0.2/TypeScript 5.9.3 over stdio/core buffers |
| Explicit actual Linux bind mount | 1 passed, 0 failed/ignored | Observed EBUSY→in-place save; mounted VirtioFS xattrs unsupported |
| Native release build and isolated Cargo install | Passed; installed executable passed all 24 PTY tests | Existing user installation preserved |
| Strict MkDocs build, generated version check | Passed | MkDocs 1.6.1/material 9.7.7, Python 3.12 |
| Both workflows actionlint and inline Bash syntax | Passed | Static workflow validation; no runner-execution claim |

[Retained raw logs](evidence/README.md) identify source/protocol tests separately
from packaged executables. The official published ARM64 archive reproduced F01
(marker created before submission) and F02 (dummy key forwarded on redirect).
The candidate’s corresponding native PTY tests pass. See the focused
[security](security-verification.md), [language](language-verification.md),
[terminal](terminal-verification.md) and [accessibility](accessibility-evidence.md)
records for exact regression names, adverse cases and limits.

## Exact packaged checkpoint

Clean source `917238f93ec19a7036bfd7fba777a514c8032bbf` produced accepted
local macOS ARM64 and Linux ARM64 VM packages. Each archive and each installed
package passed **all 24 PTY tests, zero failures/ignores**; deb/rpm uninstall
passed. Receipts record actual executables and separate binary hashes (RPM
stripping produces a different binary hash). Cargo crate verification passed.

| Package | SHA-256 | Execution |
| --- | --- | --- |
| macOS ARM64 tar.gz | `57caf30bd377804036831d704efe7dd4b9aa1a34f9146f228d5e587064b075bf` | Native macOS ARM64 |
| Linux ARM64 musl tar.gz | `5370f422891e098ccdea160a79493389e4247a3e24ac03fc6c87ff20fe32efa0` | Linux Docker VM |
| ARM64 deb | `dba4e4739667ddfd4709af2e347fcac53ad35c508bdbb9bccd44094ba40686d8` | Linux Docker VM, install/PTY/uninstall |
| ARM64 rpm | `567d3e74c60b63e2da2424439080e5d714f607d720728cf8673cf35077c19709` | Linux Docker VM, install/PTY/uninstall |

The extracted Linux musl binary also preserved a real named-user POSIX ACL,
owner/group/mode across atomic inode replacement at 80×24. A child UID65534
refused a root-owned writable file rather than lose ownership; original bytes,
identity and metadata remained intact. The explicit refusal reason was observed
at 240×24 because the 80-column status abbreviated it. The retained probe/log
includes initial harness corrections and the final passing execution. Actual
bind-mount save passed separately at the same clean SHA. Mounted VirtioFS xattrs
are unsupported; ordinary Linux filesystem xattrs passed. These are Linux-kernel
VM results, not macOS or physical-Linux results.

The initial [candidate run](https://github.com/ritru-labs/mellow/actions/runs/38137653600)
ran real native jobs: Linux ARM and Linux x64 passed; macOS ARM/Intel failed the same premature
onboarding observer. Source tests passed before that PTY failure. The test helper
now waits for a complete rendered initial frame before deciding whether to close
onboarding; local full24 passed after correction. This is a test synchronization
fix; shell safety assertions remain strict. The rerun at `757d8b0` passed native macOS ARM64 and Linux ARM64/x64 packages.
The owner then required local Jenkins instead of paid GitHub Actions; the run was
cancelled and Intel macOS did not finish. Retained historical receipts identify
only the three completed targets. Further audit pushes skip Actions runners, and
the candidate workflow is manual-only. Jenkins is the acceptance path for this
audit; no new Actions run is authorized. Static workflow validation never
substitutes for executed results.


A later local extracted macOS archive run at `757d8b0` failed two three-second
subprocess-marker observations (formatter start and the trusted-inside positive
control). Its log lacked rendered failure state. The unchanged executable then
passed all 24 tests, both affected tests individually, and both tests twice under
eight bounded CPU workers. This does not establish the original cause. Marker
time limits and security positive controls were retained; failure diagnostics now
capture marker name and screen. Both RED and GREEN logs are retained. Further
intermittency investigation remains a release-quality limitation.

## Release decision and remaining gates

**Not ready for release.** Latest published 0.3.0 remains vulnerable until an
explicitly authorized patched publication updates distribution install paths.
The open version PR #10 is preserved; this branch intentionally does not duplicate
its version bump. Implementation is unreleased even though Cargo still says 0.3.0.

All four native candidate targets must retain exact-source artifact acceptance;
Intel macOS has not completed the corrected packaged gate. The local Jenkins
controller has an online ARM64 macOS agent, with no Intel/native Linux agent.
Docker Linux VM evidence is separately labeled.
Apple signing identity/notarization and executed provenance publication are absent.
VoiceOver/braille, participant usability, actual terminal/font/system clipboard,
SSH/tmux, Intel/Linux packaged real-LSP UI, and live AI provider behavior are
not claimed by source tests, parsed PTYs, mocks or Linux VM results. Linux VM POSIX ACL and cross-owner refusal were exercised; the mounted
filesystem’s unsupported xattr branch remains unqualified. Safe save does not serialize an uncooperative
writer across its final check/rename window; trusted tools are not OS-sandboxed.

User and release docs now cover restricted trust, reviewed/cancellable AI,
metadata/hard-link save policy, indentation, paste/occurrences, Save All, local
history/trash, snippets/navigation/search, language services, CLI integration,
Safe Configuration Change and Terminal Doctor. No IDE, autonomous agent or
extension-marketplace expansion was undertaken.

## Corrections found through actual workflows

Jenkins #1 at `53b6736` failed the foreground Ctrl+C test (527 passed, one
failed, three ignored); this was preserved rather than rerun blindly. Isolated
REDs reproduced ignored INT/QUIT dispositions and a blocked signal mask. PTY
children now reset their interrupt dispositions and mask before exec using
async-signal-safe calls, preserving the parent. Native/GNU focused tests passed
11/11; final source gates passed 531 native and 528 Linux VM tests, plus 24 PTYs
on each. The CI receipt also records actual launcher signal state.

Actual packaged Python/TypeScript auto-imports at `757d8b0` exposed an app-level
validation gap that real-server core-buffer tests had missed: an import insertion
at the primary replacement start was falsely called overlap. The corrected app
guard permits that boundary, preserves refusal of true overlaps and duplicate
point insertions, and checks Unicode, caret placement and one-step undo/redo.
Clean `cd5e747` macOS ARM archive SHA-256
`c7480db9003ba2f470c7158fe529748455440d262103e413ee623279a6540ee0`,
binary `f9d1699a7d75d62a6a444b1bbfdbf4ac5f1b805a4865c3bede73da8ddb765d2f`,
passed all 24 extracted-archive PTY regressions. The final real-server packaged
workflow record and Jenkins result are linked in PR #11.

The corrected extracted macOS ARM64 `cd5e747` package also passed actual 80×24
keyboard workflows with real Python and TypeScript servers: trust, completion with
import, atomic undo/redo, definition and navigation back, Problems diagnostics, save
and clean exit. Both editor children exited with status 0. Retained
[evidence](evidence/real-lsp-cd5/verification.md) identifies the exact executable.
This closes the previously unqualified native ARM packaged real-LSP UI gate;
Intel/Linux real-LSP UI, native accessibility and remote terminal gates remain open.
