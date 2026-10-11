# Audit candidate delivery

Branch: `audit/mellow-20261011`. Starting main:
`9e42e17e8307d0171525bf325efbaf8d85178383`.
Implementation commit: `e324b7f0e64080f8c911ae41230e99ebb40313a9`.
All commit/PR writes use `binnukyadari`; no merge, tag, release, tap or deployment.

The [57-item tracker](TRACKER.md) preserves every finding, capability and idea.
Stage A safety, Stage B/C editing/integrations and small Stage D configuration
review/Terminal Doctor versions are implemented with meaningful regressions.
Later/Defer capabilities and expanded multi-file replacement remain deferred.

## Executed acceptance

| Gate | Result | Scope |
| --- | --- | --- |
| Pinned Rust formatting, check and all-target/all-feature Clippy with warnings denied | Passed | Native macOS ARM64 and Linux ARM64 Docker VM |
| Native source tests | 527 passed, 0 failed, 3 explicit ignores | macOS 26.6.2 ARM64, Rust 1.90.0 |
| Native real-PTY suite | 24 passed, 0 failed/ignored | Cargo executable, 80×24/120×34, synthetic files/providers |
| Linux source tests | 524 passed, 0 failed, 4 explicit ignores | Docker Linux VM ARM64, Rust 1.90.0 |
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

Exact committed archive/crate/package acceptance and PR identities are pending
in this checkpoint; no downloadable candidate is qualified by this source record.

## Release decision and remaining gates

**Not ready for release.** Latest published 0.3.0 remains vulnerable until an
explicitly authorized patched publication updates distribution install paths.
The open version PR #10 is preserved; this branch intentionally does not duplicate
its version bump. Implementation is unreleased even though Cargo still says 0.3.0.

All four native candidate targets must retain exact-source artifact acceptance.
Apple signing identity/notarization and executed provenance publication are absent.
VoiceOver/braille, participant usability, actual terminal/font/system clipboard,
SSH/tmux, packaged interactive real-LSP panels, and live AI provider behavior are
not claimed by source tests, parsed PTYs, mocks or Linux VM results. Linux ACL and
cross-owner preservation and the mounted filesystem’s unsupported xattr branch
remain explicitly unqualified. Safe save does not serialize an uncooperative
writer across its final check/rename window; trusted tools are not OS-sandboxed.

User and release docs now cover restricted trust, reviewed/cancellable AI,
metadata/hard-link save policy, indentation, paste/occurrences, Save All, local
history/trash, snippets/navigation/search, language services, CLI integration,
Safe Configuration Change and Terminal Doctor. No IDE, autonomous agent or
extension-marketplace expansion was undertaken.
