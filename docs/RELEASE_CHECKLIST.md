# Release checklist

A release candidate must preserve unsaved work, optional integrations and a
usable terminal at 80×24. A merged implementation or source test alone does not
qualify the downloadable files.

## Candidate gates

```bash
python3 scripts/version-docs.py --write  # after changing Cargo.toml version
bash scripts/native-ci.sh verify
bash scripts/native-ci.sh release
bash scripts/native-ci.sh crate
bash scripts/native-ci.sh install
```

Keep Cargo.toml, CHANGELOG.md, binary output and generated README/docs version
labels coherent. `version-docs.py --check` fails native verification when a
label drifts. Update user docs, integration limitations and this checklist with
the behavior being released.

## Jenkins acceptance and optional candidate workflow

This audit uses the owner's local Jenkins as its CI acceptance path. The manual
`Jenkinsfile` requires an immutable source SHA, checks the actual agent platform,
and archives logs and exact-artifact receipts, including failures. The audit
branch skips automatic GitHub Actions runners. Missing native agents remain
explicit gates; a Linux Docker VM on a Mac cannot qualify Intel macOS or a
native Linux host.

`.github/workflows/candidate-artifacts.yml` is manual-only and requires a separate
explicit request before use. It checks out the requested source and has read-only
repository permissions. Its four
native targets run the shared gates, build archives/packages and run every PTY
regression against extracted/installed executables. Reports bind the exact
source commit, archive/binary/package digests and platform. Failed logs are
uploaded separately from qualified packages; it performs no release, tag, tap
or signing-secret write. Workflow presence or an unexecuted runner is not a pass.

Local packagers require a clean checkout. Linux packaging supports linked
worktrees by exposing their common Git metadata read-only inside the disposable
container. An ARM64 Linux VM on a Mac is identified `linux-vm`; it is not native
macOS or a physical Linux host. Cross-architecture emulation does not satisfy
native target acceptance.

## Tagged workflow and exact artifacts

`.github/workflows/release.yml` runs on `v*` tags. The tag version must match
Cargo.toml and a changelog section. Its actual jobs are:

1. `check`: Linux/macOS formatting, compilation, Clippy, source tests and crate
   packaging; Linux shellcheck.
2. `macos`: native ARM64 (`macos-15`) and native Intel (`macos-15-intel`) build
   separate archives and run every PTY test against the extracted executable.
3. `linux`: native x64 and ARM64 package jobs run the extracted musl executable
   through the PTY suite, then install/test/uninstall `.deb` and `.rpm` inside
   a disposable native Linux container.
4. `release`: depends on both target families, computes SHA-256 checksums,
   attests the exact accepted assets, checks the installer and creates a GitHub
   Release with archives, packages, installer, checksums and acceptance reports.
5. `homebrew`: follows publication and updates the tap formula with checksums.

A `vX.Y.Z-dryrunN` tag creates a **draft** release and updates the tap's dry-run
branch; it is still an external write. Publication is part of the tagged
workflow, not a separate automatic promotion after packaging. Do not push a
release tag or publish without the maintainer's explicit authorization.

Every acceptance report records the source commit, platform/toolchain, archive
and executable digests and the executable used by the PTY tests. A cross-build
or `--version` check cannot substitute for that native artifact gate. Preserve
failed reports; upload artifacts only after required tests pass.

## Signing and remaining release gates

GitHub build-provenance attestations bind accepted asset digests to the workflow.
Verify downloaded assets with `gh attestation verify --repo ritru-labs/mellow`.
They are separate from Apple code signing and notarization.

The macOS packager can code-sign and strictly verify with
`MELLOW_MACOS_SIGN_IDENTITY`; `MELLOW_REQUIRE_SIGNING=1` refuses unsigned
packaging. The workflow supports a temporary signing keychain configured via
`MACOS_SIGNING_P12_BASE64`, `MACOS_SIGNING_P12_PASSWORD` and
`MACOS_SIGNING_IDENTITY` repository secrets. No signing credentials are included
in the repository. Missing credentials produce unsigned archives; do not label
them signed. Apple notarization is not implemented or verified, and remains a
release gate for any notarized-distribution claim.

Record native signing/notarization results, installer/uninstaller behavior,
real terminal/SSH/tmux observations, supported live language servers and any
known exclusions. Intel/ARM and Linux/macOS evidence remain distinct. Native
Windows is unsupported; WSL uses Linux. Screen-reader usability and actual
Unicode font shaping require native evaluation and must not be inferred from
PTY captures.

## Manual workflows

On every claimed target: open/edit/undo/redo/save/close/recover; dirty Save All
and disk conflicts; multi-file edits and coordinated undo; keyboard/mouse and
clipboard/bracketed paste; responsive search/format cancellation; AI disclosure,
read-only shell review/copy, rejection/acceptance/dismissal; completion imports,
navigation and workspace diagnostics. Verify safe-save metadata/link behavior
on the claimed filesystem. Record the candidate executable SHA-256 and exact
missing gate rather than marking unsupported environments passed.
