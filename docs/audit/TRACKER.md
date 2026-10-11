# Mellow audit implementation tracker

Starting commit: `9e42e17e8307d0171525bf325efbaf8d85178383`. Branch: `audit/mellow-20261011`.
Audit SHA-256: `edb3685901c658107be4e35b7a0146cf6664ef831d9da4b86db5ea94b1269c13`.

All 12 findings, 39 capability rows and six ideas are retained. Status applies to the stated verification scope; remaining gates are explicit. Later/Defer and M15 expansion remain deferred. No merge, release or deployment is authorized.

[Baseline](BASELINE.md) · [Machine-readable tracker](tracker.json) · [Delivery](DELIVERY.md) · [User workflows](../audit-workflows.md) · [Security](security-verification.md) · [Language](language-verification.md) · [Terminal](terminal-verification.md) · [Accessibility](accessibility-evidence.md)

Release readiness: **Not ready for publication: exact latest-head native CI rerun pending; patched publication unauthorized; Apple signing/notarization and native accessibility/terminal-family gates unavailable.**

## Coverage

| Item | Priority | Status |
| --- | --- | --- |
| [F01: AI shell suggestions can execute before the user presses Enter](#f01) | F01 Critical · Reproduced · remains in reviewed main | verified |
| [F02: Released Claude transport forwards the key through redirects](#f02) | F02 High · Reproduced in 0.3.0 · fixed in source, awaiting released verification | blocked |
| [F03: Save replaces file identity without preserving all metadata](#f03) | F03 High · Reproduced xattr loss and hard-link divergence | verified |
| [F04: Formatter execution freezes the editor and can outlive its timeout](#f04) | F04 High · Reproduced · remains in reviewed main | verified |
| [F05: Terminal paste silently ignores additional cursors](#f05) | F05 Medium · Reproduced · remains in reviewed main | verified |
| [F06: New indentation blocks always add four spaces](#f06) | F06 Medium · Reproduced · remains in reviewed main | verified |
| [F07: The command palette does not contain every action](#f07) | F07 Medium · Reproduced discovery gap | verified |
| [F08: Project search and discovery run synchronously while typing](#f08) | F08 Medium · Source-confirmed performance risk · not benchmarked over SSH | verified |
| [F09: Completion drops supporting edits such as imports](#f09) | F09 Medium · Source-confirmed missing LSP handling · live server impact untested | verified |
| [F10: Switching language families replaces the language-server process](#f10) | F10 Medium · Source-confirmed limitation | verified |
| [F11: Version and testing documentation disagree with the release](#f11) | F11 Medium · Source/release evidence | verified |
| [F12: Published archives lack a complete native artifact acceptance gate](#f12) | F12 Medium · Coverage gap · not a demonstrated broken package | implemented but unverified |
| [M01: Save All / close all / close others](#m01) | Next | verified |
| [M02: Persistent local history](#m02) | Next | verified |
| [M03: Multi-cursor + rectangle editing](#m03) | Fix now | verified |
| [M04: Indent width / .editorconfig](#m04) | Next | verified |
| [M05: Snippets with placeholders](#m05) | Next | verified |
| [M06: Code folding](#m06) | Later | deferred |
| [M07: Navigation back / forward](#m07) | Next | verified |
| [M08: Document symbols / outline](#m08) | Next | verified |
| [M09: Workspace symbol search](#m09) | Next | verified |
| [M10: Project Problems](#m10) | Next | verified |
| [M11: Completion imports and resolution](#m11) | Next | verified |
| [M12: Warm language services](#m12) | Next | verified |
| [M13: Search include/exclude filters](#m13) | Next | verified |
| [M14: Asynchronous search and discovery](#m14) | Next | verified |
| [M15: Replacement preview](#m15) | Improve | deferred |
| [M16: Save-versus-disk comparison](#m16) | Next | verified |
| [M17: Two files side-by-side diff](#m17) | Later | deferred |
| [M18: Multi-root workspaces](#m18) | Later | deferred |
| [M19: More than two editor panes](#m19) | Later | deferred |
| [M20: File operation undo / trash](#m20) | Next | verified |
| [M21: Clipboard history](#m21) | Later | deferred |
| [M22: Keyboard binding UI and key recorder](#m22) | Next | verified |
| [M23: Task runner / clickable errors](#m23) | Next | verified |
| [M24: Test discovery / test results](#m24) | Later | deferred |
| [M25: Debugger / DAP](#m25) | Later | deferred |
| [M26: Git stash / worktrees / richer conflict view](#m26) | Later | deferred |
| [M27: CLI multi-file, --wait, --goto, stdin](#m27) | Next | verified |
| [M28: Workspace trust / restricted mode](#m28) | Before richer execution | verified |
| [M29: AI context exclusions / redaction](#m29) | Next | verified |
| [M30: AI usage, cost and request limits](#m30) | Next | verified |
| [M31: AI streaming / cancellable transport](#m31) | Next | verified |
| [M32: Persistent AI change history](#m32) | Later | deferred |
| [M33: AI multi-file proposals](#m33) | Later | deferred |
| [M34: Extension ecosystem](#m34) | Defer | deferred |
| [M35: Accessibility beyond colors/keyboard](#m35) | Research now | blocked |
| [M36: Windows native / remote orchestration](#m36) | Defer | deferred |
| [M37: Encoding conversion / EOL controls](#m37) | Later | deferred |
| [M38: Large file / log viewer mode](#m38) | Later | deferred |
| [M39: Signed releases / repair diagnostics](#m39) | Next | blocked |
| [I01: Safe configuration change](#i01) | Stage D | verified |
| [I02: Terminal Doctor](#i02) | Stage D | blocked |
| [I03: Change timeline and checkpoints](#i03) | Later | deferred |
| [I04: AI request receipt](#i04) | Later | deferred |
| [I05: Environment comparison](#i05) | Later | deferred |
| [I06: Runbook capture](#i06) | Later | deferred |

<a id="f01"></a>
## F01: AI shell suggestions can execute before the user presses Enter

Priority: F01 Critical · Reproduced · remains in reviewed main. Status: **verified**.

Baseline revalidation: The “Ask AI for a shell command” workflow promises to type a suggestion without running it. A lone carriage return embedded inside the returned string survives cleaning. With a visible /bin/sh session, the string is written directly to the PTY and the carriage return submits it..

Current implementation/evidence: Shell suggestions reject C0/DEL/C1 before cleaning and remain in read-only review. Copy is the only transfer action; there is no insertion into an arbitrary PTY..

Required change: Recommended change. Reject terminal control characters, including CR, LF, ESC, NUL and C1 controls, in shell suggestions. Keep the result in a read-only review view until an explicit Insert action. Insert only into a verified idle shell prompt; never into an arbitrary foreground program or partially typed command. Treat bracketed paste as transport, not as an execution authorization boundary..

Dependencies: None.

Acceptance: Control-byte variants cannot execute or alter PTY state; safe suggestions require manual paste and separate shell submission..

Tests: shell_commands_reject_every_terminal_control_before_cleaning; release_binary_rejects_hostile_ai_shell_before_terminal_input; ordinary safe shell review PTY.

Source: `src/ai.rs`; `src/app.rs`; `tests/release_native_smoke.rs`.

Evidence: [security-verification.md](security-verification.md); [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="f02"></a>
## F02: Released Claude transport forwards the key through redirects

Priority: F02 High · Reproduced in 0.3.0 · fixed in source, awaiting released verification. Status: **blocked**.

Baseline revalidation: The downloaded release follows redirects. Its Claude adapter sends x-api-key, which can reach a different destination. Main disables redirects and redacts the configuration Debug representation, but users installing the latest published release still receive the older transport..

Current implementation/evidence: Retains PR #9 no-redirect/redacted Debug protections and applies them to cancellable streaming. Official 0.3.0 RED forwards a dummy key; candidate PTY target receives zero requests..

Required change: Recommended change. Publish and verify a patched release containing PR #9, with an explicit upgrade note. Run the redirect regression against the packaged executable. Keep model/provider tests independent of real credentials..

Dependencies: None.

Acceptance: No redirected credential-bearing request; upgrade distributed install paths only through an explicitly authorized patched release..

Tests: redirects_never_receive_the_claude_key; ai_requests_do_not_follow_redirects_to_another_host; debug_output_never_shows_the_api_key; release_binary_claude_redirect_never_contacts_key_target.

Source: `src/ai.rs`; `src/claude.rs`; `tests/release_native_smoke.rs`.

Evidence: [security-verification.md](security-verification.md); [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9; ff6eb860caaaef74fc34d2a3480690f9ea50c89e · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Publishing/upgrading latest release and distribution install paths requires explicit user authorization; latest 0.3.0 remains vulnerable until publication..

<a id="f03"></a>
## F03: Save replaces file identity without preserving all metadata

Priority: F03 High · Reproduced xattr loss and hard-link divergence. Status: **verified**.

Baseline revalidation: Atomic replacement creates a new inode and copies permission bits. It does not preserve user extended attributes or the original hard-link relationship. Owner/group and ACL preservation are also not implemented explicitly; those effects were not reproduced here. This is already acknowledged broadly in the FAQ, but matters particularly for an SSH/configuration editor..

Current implementation/evidence: Atomic save preserves supported xattrs, native ACL, owner/group/mode or refuses. Hard links are refused with independent Save As. Exclusive Save As, inode-bound conflict baseline and no-follow in-place fallback protect competing paths..

Required change: Recommended change. Define a save policy for hard links, ownership, ACLs and xattrs. Preserve supported metadata, detect unsupported cases and explain the tradeoff before proceeding. Do not silently switch every save to in-place writing: that introduces partial-write risks..

Dependencies: None.

Acceptance: Native metadata fixtures survive atomic save; linked names and concurrent-writer fixtures retain existing work. Actual Linux EBUSY mount uses verified in-place policy..

Tests: atomic_save_preserves_extended_attributes_owner_group_and_mode; atomic_save_preserves_native_acl_and_nondefault_group; saving_a_hard_link_refuses_without_mutating_either_name; save_baseline_cannot_adopt_a_writer_after_the_conflict_check; saving_a_bind_mounted_file_rewrites_it_in_place; Actual extracted Linux musl POSIX ACL preservation and cross-owner refusal PTY (retained probe/log).

Source: `src/buffer.rs`.

Evidence: [security-verification.md](security-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Mounted VirtioFS does not support user xattrs; normal Linux filesystem xattrs verified. Safe save does not serialize uncooperative writers across final conflict-check/rename; exact claimed-platform results remain separate..

<a id="f04"></a>
## F04: Formatter execution freezes the editor and can outlive its timeout

Priority: F04 High · Reproduced · remains in reviewed main. Status: **verified**.

Baseline revalidation: Format-on-save calls a blocking process runner from the UI path. The runner has a 10-second child wait deadline, then joins pipe-reader threads after the direct child exits. A descendant that retains stdout/stderr can keep those joins blocked after the deadline..

Current implementation/evidence: Formatter workers bind identity/revision and cancellation; one bounded deadline covers stdin, process exit and inherited output pipes; process groups are terminated..

Required change: Recommended change. Move formatting off the UI thread with progress and cancellation. Bind the result to the original buffer identity/revision. Apply one deadline to process exit, input and output draining; bound output and terminate the formatter process group when cancelled/timed out..

Dependencies: F03 safe-save contract; M28 execution trust.

Acceptance: Palette stays interactive during formatter execution; stale output never edits; inherited pipes and descendants stop at deadline..

Tests: exited_formatter_with_inherited_output_pipe_obeys_deadline; input_backpressure_obeys_one_deadline; cancellation_kills_the_formatter_and_its_descendant; responsive/stale/cancel and inherited-pipe PTY.

Source: `src/format.rs`; `src/app.rs`.

Evidence: [security-verification.md](security-verification.md); [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="f05"></a>
## F05: Terminal paste silently ignores additional cursors

Priority: F05 Medium · Reproduced · remains in reviewed main. Status: **verified**.

Baseline revalidation: The editor command for Paste supports extra cursors and rectangular selections. The bracketed terminal-paste event uses another path that updates only the ordinary selection or primary cursor. Common terminal paste gestures therefore behave differently from editor Paste..

Current implementation/evidence: Keyboard insertion, clipboard and bracketed paste share selection/multi-cursor/rectangle edit operations with grouped undo and grapheme boundaries. Clipboard workers reject stale selections..

Required change: Recommended change. Route bracketed paste and clipboard Paste through a shared edit operation, retaining the existing selection, multi-cursor and rectangle rules. Specify how multiline paste is distributed..

Dependencies: Shared buffer edit operations.

Acceptance: All transports produce exact expected Unicode bytes at each cursor/row; one undo/redo covers the entire edit..

Tests: Unicode multiple-cursor, clipboard, bracketed-paste and Alt+Shift rectangle PTY; audit_clipboard_late_result_never_edits_a_background_or_changed_selection.

Source: `src/app.rs`; `src/buffer.rs`; `src/snippets.rs`.

Evidence: [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="f06"></a>
## F06: New indentation blocks always add four spaces

Priority: F06 Medium · Reproduced · remains in reviewed main. Status: **verified**.

Baseline revalidation: The buffer detects tabs versus spaces, but the space indentation unit is hard-coded to four. There is no configurable width or .editorconfig integration in the reviewed settings. Preserving a current line’s indentation works; inferring the next block’s width does not..

Current implementation/evidence: Per-buffer width and style use content inference, supported .editorconfig settings and explicit status/palette overrides, shared by Enter, Tab/outdent and LSP formatting options..

Required change: Recommended change. Add per-buffer indentation width, content inference, .editorconfig support and an explicit status-bar override. Use that value consistently in insert, outdent, visual columns and formatter requests..

Dependencies: None.

Acceptance: Two/four/eight-space fixtures and tab files retain conventions; explicit override is visible and persisted where configured. Supported editorconfig subset is documented..

Tests: buffer indentation and editorconfig regressions; settings_round_trip_parser_supports_documented_values.

Source: `src/buffer.rs`; `src/settings.rs`; `src/ui.rs`.

Evidence: [../audit-workflows.md](../audit-workflows.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="f07"></a>
## F07: The command palette does not contain every action

Priority: F07 Medium · Reproduced discovery gap. Status: **verified**.

Baseline revalidation: Explorer file rename, file/folder creation and deletion are handled by tree-specific keys and controls. They are not all registered as searchable commands. The README and help say every command is searchable, which sets a stronger expectation..

Current implementation/evidence: Context-aware palette commands expose folder/file creation, rename and recoverable trash with aliases and actionable disabled explanations..

Required change: Recommended change. Register context-aware explorer actions in the shared command vocabulary. Show why an unavailable action is disabled and offer the next useful step. Include aliases such as folder/directory and rename/move..

Dependencies: None.

Acceptance: Searchable create-folder and rename workflows work without tree-key memorization; unavailable context gives a useful next step..

Tests: command palette registry/context tests; palette/mouse native PTY.

Source: `src/command.rs`; `src/app.rs`.

Evidence: [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: First-time participant/pilot usefulness evaluation unavailable; source and native PTY results cannot establish participant acceptance..

<a id="f08"></a>
## F08: Project search and discovery run synchronously while typing

Priority: F08 Medium · Source-confirmed performance risk · not benchmarked over SSH. Status: **verified**.

Baseline revalidation: Changing the project query calls search_project directly; it reads files on the event path. Initial discovery runs git ls-files with blocking output collection. The file/result limits bound much of the work, but do not make slow storage or process execution non-blocking..

Current implementation/evidence: Debounced generation-tagged workers cancel and bound discovery/search; include/exclude scope and skipped/truncated results are visible; unsaved named buffers overlay disk..

Required change: Recommended change. Use a cancellable worker, a short debounce, query generations and incremental results. Show scope, skipped files and truncation. Add include/exclude filters and configurable limits; merge new unsaved buffers into discovery explicitly..

Dependencies: None.

Acceptance: Slow synthetic workers do not block input; superseded results cannot publish; cancellation, scope and limit reporting are verified..

Tests: f08_worker_only_returns_the_current_generation_and_cancels_without_waiting; f08_discovery_deadline_covers_descendant_inherited_stdout; project_search_configured_scope_and_filtered_counts_are_visible_at_80_by_24; quick_open_pending_discovery_shows_searching_at_80_by_24.

Source: `src/search.rs`; `src/workspace.rs`; `src/app.rs`.

Evidence: [language-verification.md](language-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Slow-storage/SSH performance not measured; local synthetic cancellation/responsiveness evidence only..

<a id="f09"></a>
## F09: Completion drops supporting edits such as imports

Priority: F09 Medium · Source-confirmed missing LSP handling · live server impact untested. Status: **verified**.

Baseline revalidation: Completion parsing retains a label, insertion text and primary range. It does not retain additionalTextEdits or a completion command, and no completion-item resolve path was found. Some servers use those mechanisms to add imports or finish an edit..

Current implementation/evidence: Completion resolve retains opaque provider data; primary/import edits validate and apply atomically. Unsupported completion commands are disclosed and never executed..

Required change: Recommended change. Support the advertised LSP completion contract or clearly constrain capabilities. Apply supporting text edits atomically with stale-document and overlap checks. Add completion-item resolution when requested by the server. Snippet support is a separate feature and currently advertised false..

Dependencies: Shared atomic buffer transactions.

Acceptance: Real Python and TypeScript imports plus symbol insertion undo/redo together; stale, overlapping and malformed edits apply neither..

Tests: f09_completion_import_is_atomic_undoable_and_discloses_unexecuted_command; f09_stale_completion_import_applies_neither_edit; f09_overlapping_completion_import_applies_neither_edit; actual Pyright and TypeScript workflows.

Source: `src/lsp.rs`; `src/app.rs`; `src/buffer.rs`.

Evidence: [language-verification.md](language-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Actual real-server stdio/core workflow verified; packaged interactive completion/import/navigation/Problems selection remains unqualified..

<a id="f10"></a>
## F10: Switching language families replaces the language-server process

Priority: F10 Medium · Source-confirmed limitation. Status: **verified**.

Baseline revalidation: LanguageService owns one optional client. When the next document uses another server, it stops the current one and spawns the next. Same-server file switching is reused, which is good. Mixed Rust/Python/YAML/Terraform projects cannot keep each language service warm..

Current implementation/evidence: Bounded workspace/server pool keeps language processes warm, synchronizes all open documents, aggregates file-named Problems and exposes restart/status. Interaction intents prevent abandoned navigation..

Required change: Recommended change. Pool clients by workspace and server identity. Keep all open documents synchronized and aggregate Problems across files with explicit scope. Bound idle servers and expose restart/status per language..

Dependencies: F09 validated response handling.

Acceptance: Four alternating cycles preserve both server PIDs; inactive unsaved diagnostics/references retain correct file identity; closed/reopened requests do not revive..

Tests: real mixed Pyright/TypeScript PID reuse, inactive unsaved diagnostics/references and workspace symbols; lsp_interaction regressions; switching_symbol_scope_invalidates_the_previous_panel_request; unchanged_inactive_document_sync_is_idempotent.

Source: `src/lsp.rs`; `src/app.rs`.

Evidence: [language-verification.md](language-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Actual real-server stdio/core workflow verified; packaged interactive completion/import/navigation/Problems selection remains unqualified..

<a id="f11"></a>
## F11: Version and testing documentation disagree with the release

Priority: F11 Medium · Source/release evidence. Status: **verified**.

Baseline revalidation: README and the docs landing page still say 0.2.1; Cargo and the published release are 0.3.0. TESTING describes PTY work as future work even though PTY tests exist. The release checklist describes publication separation that no longer matches the workflow..

Current implementation/evidence: Cargo version generates release labels; docs distinguish unsafe published 0.3.0 from unreleased candidate and describe actual native/source/package gates. No competing PR #10 version bump..

Required change: Recommended change. Generate release version labels from one source. Update capability tables and testing/release instructions at the same time as the release. Publish a concise supported-platform and integration validation table..

Dependencies: None.

Acceptance: Cargo, binary and generated release labels agree; actual artifact jobs and validation limits are documented..

Tests: python3 scripts/version-docs.py --check; native-ci verify and package version checks.

Source: `scripts/version-docs.py`; `README.md`; `docs/TESTING.md`; `docs/RELEASE_CHECKLIST.md`.

Evidence: [BASELINE.md](BASELINE.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: ff6eb860caaaef74fc34d2a3480690f9ea50c89e · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="f12"></a>
## F12: Published archives lack a complete native artifact acceptance gate

Priority: F12 Medium · Coverage gap · not a demonstrated broken package. Status: **implemented but unverified**.

Baseline revalidation: CI contains meaningful unit and PTY tests. However, tagged packaging builds separate musl/macOS target binaries and largely checks architecture and --version. The release publish job tests installer version output. The available archive-install PTY gate is not invoked by release.yml; Intel macOS is cross-built without a native Intel test in that workflow..

Current implementation/evidence: Read-only exact-PR-head candidate jobs cover four native targets; each extracted archive runs all real PTY tests, with exact SHA/digests and package install/uninstall gates. Release path adds provenance and optional signing..

Required change: Recommended change. Run the existing PTY smoke suite against each extracted release archive on its native target. Add install/uninstall checks for .deb/.rpm and a documented Intel macOS gate, plus release provenance and signing/notarization work..

Dependencies: F01–F11 regressions; Clean committed source.

Acceptance: Each claimed archive must pass all PTY tests on its native architecture with recorded source/archive/binary identity; no cross-build or unexecuted workflow substitutes for acceptance..

Tests: native-ci artifact-install; .deb/.rpm install/uninstall PTY; actionlint and Bash validation; scripts/test-artifact-provenance.sh: same-version stale-source and incorrect-target archives refused; All 24 PTY tests against extracted native macOS archive, Linux musl archive, installed deb and rpm; uninstall verified.

Source: `.github/workflows/candidate-artifacts.yml`; `.github/workflows/release.yml`; `scripts/native-ci.sh`; `scripts/package-linux.sh`; `scripts/package-macos.sh`.

Evidence: [../RELEASE_CHECKLIST.md](../RELEASE_CHECKLIST.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: ff6eb860caaaef74fc34d2a3480690f9ea50c89e; 917238f93ec19a7036bfd7fba777a514c8032bbf · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Local macOS ARM archive and Linux ARM VM archive/deb/rpm accepted at 917238f. Initial native Linux ARM GitHub candidate passed; initial macOS jobs failed incomplete onboarding observer. Corrected complete-frame observer passes locally; exact latest-head four-target rerun must finish..

<a id="m01"></a>
## M01: Save All / close all / close others

Priority: Next. Status: **verified**.

Baseline revalidation: Missing; baseline src/command.rs.

Current implementation/evidence: Save All reviews dirty tabs and stops safely on conflict; Close All/Others require dirty-work decisions..

Required change: Essential once multiple files and multi-file edits exist; give a dirty-file summary and per-file conflict handling..

Dependencies: Stage A foundation.

Acceptance: Conflicts stop coordinated saving without losing remaining work; cancelled closure retains dirty tabs..

Tests: audit_save_all_reviews_dirty_tabs_stops_at_conflicts_and_keeps_other_work; audit_close_others_cancel_keeps_dirty_tabs_and_close_all_does_not_lose_text; two-file Save All PTY.

Source: `src/app.rs`; `src/command.rs`.

Evidence: [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m02"></a>
## M02: Persistent local history

Priority: Next. Status: **verified**.

Baseline revalidation: Missing; baseline src/recovery.rs.

Current implementation/evidence: Private bounded restart-persistent per-file checkpoints around save, accepted AI and file operations; restore is undoable and never silently saves. Optional disable and per-file forget preserve recovery journals..

Required change: Restore earlier saved text, accepted AI edits and safe file-operation snapshots across restarts. Distinct from crash journals..

Dependencies: Stage A foundation.

Acceptance: Restore pre-save bytes after restart, retain current disk until Save, enforce retention and deny sensitive files/content..

Tests: history::tests (8); checkpoint restart/restore PTY; audit_disabled_history_does_not_create_checkpoints_and_pending_usage_closes_budget.

Source: `src/history.rs`; `src/app.rs`; `src/settings.rs`.

Evidence: [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m03"></a>
## M03: Multi-cursor + rectangle editing

Priority: Fix now. Status: **verified**.

Baseline revalidation: Partial; baseline src/app.rs.

Current implementation/evidence: Select-next/all occurrences plus shared Unicode multi-cursor and rectangle paste..

Required change: Already implemented. Unify paste and selection semantics; add select next occurrence / select all occurrences..

Dependencies: Stage A foundation.

Acceptance: Each occurrence/cursor/rectangle row updates once; one undo restores all..

Tests: audit_occurrence_selection_replaces_unicode_once_and_undo_restores_every_match; F05 native PTY.

Source: `src/app.rs`; `src/buffer.rs`.

Evidence: [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m04"></a>
## M04: Indent width / .editorconfig

Priority: Next. Status: **verified**.

Baseline revalidation: Missing width/policy layer; baseline src/settings.rs.

Current implementation/evidence: Same convention-preserving width/style implementation as F06..

Required change: Respect project formatting conventions and expose effective settings..

Dependencies: Stage A foundation.

Acceptance: One buffer indentation contract governs insertion/outdent/visual columns/formatting..

Tests: F06 buffer/settings/editorconfig regressions.

Source: `src/buffer.rs`; `src/settings.rs`; `src/ui.rs`.

Evidence: [../audit-workflows.md](../audit-workflows.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m05"></a>
## M05: Snippets with placeholders

Priority: Next. Status: **verified**.

Baseline revalidation: Missing; baseline src/lsp.rs.

Current implementation/evidence: Small local snippet vocabulary with numbered/linked placeholders, Tab traversal, final caret, grouped Unicode edit and stale-session cancellation..

Required change: Small templates for shell functions, Terraform resources, YAML structures and ordinary code; tab stops and linked placeholders..

Dependencies: Stage A foundation.

Acceptance: Linked Unicode/multiline edits remap stops and final caret; unsupported syntax or ambiguous multi-cursor sessions refuse safely..

Tests: snippets::tests (7); linked placeholder bracketed paste PTY.

Source: `src/snippets.rs`; `src/app.rs`.

Evidence: [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m06"></a>
## M06: Code folding

Priority: Later. Status: **deferred**.

Baseline revalidation: Missing; baseline src/command.rs.

Current implementation/evidence: Missing; baseline src/command.rs; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: Collapse functions and YAML/HCL blocks; start with syntax/LSP ranges and visible fold markers..

Dependencies: Future product scope.

Acceptance: Collapse functions and YAML/HCL blocks; start with syntax/LSP ranges and visible fold markers..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="m07"></a>
## M07: Navigation back / forward

Priority: Next. Status: **verified**.

Baseline revalidation: Missing; baseline src/command.rs.

Current implementation/evidence: Bounded navigation Back/Forward retains exact file/cursor for goto, definitions and related jumps..

Required change: Return from definition, references, search and line jumps without losing context..

Dependencies: Stage A foundation.

Acceptance: Back/Forward restores the expected file, line and column without overwriting edits..

Tests: audit_navigation_back_forward_returns_to_exact_file_and_cursor; Save All/navigation PTY.

Source: `src/app.rs`; `src/command.rs`.

Evidence: [language-verification.md](language-verification.md); [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m08"></a>
## M08: Document symbols / outline

Priority: Next. Status: **verified**.

Baseline revalidation: Partial; baseline src/syntax_tree.rs.

Current implementation/evidence: Searchable server document outline plus bounded offline local symbols, including JSON/YAML/HCL..

Required change: Rust/Python/Shell symbols exist. Add LSP documentSymbol and YAML/HCL/JSON outline fallbacks..

Dependencies: Stage A foundation.

Acceptance: Local outline works offline and server result navigation validates file/range and current intent..

Tests: syntax_tree::tests (16); outline and symbol protocol tests.

Source: `src/syntax_tree.rs`; `src/lsp.rs`; `src/app.rs`.

Evidence: [language-verification.md](language-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m09"></a>
## M09: Workspace symbol search

Priority: Next. Status: **verified**.

Baseline revalidation: Missing; baseline src/lsp.rs.

Current implementation/evidence: Workspace-symbol requests are scoped, cancellable and query-generation bound; widened/empty queries do not retain old/local subsets..

Required change: Find symbols across files with a clear language-server prerequisite..

Dependencies: Stage A foundation.

Acceptance: Latest query wins and results name workspace files; empty response remains empty..

Tests: real Pyright workspace-symbol fixture; workspace_symbol_query_widening_discards_the_previous_result_subset; empty_workspace_symbols_do_not_fall_back_to_current_file_outline; switching_symbol_scope_invalidates_the_previous_panel_request; unchanged_inactive_document_sync_is_idempotent.

Source: `src/lsp.rs`; `src/app.rs`.

Evidence: [language-verification.md](language-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Actual real-server stdio/core workflow verified; packaged interactive completion/import/navigation/Problems selection remains unqualified..

<a id="m10"></a>
## M10: Project Problems

Priority: Next. Status: **verified**.

Baseline revalidation: Partial; baseline src/lsp.rs.

Current implementation/evidence: Aggregate Problems retains diagnostics for inactive unsaved open files across pooled servers and names each file..

Required change: Current-file problems exist. Add file grouping and aggregate workspace diagnostics..

Dependencies: Stage A foundation.

Acceptance: Inactive diagnostics replace correctly, preserve file identity and disappear on relevant close/update..

Tests: real mixed-language unsaved diagnostics fixture; stale document protocol tests.

Source: `src/lsp.rs`; `src/app.rs`.

Evidence: [language-verification.md](language-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Actual real-server stdio/core workflow verified; packaged interactive completion/import/navigation/Problems selection remains unqualified..

<a id="m11"></a>
## M11: Completion imports and resolution

Priority: Next. Status: **verified**.

Baseline revalidation: Partial; baseline src/lsp.rs.

Current implementation/evidence: F09 atomic completion/import/resolve implementation..

Required change: Complete the LSP edit lifecycle; add snippet support separately..

Dependencies: Stage A foundation.

Acceptance: Primary and supporting edits succeed or refuse as one undo group..

Tests: F09 synthetic and actual Python/TypeScript tests.

Source: `src/lsp.rs`; `src/app.rs`; `src/buffer.rs`.

Evidence: [language-verification.md](language-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Actual real-server stdio/core workflow verified; packaged interactive completion/import/navigation/Problems selection remains unqualified..

<a id="m12"></a>
## M12: Warm language services

Priority: Next. Status: **verified**.

Baseline revalidation: Partial; baseline src/lsp.rs.

Current implementation/evidence: F10 bounded warm language-server pool and synchronized open buffers..

Required change: Pool per-language clients and retain unsaved document synchronization..

Dependencies: Stage A foundation.

Acceptance: Alternating supported languages keeps services warm and unsaved documents synchronized..

Tests: real mixed-language process reuse/diagnostics/references fixture; switching_symbol_scope_invalidates_the_previous_panel_request; unchanged_inactive_document_sync_is_idempotent.

Source: `src/lsp.rs`; `src/app.rs`.

Evidence: [language-verification.md](language-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Actual real-server stdio/core workflow verified; packaged interactive completion/import/navigation/Problems selection remains unqualified..

<a id="m13"></a>
## M13: Search include/exclude filters

Priority: Next. Status: **verified**.

Baseline revalidation: Missing; baseline src/search.rs.

Current implementation/evidence: Bounded .mellow/search.json include/exclude and limits apply to disk/unsaved overlays; scope and skipped counts render at 80x24..

Required change: Filter paths and decide whether ignored files participate; keep scope/truncation visible..

Dependencies: Stage A foundation.

Acceptance: Filters apply consistently; escaped symlinks are excluded and truncation is visible..

Tests: search::tests; workspace::tests; project_search_configured_scope_and_filtered_counts_are_visible_at_80_by_24.

Source: `src/search.rs`; `src/workspace.rs`; `src/ui.rs`.

Evidence: [language-verification.md](language-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m14"></a>
## M14: Asynchronous search and discovery

Priority: Next. Status: **verified**.

Baseline revalidation: Missing worker path; baseline src/workspace.rs.

Current implementation/evidence: F08 asynchronous cancellable search/discovery, plus ready-generation quick-open acceptance..

Required change: Debounce, cancel and stream results while keeping input responsive..

Dependencies: Stage A foundation.

Acceptance: Old query/discovery results cannot replace or open stale candidates..

Tests: F08 workers; audit_quick_open_never_opens_stale_candidates_while_query_is_pending; quick_open_pending_discovery_shows_searching_at_80_by_24.

Source: `src/search.rs`; `src/workspace.rs`; `src/app.rs`.

Evidence: [language-verification.md](language-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Slow-storage/SSH performance not measured; local synthetic cancellation/responsiveness evidence only..

<a id="m15"></a>
## M15: Replacement preview

Priority: Improve. Status: **deferred**.

Baseline revalidation: Present / bounded; baseline src/app.rs.

Current implementation/evidence: Existing current-buffer replacement preview, literal replacement and one-undo Replace All are preserved. Multi-file preview expansion remains explicitly deferred..

Required change: Keep existing preview; expand reviewed multi-file transaction UX and define undo after save..

Dependencies: Stage A foundation.

Acceptance: Preserve current-buffer preview; a future multi-file design requires reviewed transactions and undo-after-save policy..

Tests: goal_replace_ regressions (3).

Source: `src/app.rs`.

Evidence: [language-verification.md](language-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m16"></a>
## M16: Save-versus-disk comparison

Priority: Next. Status: **verified**.

Baseline revalidation: Partial; baseline src/buffer.rs.

Current implementation/evidence: Read-only save-versus-disk comparison reports changed lines and newline-only differences; ordinary conflict-safe save remains authoritative..

Required change: External-change prompt exists; add compare/reload/merge views so users can decide without leaving Mellow..

Dependencies: Stage A foundation.

Acceptance: Comparison never reports agreement when final newline/EOL bytes differ and never changes either side..

Tests: audit_disk_comparison_does_not_hide_final_newline_differences; existing save conflict tests.

Source: `src/app.rs`.

Evidence: [../audit-workflows.md](../audit-workflows.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m17"></a>
## M17: Two files side-by-side diff

Priority: Later. Status: **deferred**.

Baseline revalidation: Partial; baseline src/command.rs.

Current implementation/evidence: Partial; baseline src/command.rs; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: Git and AI diffs exist. Add arbitrary file comparison and synchronized scrolling..

Dependencies: Future product scope.

Acceptance: Git and AI diffs exist. Add arbitrary file comparison and synchronized scrolling..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="m18"></a>
## M18: Multi-root workspaces

Priority: Later. Status: **deferred**.

Baseline revalidation: Missing; baseline src/app.rs.

Current implementation/evidence: Missing; baseline src/app.rs; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: Useful for app + infra repositories; first make one root reliable..

Dependencies: Future product scope.

Acceptance: Useful for app + infra repositories; first make one root reliable..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="m19"></a>
## M19: More than two editor panes

Priority: Later. Status: **deferred**.

Baseline revalidation: Missing; baseline src/app.rs.

Current implementation/evidence: Missing; baseline src/app.rs; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: Two panes already work. Generalize only after real usage requires it..

Dependencies: Future product scope.

Acceptance: Two panes already work. Generalize only after real usage requires it..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="m20"></a>
## M20: File operation undo / trash

Priority: Next. Status: **verified**.

Baseline revalidation: Missing; baseline src/app.rs.

Current implementation/evidence: Exclusive recoverable same-filesystem trash/restore with private manifests and restart support; existing/new work never overwritten..

Required change: Current explorer deletion is confirmed and blocks open files, but removes files directly. Offer recovery for accidental deletion..

Dependencies: Stage A foundation.

Acceptance: Restore after restart; refuse changed review/parent/manifest or existing destination; cross-device failure leaves source..

Tests: file_ops::tests (9).

Source: `src/file_ops.rs`; `src/app.rs`.

Evidence: [security-verification.md](security-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9; 917238f93ec19a7036bfd7fba777a514c8032bbf · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m21"></a>
## M21: Clipboard history

Priority: Later. Status: **deferred**.

Baseline revalidation: Missing; baseline src/command.rs.

Current implementation/evidence: Missing; baseline src/command.rs; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: Local ring with sensitive-content opt-out, separate from system clipboard..

Dependencies: Future product scope.

Acceptance: Local ring with sensitive-content opt-out, separate from system clipboard..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="m22"></a>
## M22: Keyboard binding UI and key recorder

Priority: Next. Status: **verified**.

Baseline revalidation: Missing; baseline src/keymap.rs.

Current implementation/evidence: Searchable binding view reads the active keymap source, including MELLOW_KEYMAP; Terminal Doctor records decoded delivered key events..

Required change: Configurable bindings and VS Code/nano presets already exist. Add a screen that shows the actual key bytes/capabilities received..

Dependencies: Stage A foundation.

Acceptance: Actual configured bindings are visible and decoded delivered shortcuts can be inspected without secret paste retention..

Tests: keymap regressions; audit_terminal_doctor_omits_pasted_secrets_and_records_delivered_key; Doctor PTY.

Source: `src/app.rs`; `src/keymap.rs`.

Evidence: [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m23"></a>
## M23: Task runner / clickable errors

Priority: Next. Status: **verified**.

Baseline revalidation: Missing; baseline src/command.rs.

Current implementation/evidence: Trusted explicit argv tasks execute off-thread with bounded deadlines/output and cancellability; failed output retains clickable existing path:line:column locations..

Required change: Shell exists. Add trusted named validate/test/build tasks and file:line links..

Dependencies: M28 trust; F04 bounded subprocess runner.

Acceptance: Untrusted tasks cannot launch; failed output remains reviewable and existing Unicode locations navigate correctly; no implicit shell execution..

Tests: audit_task_failure_preserves_stderr_links_without_save_and_trust_is_required; audit_task_locations_recognize_existing_unicode_paths_after_tool_prefixes.

Source: `src/app.rs`; `src/format.rs`.

Evidence: [security-verification.md](security-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m24"></a>
## M24: Test discovery / test results

Priority: Later. Status: **deferred**.

Baseline revalidation: Missing; baseline src/command.rs.

Current implementation/evidence: Missing; baseline src/command.rs; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: Start with task output/problem matchers, then test adapters if users need them..

Dependencies: Future product scope.

Acceptance: Start with task output/problem matchers, then test adapters if users need them..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="m25"></a>
## M25: Debugger / DAP

Priority: Later. Status: **deferred**.

Baseline revalidation: Missing; baseline src/main.rs.

Current implementation/evidence: Missing; baseline src/main.rs; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: Breakpoints, variables and stepping are a separate substantial product investment..

Dependencies: Future product scope.

Acceptance: Breakpoints, variables and stepping are a separate substantial product investment..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="m26"></a>
## M26: Git stash / worktrees / richer conflict view

Priority: Later. Status: **deferred**.

Baseline revalidation: Partial Git surface; baseline src/git.rs.

Current implementation/evidence: Partial Git surface; baseline src/git.rs; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: Stage/commit/branches/history already work. Add high-value missing workflows progressively..

Dependencies: Future product scope.

Acceptance: Stage/commit/branches/history already work. Add high-value missing workflows progressively..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="m27"></a>
## M27: CLI multi-file, --wait, --goto, stdin

Priority: Next. Status: **verified**.

Baseline revalidation: Missing; baseline src/main.rs.

Current implementation/evidence: CLI supports multiple paths, --goto, attached --wait, --trust and bounded UTF-8 stdin drafts. Concrete Darwin tty reopening restores interaction after piped input..

Required change: Current CLI accepts one PATH and --no-mouse. These options let Git, scripts and other tools use Mellow as an editor..

Dependencies: Stage A foundation.

Acceptance: Correct tab/caret saves, wait remains attached, and piped Unicode draft remains editable and recoverable..

Tests: cli_supports_multiple_paths_goto_wait_stdin_and_explicit_trust; multi-file/goto/wait and piped Unicode stdin PTY.

Source: `src/main.rs`; `src/app.rs`.

Evidence: [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m28"></a>
## M28: Workspace trust / restricted mode

Priority: Before richer execution. Status: **verified**.

Baseline revalidation: Missing; baseline src/app.rs.

Current implementation/evidence: Restricted-by-default session trust gates LSP, formatters, tasks and configured validators; canonical workspace/display-parent boundary excludes outside aliases. Revocation cancels/stops services..

Required change: Gate optional LSP, formatter and task execution for untrusted projects. Core editing stays available..

Dependencies: Stage A foundation.

Acceptance: Core editing works restricted; outside files remain editable but never inherit workspace execution trust..

Tests: execution_trust regressions; audit_explicit_validator_cannot_inherit_another_workspaces_execution_trust; outside-file/symlink native PTY positive controls.

Source: `src/app.rs`; `src/main.rs`.

Evidence: [security-verification.md](security-verification.md); [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m29"></a>
## M29: AI context exclusions / redaction

Priority: Next. Status: **verified**.

Baseline revalidation: Missing policy layer; baseline src/ai.rs.

Current implementation/evidence: Mandatory secret paths/content exclusions plus bounded private user-owned additive ai-deny.conf. Canonical aliases and repository-root staged paths fail closed. Existing selection disclosure remains required..

Required change: Explicit selection disclosure exists; add deny patterns for secrets, staged diff review and opt-in context sources..

Dependencies: Stage A foundation.

Acceptance: Denied context never reaches local mock provider; defaults cannot be removed and malformed/insecure policy refuses..

Tests: additive_deny_globs_match_paths_and_defaults_cannot_be_removed; user_local_policy_is_bounded_and_resolves_file_aliases_fail_closed; staged_alias_privacy_uses_the_repository_root_not_process_cwd; ordinary AI disclosure PTY.

Source: `src/ai.rs`; `src/app.rs`.

Evidence: [security-verification.md](security-verification.md); [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m30"></a>
## M30: AI usage, cost and request limits

Priority: Next. Status: **verified**.

Baseline revalidation: Missing; baseline src/ai.rs.

Current implementation/evidence: Per-session request/output-token/observed-token budgets count failed and cancelled attempts; actual supplied usage survives stale/error results; missing usage stays unknown. No monetary ceiling claim..

Required change: Record provider/model/token usage where available, distinguish estimates, cap outputs and expose budget policies..

Dependencies: Stage A foundation.

Acceptance: Queued usage is polled before new authorization; output/request caps enforced and unknown/partial usage is honestly displayed..

Tests: audit_ai_budgets_count_failed_attempts_unknown_usage_and_ignore_stale_progress; audit_disabled_history_does_not_create_checkpoints_and_pending_usage_closes_budget; streaming usage protocol tests.

Source: `src/ai.rs`; `src/settings.rs`; `src/app.rs`.

Evidence: [security-verification.md](security-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m31"></a>
## M31: AI streaming / cancellable transport

Priority: Next. Status: **verified**.

Baseline revalidation: Missing; baseline src/app.rs.

Current implementation/evidence: Actual cancellable async HTTP supports bounded SSE and JSON fallback; data-only sanitized preview, completion validation and explicit review gate prevent partial/late application..

Required change: Current Escape dismisses the UI response; blocking worker requests can continue. Explain this and add best-effort transport cancellation..

Dependencies: Stage A foundation.

Acceptance: Escape closes fixture transport; valid late proposal cannot overwrite subsequent edits or reopen review; partial streams never apply..

Tests: cancellation_aborts_the_inflight_transport_and_closes_its_socket; streaming_missing_completion_marker_never_accepts_partial_proposals; actual SSE cancellation PTY.

Source: `src/ai.rs`; `src/app.rs`.

Evidence: [security-verification.md](security-verification.md); [terminal-verification.md](terminal-verification.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Broader native/integration limits in evidence documents.

<a id="m32"></a>
## M32: Persistent AI change history

Priority: Later. Status: **deferred**.

Baseline revalidation: Missing; baseline docs/ai.md.

Current implementation/evidence: Missing; baseline docs/ai.md; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: Session summaries exist. Store optional before/after fingerprints and accepted/rejected outcomes without retaining secrets by default..

Dependencies: Future product scope.

Acceptance: Session summaries exist. Store optional before/after fingerprints and accepted/rejected outcomes without retaining secrets by default..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="m33"></a>
## M33: AI multi-file proposals

Priority: Later. Status: **deferred**.

Baseline revalidation: Missing by current design; baseline docs/ai.md.

Current implementation/evidence: Missing by current design; baseline docs/ai.md; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: Selection edits are single-file. Add only with reviewed transaction boundaries and rollback..

Dependencies: Future product scope.

Acceptance: Selection edits are single-file. Add only with reviewed transaction boundaries and rollback..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="m34"></a>
## M34: Extension ecosystem

Priority: Defer. Status: **deferred**.

Baseline revalidation: Missing; baseline Cargo.toml.

Current implementation/evidence: Missing; baseline Cargo.toml; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: A full marketplace adds compatibility/security costs. Begin with constrained language/task/snippet bundles..

Dependencies: Future product scope.

Acceptance: A full marketplace adds compatibility/security costs. Begin with constrained language/task/snippet bundles..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="m35"></a>
## M35: Accessibility beyond colors/keyboard

Priority: Research now. Status: **blocked**.

Baseline revalidation: Unverified / no dedicated screen-reader mode found; baseline src/terminal.rs.

Current implementation/evidence: Actual host/accessibility inventory and 80x24 keyboard/render evidence recorded; no running VoiceOver or participant evaluation was available..

Required change: Evaluate with real screen readers and users; color fallbacks alone do not prove accessibility..

Dependencies: Stage A foundation.

Acceptance: Real assistive-technology/user evidence is required beyond color/keyboard source tests..

Tests: native keyboard PTY and 80x24 TestBackend renders; host VoiceOver inventory.

Source: `docs/audit/accessibility-evidence.md`; `src/ui.rs`.

Evidence: [accessibility-evidence.md](accessibility-evidence.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: ecff35f09970250a553747bf6d869f1756cb4088 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Native VoiceOver/braille and real assistive-technology participant evaluation unavailable; Terminal.app/iTerm2/Ghostty, physical fonts/mouse and SSH/tmux not qualified..

<a id="m36"></a>
## M36: Windows native / remote orchestration

Priority: Defer. Status: **deferred**.

Baseline revalidation: Not supported / absent; baseline src/pty.rs.

Current implementation/evidence: Not supported / absent; baseline src/pty.rs; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: Linux through WSL and running Mellow on an SSH host are existing paths. No native ConPTY or VS Code-style remote manager is present..

Dependencies: Future product scope.

Acceptance: Linux through WSL and running Mellow on an SSH host are existing paths. No native ConPTY or VS Code-style remote manager is present..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="m37"></a>
## M37: Encoding conversion / EOL controls

Priority: Later. Status: **deferred**.

Baseline revalidation: Partial; baseline src/buffer.rs.

Current implementation/evidence: Partial; baseline src/buffer.rs; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: UTF-8, BOM and CRLF preservation exist. Other encodings and explicit conversion UI were not found..

Dependencies: Future product scope.

Acceptance: UTF-8, BOM and CRLF preservation exist. Other encodings and explicit conversion UI were not found..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="m38"></a>
## M38: Large file / log viewer mode

Priority: Later. Status: **deferred**.

Baseline revalidation: Partial; baseline src/buffer.rs.

Current implementation/evidence: Partial; baseline src/buffer.rs; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: 100 MiB open limit, reduced intelligence above 5 MiB. Add measured long-line/wrap behavior, read-only/tail workflows if demanded..

Dependencies: Future product scope.

Acceptance: 100 MiB open limit, reduced intelligence above 5 MiB. Add measured long-line/wrap behavior, read-only/tail workflows if demanded..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="m39"></a>
## M39: Signed releases / repair diagnostics

Priority: Next. Status: **blocked**.

Baseline revalidation: Partial; baseline scripts/package-macos.sh.

Current implementation/evidence: Exact source/digest acceptance, release attestation wiring, explicit optional codesign verification/required-signing refusal and Terminal Doctor repair report implemented. Signing credentials/notarization absent..

Required change: Checksums and installer exist. Add provenance, macOS signing, and a single diagnostics report for terminal and integrations..

Dependencies: Stage A foundation.

Acceptance: Release provenance must execute and native signature/notarization must validate with authorized identity before claiming signed distribution..

Tests: workflow actionlint; package digests and extracted PTY; codesign hook validation; Doctor PTY; MELLOW_REQUIRE_SIGNING=1 refuses unsigned package creation with explicit missing-identity reason.

Source: `.github/workflows/release.yml`; `scripts/package-macos.sh`; `src/app.rs`.

Evidence: [../RELEASE_CHECKLIST.md](../RELEASE_CHECKLIST.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: ff6eb860caaaef74fc34d2a3480690f9ea50c89e; 917238f93ec19a7036bfd7fba777a514c8032bbf · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Apple signing identity and notarization credentials unavailable; no signature/notarization/provenance publication executed..

<a id="i01"></a>
## I01: Safe configuration change

Priority: Stage D. Status: **verified**.

Baseline revalidation: Proposal absent at baseline.

Current implementation/evidence: Small JSON/YAML structural-change review refuses ambiguous parsing, redacts secret values, records recovery/history, and explicitly reviews optional trusted argv validators. Only successful current validation offers Save..

Required change: Small first version. Start with JSON/YAML validation and user-configured shell/format checks. Add Terraform and Kubernetes integrations only with explicit context and command visibility. Some validators can execute code or contact services; do not label all validation “read-only.”.

Dependencies: Stage A/B/C foundation; M28 trust; M02 history; F04 bounded subprocess runner.

Acceptance: Review reveals exact changed paths; failed/cancelled/stale validators never save/deploy and malformed validator config never bypasses validation..

Tests: config_change::tests (7); audit_config_validator_failed_cancelled_or_stale_never_offers_save; audit_validator_config_error_or_oversize_never_offers_implicit_structural_save; JSON review PTY.

Source: `src/config_change.rs`; `src/app.rs`.

Evidence: [terminal-verification.md](terminal-verification.md); [../audit-workflows.md](../audit-workflows.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: First-time participant/pilot usefulness evaluation unavailable; source and native PTY results cannot establish participant acceptance..

<a id="i02"></a>
## I02: Terminal Doctor

Priority: Stage D. Status: **blocked**.

Baseline revalidation: Proposal absent at baseline.

Current implementation/evidence: Small Terminal Doctor shows terminal/color/key/paste/mouse/clipboard route facts and decoded event recorder. Redacted report is copied only by explicit action; pasted contents are omitted..

Required change: Small first version. Detect legacy/enhanced keys, color tier, paste protocol, clipboard route and mouse mode. Produce a redacted diagnostic bundle with user consent..

Dependencies: Stage A/B/C foundation; Redacted decoded-event handling.

Acceptance: Report contains available protocol facts and decoded events without secret paste text; terminal-family/SSH usability must be separately qualified..

Tests: audit_terminal_doctor_omits_pasted_secrets_and_records_delivered_key; native Doctor PTY.

Source: `src/app.rs`; `src/ui.rs`.

Evidence: [terminal-verification.md](terminal-verification.md); [accessibility-evidence.md](accessibility-evidence.md).

Verification scope: Source/module/protocol and recorded native PTY evidence as applicable; packaged exact-commit gates recorded separately..

Commit/PR: e324b7f0e64080f8c911ae41230e99ebb40313a9 · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Native Terminal.app, iTerm2, Ghostty, WSL/Windows Terminal and SSH/tmux comparison remains unperformed..

<a id="i03"></a>
## I03: Change timeline and checkpoints

Priority: Later. Status: **deferred**.

Baseline revalidation: Proposal absent at baseline.

Current implementation/evidence: M02 implements per-file checkpoints and explicit save/AI-accept events; a full timeline and multi-file checkpoint consistency remain Later..

Required change: Small first version. Begin with per-file checkpoints and explicit save/AI accept events. Use retention/size controls and private permissions. Add multi-file checkpoint consistency later..

Dependencies: Stages A/B/C verified.

Acceptance: Proof of value. A user restores an earlier state after restart without losing newer work; history does not retain excluded secrets..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="i04"></a>
## I04: AI request receipt

Priority: Later. Status: **deferred**.

Baseline revalidation: Proposal absent at baseline.

Current implementation/evidence: M29–M31 implement deny policies, budgets, provider usage and cancellation; durable request IDs/receipts and price accounting remain Later..

Required change: Small first version. Use the existing provider adapters. Add request IDs, output caps, context allow/deny rules and per-session budgets. Label estimated prices and absent usage honestly..

Dependencies: Stages A/B/C verified.

Acceptance: Proof of value. A user can explain what was sent and why a request cost something; cancelling cannot be misrepresented as a refund or guaranteed server-side cancellation..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="i05"></a>
## I05: Environment comparison

Priority: Later. Status: **deferred**.

Baseline revalidation: Proposal absent at baseline.

Current implementation/evidence: Proposal absent at baseline; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: Small first version. Start entirely locally with two YAML/JSON files. Show paths such as spec.replicas, before/after values and structural changes; redact secret-like values. Treat arrays/order explicitly..

Dependencies: Stages A/B/C verified.

Acceptance: Proof of value. The structural view agrees with the textual diff; duplicate keys or ambiguous parsing stop the semantic view instead of hiding a change..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..

<a id="i06"></a>
## I06: Runbook capture

Priority: Later. Status: **deferred**.

Baseline revalidation: Proposal absent at baseline.

Current implementation/evidence: Proposal absent at baseline; no expansion undertaken because the report explicitly labels this Later/Defer..

Required change: Small first version. Make capture opt-in. Record exit status, timestamps, command/cwd and user-selected output. Let the user edit before exporting; never replay commands automatically..

Dependencies: Stages A/B/C verified.

Acceptance: Proof of value. Another engineer can follow the reviewed steps, and the export contains neither credentials nor unselected terminal history..

Tests: No new acceptance claimed for deferred scope.

Source: Baseline surface; explicitly deferred.

Evidence: No new capability acceptance claimed.

Verification scope: No new capability acceptance claimed..

Commit/PR: Pending focused commits · https://github.com/ritru-labs/mellow/pull/11.

Remaining gates: Requires future product-scope decision; not part of current audit implementation..
