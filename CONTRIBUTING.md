# Contributing to Mellow

Thanks for helping build Mellow.

## Before coding

Mellow has one product rule above all others: **visual polish and ease of use are functional requirements, not finishing work**.

Changes should preserve these constraints:

- usable at 80×24
- no required Nerd Font
- no required AI/cloud connection
- no modal-editing knowledge required
- real SSH/terminal behavior beats browser-only convenience
- Unicode and terminal-cell geometry must be treated deliberately

## Development loop

Docker is the canonical development environment:

```bash
make verify
make run FILE=fixtures/demo.rs
make run FILE=fixtures/unicode.txt
```

For any UI change, manually inspect at least:

- 80×24
- 120×34
- 160×45
- TrueColor
- 256-color fallback when practical

See `docs/VISUAL_QA.md`.

## Commit discipline

Prefer small commits that do one thing. Run `make verify` before each push. Do not combine a large visual redesign with unrelated editor-engine changes unless the dependency is unavoidable.

## Pull requests

A good PR explains:

1. what user problem is solved;
2. what terminal/Unicode edge cases were considered;
3. how the change was tested;
4. screenshots or terminal captures for visual changes when useful.
