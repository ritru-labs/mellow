# Mellow — guide for Claude Code

Mellow is a calm, modeless terminal text editor in Rust (crate and binary: `mellow`), crafted by Ritru Labs. Rust is pinned to 1.90.0 in `rust-toolchain.toml`.

## Commands (run all before every commit)
- Format: `cargo fmt --all -- --check`
- Lint: `cargo clippy --locked --all-targets --all-features -- -D warnings`
- Test: `cargo test --locked --all-targets` (unit tests plus real-PTY smoke tests in `tests/release_native_smoke.rs`)
- Release build: `cargo build --locked --release`
- Install locally: `cargo install --locked --path .` then run `mellow .`

## Layout
- `src/app.rs` — event loop, editor state, every mode and command (large; prefer graft over reading it whole)
- `src/ui.rs` — all rendering
- `src/buffer.rs` — rope text, undo, safe saves (atomic replace; read-only and bind-mount handling)
- `src/lsp.rs` — language servers (writes go through a non-blocking writer thread)
- `src/git.rs` — Git via the system CLI (`git_command()` sets no-optional-locks, no prompts, setsid)
- `src/pty.rs` — integrated terminal; `src/syntax_tree.rs` — Tree-sitter; `src/theme.rs` — six themes, contrast-tested
- `src/settings.rs`, `src/session.rs`, `src/recovery.rs` — config (`~/.config/mellow`), sessions and crash journals (`~/.local/state/mellow`)
- `src/brand.rs` — product name, credit, tagline, `MELLOW_` env prefix. Change branding only here.

## Rules
- Every bug fix gets a regression test that fails without the fix.
- `mellow --version` must print exactly `mellow <version>` (checked by `scripts/native-ci.sh`).
- Never hand-edit `Cargo.lock`. Never change the `MELLOW_SESSION_V3` / `MELLOW_RECOVERY_V1` file markers without a migration.
- Keep the UI calm: the editing canvas dominates; status text is plain words.

## Graft
This repo is wired to Graft (`.mcp.json`, `.claude/`). Use `graft ask`, `graft grep`, `graft callers` before reading big files. Needs `npm install -g @nanonets/graft`; the `graft/` graph is local and git-ignored (`graft build`).

## macOS build fails with "tapi error ... unknown architecture arm64e.x1"
Not a Mellow bug: the macOS SDK is newer than the linker. Check `which cc`, `cc --version`, `xcode-select -p`, `ls /Library/Developer/CommandLineTools/SDKs`. Fix by reinstalling Command Line Tools (`sudo rm -rf /Library/Developer/CommandLineTools && xcode-select --install`), or build against an older SDK with `SDKROOT=<path to older .sdk>`, or force Apple's compiler with `CC=/usr/bin/clang` if Homebrew's is first in PATH. Then `cargo clean` and rebuild.
