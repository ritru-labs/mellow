# Audit candidate delivery

Branch: `audit/mellow-20261011`. Starting main:
`9e42e17e8307d0171525bf325efbaf8d85178383`.
Implementation commit: `e324b7f0e64080f8c911ae41230e99ebb40313a9`.
PR: [#11](https://github.com/ritru-labs/mellow/pull/11), authored by `binnukyadari`.
Packaging commit: `ff6eb86`; static Linux/provenance fix: `917238f`.
All commit/PR writes use `binnukyadari`; no merge, tag, release, tap or deployment.

The [57-item tracker](TRACKER.md) preserves every finding, capability and idea.
Stage A safety, Stage B/C editing/integrations and small Stage D configuration
review/Terminal Doctor versions are implemented with meaningful regressions.
Later/Defer capabilities and expanded multi-file replacement remain deferred.

## Executed acceptance

| Gate | Result | Scope |
| --- | --- | --- |
| Pinned Rust formatting, check and all-target/all-feature Clippy with warnings denied | Passed | Native macOS ARM64 and Linux ARM64 Docker VM |
| Native source tests | 528 passed, 0 failed, 3 explicit ignores | macOS 26.6.2 ARM64, Rust 1.90.0 |
| Native real-PTY suite | 24 passed, 0 failed/ignored | Cargo executable, 80×24/120×34, synthetic files/providers |
| Linux source tests | 525 passed, 0 failed, 4 explicit ignores | Docker Linux VM ARM64, Rust 1.90.0 |
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
fix; shell safety assertions remain strict. The latest-head rerun and updated
exact-head receipts must be checked in PR #11 before claiming all four targets.
Static workflow validation never substitutes for those actual results.


## Release decision and remaining gates

**Not ready for release.** Latest published 0.3.0 remains vulnerable until an
explicitly authorized patched publication updates distribution install paths.
The open version PR #10 is preserved; this branch intentionally does not duplicate
its version bump. Implementation is unreleased even though Cargo still says 0.3.0.

All four native candidate targets must retain exact-source artifact acceptance.
Apple signing identity/notarization and executed provenance publication are absent.
VoiceOver/braille, participant usability, actual terminal/font/system clipboard,
SSH/tmux, packaged interactive real-LSP panels, and live AI provider behavior are
not claimed by source tests, parsed PTYs, mocks or Linux VM results. Linux VM POSIX ACL and cross-owner refusal were exercised; the mounted
filesystem’s unsupported xattr branch remains unqualified. Safe save does not serialize an uncooperative
writer across its final check/rename window; trusted tools are not OS-sandboxed.

User and release docs now cover restricted trust, reviewed/cancellable AI,
metadata/hard-link save policy, indentation, paste/occurrences, Save All, local
history/trash, snippets/navigation/search, language services, CLI integration,
Safe Configuration Change and Terminal Doctor. No IDE, autonomous agent or
extension-marketplace expansion was undertaken.
