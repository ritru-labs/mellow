# Visual QA contract

Visual experience is a release gate for Mellow.

## Required terminal sizes

### 80×24 — compact

- editing canvas remains dominant
- save/commands/quit affordances remain visible
- overlays fit without clipping
- file path may truncate before core actions disappear

### 120×34 — comfortable

- full primary shortcut strip
- command palette centered with calm whitespace
- status text readable without crowding

### 160×45 — expanded

- do not fill spare space with permanent panels
- preserve the same calm hierarchy rather than becoming an IDE dashboard

## Interaction smoke test

1. open `fixtures/demo.rs`
2. click three different cursor positions
3. type ASCII and Unicode
4. paste multiple lines
5. undo and redo
6. open Ctrl+P and filter a command
7. open F1 help and dismiss it
8. edit, press Ctrl+Q, test Cancel and Save & Quit
9. resize while editing and while an overlay is open
10. exit and confirm the shell is restored correctly

## Unicode smoke test

Use `fixtures/unicode.txt` and inspect:

- Telugu
- Japanese
- emoji
- combining marks
- wide CJK characters
- tabs

A visually attractive editor that misplaces the cursor on real Unicode text is not visually correct.
