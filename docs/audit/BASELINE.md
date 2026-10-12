# Baseline and interrupted-session preservation

Audit: `Mellow_Deep_Product_Audit_2026-10-11.html`, read in full from the user's
Downloads folder. SHA-256:
`edb3685901c658107be4e35b7a0146cf6664ef831d9da4b86db5ea94b1269c13`.
It contains 12 findings, 39 matrix rows, six ideas, roadmap and verification
limits; the tracker retains all 57 IDs and their original priority/acceptance.

Exact starting main: `9e42e17e8307d0171525bf325efbaf8d85178383`.
Remote main was fetched and remained at that commit on 2026-10-11. Published
release was v0.3.0; open PR10 (`release/0.3.1`, head `6c5ebe4d`) changes release
version/changelog, not these implementation areas. Source already retains PR9's
redirect/debug credential fixes. The audit feature branch does not duplicate or
revert that work and does not change Cargo's version underneath PR10.

Original checkout: `/Users/rukmini/Downloads/mellow`, local main `a18dd52`,
54 commits behind the audited remote main, with owner `.claude` helper changes.
That checkout and its edits were retained. The isolated linked worktree is
`/Users/rukmini/mellow-audit-20261011`, branch `audit/mellow-20261011`.
Interrupted uncommitted implementation in this worktree was inspected and
retained, then fixed and expanded; source presence was not counted as acceptance.
Repository instructions, product/design contracts, README, architecture,
contributing, development, testing and release documentation were read before
implementation. Graft supplied focused call/source maps of large modules.

The first native working-candidate unit run produced 450 passes, nine failures
and two optional ignored real-server tests. Those failures were preserved in
`/private/tmp/mellow-audit-baseline-tests.log`; later passing runs use the pinned
Rust1.90 toolchain and do not relabel this interrupted candidate as pristine main.
Independent security/save, terminal/regression and language/search reviewers
reproduced and resolved additional faults during implementation.

Published ARM64 macOS archive downloaded for executable RED reproduction:
`mellow-0.3.0-aarch64-apple-darwin.tar.gz`. SHA-256:
`10c386ddaa9ac53c6cfd261d98edcf57fae081bd925c7e663d51613074490fd0`,
matching the GitHub release asset digest. Decisive real-PTY regressions reproduced
F01 (synthetic marker created before user submission) and F02 (redirect target
received the synthetic key). This demonstrates the released defects separately
from the retained source protections and candidate's passing regressions.

GitHub authentication and commit identity were inspected. All branch/PR writes
must use `binnukyadari`; `MaripeddiSupraj` remains unused. No main merge, release
tag, publication, deployment, tap update or credential modification is authorized.
The delivery record supplies actual focused commits, PR and final verification.
