# Development and documentation

## Build the editor

The repository pins Rust **1.90.0**, including rustfmt and Clippy. You need a
Rust toolchain and your platform's C compiler/linker (Xcode Command Line Tools
on macOS, or the equivalent build tools on Linux). Cargo downloads dependencies
from its configured registry on the first build.

```bash
git clone https://github.com/ritru-labs/mellow.git
cd mellow
cargo build --locked
cargo run --locked -- --help
cargo run --locked -- notes.txt
```

The last command is interactive and opens a file in your terminal. The compiled
debug binary is `target/debug/mellow`. To install your checkout on your PATH,
use `cargo install --locked --path .`.

## Verify a change

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
```

The tests include native PTY smoke checks for editing, saving and recovery.
See [Testing](TESTING.md), [Visual QA](VISUAL_QA.md) and
[Contributing](https://github.com/ritru-labs/mellow/blob/main/CONTRIBUTING.md).
`make verify` runs the repository checks in Docker when Docker is available.

## Preview this documentation

Use Python 3.12, matching documentation CI. From the repository root:

```bash
python3.12 -m venv /tmp/mellow-docs-venv
/tmp/mellow-docs-venv/bin/python -m pip install 'mkdocs==1.6.1' 'mkdocs-material==9.7.7'
/tmp/mellow-docs-venv/bin/mkdocs build --strict
/tmp/mellow-docs-venv/bin/mkdocs serve --dev-addr 127.0.0.1:8008
```

Open `http://127.0.0.1:8008/mellow/` and check both a desktop and a narrow mobile
viewport. Stop the preview with `Ctrl+C`. Generated output is in `site/` and is
ignored by Git. If port 8008 is busy, choose another unused port.

Keep existing page filenames stable to preserve published URLs. Add pages to
`mkdocs.yml`, use relative Markdown links and verify the strict build. Base
feature claims on source and tests; label design intentions and unverified
integrations explicitly.

## How publication works

[`.github/workflows/docs.yml`](https://github.com/ritru-labs/mellow/blob/main/.github/workflows/docs.yml)
installs the same pinned MkDocs versions and runs `mkdocs build --strict`.
Pull requests changing docs or `mkdocs.yml` build without deploying. A matching
push to `main` builds and deploys to GitHub Pages. Manual workflow dispatch can
also deploy, so it is not a preview mechanism.

A local commit or local preview does not publish. Have the documentation change
reviewed before merging it into `main`; that merge triggers publication under
the current workflow. Editor releases are separate: `release.yml` runs for
version tags. See the [release checklist](RELEASE_CHECKLIST.md).
