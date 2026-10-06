# Release checklist

Mellow release candidates must preserve the editor's core promise: no silent work loss, no fake integrations, and a usable terminal-native experience on every claimed platform.

## Required automated gates

Run locally through the canonical Docker loop:

```bash
make verify
make release
make package
make install-check
```

Every pull request must also pass the native GitHub Actions matrix on Linux and macOS:

- `cargo fmt --all -- --check`
- `cargo check --locked --all-targets`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `cargo test --locked --all-targets`
- `cargo build --locked --release`
- `cargo package --locked`
- source-install smoke test followed by `mellow --version`

## Manual release smoke checks

On each claimed native platform:

1. Open an existing UTF-8 file and an untitled buffer.
2. Edit, undo/redo, save, Save As, close a dirty tab, and exercise recovery.
3. Open Quick Open and verify fuzzy search plus Tab path completion.
4. Verify Tree-sitter/LSP behavior with and without external language servers installed.
5. Exercise a multi-file LSP rename and coordinated Undo/Redo without silently saving buffers.
6. Open the integrated terminal, run an interactive shell, resize it, test Ctrl+C, alternate-screen applications, bracketed paste, and mouse-aware TUI input.
7. Exercise Git status, hunk stage/unstage/revert, commit, branch switching, blame, fetch, fast-forward-only pull, push, and manual conflict resolution.
8. Verify settings/onboarding remain readable at 80×24 and in compact-terminal fallback.
9. Verify AI remains disabled without provider configuration and that provider edits always require review before apply.

## Tagged artifacts

Pushing a tag matching `v*` (or manually running the release workflow) builds release binaries and uploads:

- a platform/architecture-named tarball,
- README + LICENSE,
- a SHA-256 checksum.

Artifact publication is intentionally separate from GitHub Release creation until the project has a signed release/versioning policy.

## Platform claims

Linux and macOS are production targets once their native CI jobs are green for the release commit.

Windows remains experimental until a native ConPTY-backed PTY implementation and Windows CI/release artifacts are verified. Do not advertise Windows production support before that gate exists.
