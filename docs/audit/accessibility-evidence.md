# Accessibility evaluation evidence and native gates

Evaluation host observed on 2026-10-11: `sw_vers` reports macOS 26.6.2,
build 25G83; `uname -m` reports arm64. `pgrep -x VoiceOver` returned no running
VoiceOver process. No assistive-technology user session was performed, and no
screen-reader usability result is claimed.

Actual automated evaluation uses a real Unix PTY and the rendered text at 80×24:
keyboard-only open/palette/close, editor focus, save/dirty confirmation, recovery,
legacy-key fallbacks and Doctor decoded events; mouse selection has separate
PTY regressions. The native regression suite also evaluates Unicode buffer
operations and bounded output. UI render tests check compact layouts, ASCII
fallback and theme/status semantics. Exact logs/counts/executable selection
are in [terminal verification](terminal-verification.md) and the delivery record.

This is evidence of keyboard operation and terminal text layout, not VoiceOver
speech, focus announcement, Braille, native shaping, contrast measurement with
actual terminal palettes, or successful use by a disabled participant. Mellow
renders through the terminal emulator; emulator assistive-technology behavior
is part of the complete workflow and cannot be substituted by vt100 screen text.

Remaining native evaluation: launch the exact extracted candidate in Terminal.app
and the user's normal terminal; record terminal/font/version and binary SHA-256;
use VoiceOver to read file/cursor position, selection, palette, dirty/conflict
prompts, diagnostics, AI disclosure/review and recovery; observe focus and speech
while typing/cancelling background operations. Repeat over actual SSH/tmux where
support is claimed. Include a participant evaluation of independent task success
and record failures, rather than declaring source presence accessible.

The environment enabled the keyboard/PTY portion. No native screen-reader,
font/terminal application or participant evaluation is recorded; M35 remains
blocked on that gate. The research outcome is this evidence record and a concrete
native protocol, with implementation deferred until substantiated native issues
are observed.
