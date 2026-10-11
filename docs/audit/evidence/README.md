# Retained execution evidence

These logs were captured from the shared working candidate before its focused commits. The tracker identifies the resulting implementation commit; exact committed archive source/binary/digest acceptance is recorded separately in DELIVERY.md and dist acceptance reports. They do not identify the unchanged starting HEAD as the tested implementation.

Native means macOS ARM64 26.6.2 here. Linux VM logs ran under Docker on the ARM Mac. Earlier real-language workflows use actual Pyright/TypeScript stdio and core buffers. The later `real-lsp-cd5/` evidence additionally exercises the extracted macOS ARM64 executable through actual 80×24 keyboard UI with both real servers. The published 0.3.0 logs are decisive red reproductions with synthetic local providers/credentials. Empty actionlint output records a successful exit, not a workflow execution. Optional Linux bind-mount evidence and command are in security-verification.md.

Initial aggregate runs failed on PTY budget overshoot, asynchronous Quick Open fixture readiness and BSD fixture socket flags. Their substantiated causes were fixed and final native/Linux gates passed; the final logs are retained here.

Historical GitHub run `38138400708` at `757d8b0` was cancelled when the owner required local Jenkins: ARM macOS and both Linux native package jobs passed; Intel macOS was cancelled. `github-757-cancelled-run.json` and completed-target receipts preserve that boundary. Further Actions runner allocation for this audit is disabled.

`head-macos-acceptance.log` retains the later local marker-observation failure. Same extracted executable full24, isolated tests and two controlled CPU-load rounds passed, as recorded in the accompanying logs. The original cause remains unexplained; no timing limit or security positive control was weakened.

The Jenkins migration has six passing synthetic preflight boundary cases (exact SHA, branch/wrong SHA, wrong native target, dirty owner work, mismatched toolchain host). These and successful server-side Jenkinsfile validation do not establish a Jenkins product run. `jenkins-switch-*` logs retain the required local gates immediately before the migration commit. The executed Jenkins build and immutable-source artifacts are linked in PR #11.

Final actual-workflow corrections retain decisive RED/GREEN proof. `jenkins-1-source-red.log` and `jenkins-1-result.json` record the failed native Jenkins source run: inherited ignored launcher signals stopped a foreground job from responding to Ctrl+C. `pty-launcher-*-red.log` reproduce ignored/blocked signal profiles; native/GNU GREEN logs verify child defaults and unchanged parent state. The source gate logs after the correction record 531 native / 528 Linux-VM unit passes and all 24 PTY tests, with explicit optional ignores.

`real-lsp-cd5/` retains the actual packaged Python/TypeScript auto-import refusal before correction, both passing corrected workflows, binary/source provenance, safe owned-child probe, decoded screens and raw terminal recordings. `macos-cd5.acceptance.txt` identifies the independently accepted clean-commit archive. These are code checkpoint receipts, not prospective final Jenkins results; the exact final candidate Jenkins build remains separately linked in PR #11.

Repository text log/screen copies trim trailing whitespace for source-control checks. Original byte logs remain in the captured local paths and Jenkins archives; terminal.raw recordings are unchanged.

`jenkins-2-package-red.log` retains the next overall failed Jenkins result: native source/release/crate/install and extracted archive gates passed, but Linux packaging failed at an empty array in Bash 3.2. `linux-package-clone-red.log` reproduces the actual entrypoint failure in a normal clean clone. Native/GNU GREEN argument-capture fixtures cover normal clones and linked worktrees with spaces; they execute no Docker containers and establish no product acceptance. The subsequent Jenkins result, rather than these synthetic fixtures, establishes packaged Linux acceptance.
