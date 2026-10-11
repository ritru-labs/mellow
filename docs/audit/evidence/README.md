# Retained execution evidence

These logs were captured from the shared working candidate before its focused commits. The tracker identifies the resulting implementation commit; exact committed archive source/binary/digest acceptance is recorded separately in DELIVERY.md and dist acceptance reports. They do not identify the unchanged starting HEAD as the tested implementation.

Native means macOS ARM64 26.6.2 here. Linux VM logs ran under Docker on the ARM Mac. Real-language workflows use actual Pyright/TypeScript stdio and core buffers, not packaged interactive language panels. The published 0.3.0 logs are decisive red reproductions with synthetic local providers/credentials. Empty actionlint output records a successful exit, not a workflow execution. Optional Linux bind-mount evidence and command are in security-verification.md.

Initial aggregate runs failed on PTY budget overshoot, asynchronous Quick Open fixture readiness and BSD fixture socket flags. Their substantiated causes were fixed and final native/Linux gates passed; the final logs are retained here.

Historical GitHub run `38138400708` at `757d8b0` was cancelled when the owner required local Jenkins: ARM macOS and both Linux native package jobs passed; Intel macOS was cancelled. `github-757-cancelled-run.json` and completed-target receipts preserve that boundary. Further Actions runner allocation for this audit is disabled.

`head-macos-acceptance.log` retains the later local marker-observation failure. Same extracted executable full24, isolated tests and two controlled CPU-load rounds passed, as recorded in the accompanying logs. The original cause remains unexplained; no timing limit or security positive control was weakened.

The Jenkins migration has six passing synthetic preflight boundary cases (exact SHA, branch/wrong SHA, wrong native target, dirty owner work, mismatched toolchain host). These and successful server-side Jenkinsfile validation do not establish a Jenkins product run. `jenkins-switch-*` logs retain the required local gates immediately before the migration commit. The executed Jenkins build and immutable-source artifacts are linked in PR #11.
