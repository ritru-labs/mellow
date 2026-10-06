# Docker-first testing contract

## Canonical local loop

Every change should pass locally before push:

```bash
make verify
```

which runs formatting, compile checks, Clippy and tests inside the same Rust Docker image.

## Why Docker

- deterministic Rust/toolchain version
- no developer-machine dependency drift
- fast incremental rebuilds through named Cargo/target volumes
- Linux behavior is exercised even when development happens on macOS
- easy CI parity

## Manual TTY smoke test

```bash
make run FILE=fixtures/demo.rs
```

Verify:

1. alternate screen opens cleanly
2. typing edits text without a mode switch
3. arrows/Home/End/PageUp/PageDown work
4. line numbers and cursor stay aligned
5. `Ctrl+S` persists changes
6. `Ctrl+Q` protects dirty content
7. terminal is restored after exit
8. resize the terminal through compact/comfortable/expanded sizes

## Unicode smoke test

```bash
make run FILE=fixtures/unicode.txt
```

Explicitly verify Telugu, Japanese, emoji, combining marks, wide characters and tabs. A terminal editor that counts JavaScript/Rust code units as screen cells is not production-correct.

## Later PTY integration suite

We will add PTY-driven tests for:

- resize events
- raw mode restoration
- mouse sequences
- bracketed paste
- clipboard adapters (OSC 52 where available)
- terminal capability fallbacks
- key conflicts across common terminal emulators

## CI / non-interactive behavior

The verification script uses `docker compose run -T` so formatting, checks and tests do not require a pseudo-TTY. Interactive editor runs intentionally keep a TTY.
