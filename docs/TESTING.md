# Testing and evidence

Run the native gates before each commit with Rust 1.90.0:

```bash
bash scripts/native-ci.sh verify
bash scripts/native-ci.sh release
bash scripts/native-ci.sh crate
```

`verify` checks release documentation labels, formatting, all-target compilation,
Clippy and all-target tests. `make verify` remains the Docker alternative; Linux
container results describe Linux, even when Docker runs on a Mac.

## Real PTY regressions

`tests/release_native_smoke.rs` starts the executable under a real Unix PTY at
80×24 and other fixture sizes. It exercises editing, selection, Unicode paste,
save, dirty-file protection, crash recovery, terminal restoration, compact
layout and mouse/keyboard interactions. Its AI security fixtures use loopback
mock providers and dummy credentials: a hostile shell response cannot create a
marker, and a Claude redirect cannot contact the second endpoint with its key.
These are transport and editor regressions, not live-provider qualification.

```bash
cargo test --locked --test release_native_smoke
MELLOW_SMOKE_BINARY=/absolute/path/to/extracted/mellow \
  cargo test --locked --test release_native_smoke -- --nocapture
```

The second command tests exactly the selected executable. Source unit tests do
not establish archive acceptance. `native-ci.sh artifact-install` extracts a
version/target-matching release archive, verifies the executable version, runs
all PTY regressions and writes archive/binary SHA-256, source commit, host and
outcome to an adjacent `.acceptance.txt` file. A failed or unexecuted test fails
the gate. Native macOS Intel, macOS ARM64, Linux x64 and Linux ARM64 are separate
targets; emulated runs do not satisfy a native artifact gate.

## Manual terminal and integration evaluation

Open `fixtures/unicode.txt` and verify Telugu, CJK, emoji, combining marks and
tabs with actual fonts. Exercise Terminal.app, iTerm2, Ghostty and Linux/WSL
terminals; SSH with and without tmux; legacy and enhanced keys; 16/256/true
color; bracketed paste on/off; mouse and keyboard-only navigation. Record
terminal version, OS/architecture, font, transport and exact executable digest.
PTY text parsing does not establish native glyph shaping or screen-reader
usability.

Live Python/TypeScript language-server completion/imports, mixed-language
navigation/diagnostics and live AI provider/account compatibility require their
own named integration results. Mocks establish only their protocol fixtures.
VoiceOver or other screen-reader acceptance requires a real assistive-technology
session and user evaluation; keyboard/color tests alone are insufficient.

## Evidence status

The automated gates are implemented; their presence is not a passing run.
Release evidence must link the actual logs and archive digests for each target.
Native Intel/Linux jobs, real SSH/tmux, live provider accounts, native font
rendering and screen-reader/user evaluations remain unverified until recorded
for the candidate. See the implementation tracker for current local results.
