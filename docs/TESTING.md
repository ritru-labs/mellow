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

The October audit uses local Jenkins for CI, with immutable source selection and
actual agent-platform validation. Automatic Actions jobs skip this audit branch;
the optional four-target candidate workflow is manual-only. Do not trigger it
without the owner's explicit request. Keep native-agent, Docker VM and unavailable
platform results distinct in the acceptance record.

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

## PTY feature checks

`scripts/pty-feature-checks.py` drives an installed binary through workflows the
Rust smoke tests do not cover: the terminal panel and file tree, palette
commands, themes, Git changes, project search, format on save and SSH,
including exit after an SSH disconnect. Each check asserts on saved files or on
the rendered screen.

```bash
python3 -m venv .venv && .venv/bin/pip install pyte
MELLOW_BIN=~/.cargo/bin/mellow .venv/bin/python scripts/pty-feature-checks.py
```

Name checks as arguments to run a subset. Checks that need `rustfmt` or
passwordless `ssh localhost` report SKIP without them. A failing check leaves its
last screen in `~/.cache/mellow-feature-checks/<check>/screen.txt`, and the exit
status is 1. These are local evidence, not a CI gate.

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

## Manual Jenkins job

Create a manual Pipeline using the reviewed `Jenkinsfile`, without SCM polling.
`SOURCE_SHA` is mandatory and immutable; use the public Mellow repository or an
absolute agent-local checkout for `SOURCE_REPOSITORY`. The default agent is the
existing `vidhur-mac && darwin-arm64`, with command-scoped Rust 1.90.0. The job
does not change global toolchains, credentials, other jobs or installed Mellow.
When the controller root URL is unset, `JENKINS_BASE_URL` supplies the evidence
URL without changing controller settings. It runs formatting/lint/source tests,
release/crate, isolated installation, and
all PTY tests against the extracted native archive. Optional Linux VM packages
are labeled separately and include deb/rpm install/test/uninstall. Failure logs
and source/archive/binary receipts are always archived.

Only enable optional Intel or native Linux gates when a correctly labeled agent
exists. Host/toolchain/target mismatches fail before product tests. A timeout or
missing agent is an unavailable gate, never a passing target. Check the actual
Jenkins build result and artifact receipts linked in PR #11.
