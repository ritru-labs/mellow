# Language, navigation and search verification

This evidence covers F08–F10 and matrix rows M07–M15 from the October 11,
2026 audit. The starting commit is
`9e42e17e8307d0171525bf325efbaf8d85178383`. These runs exercised the shared
working candidate before its focused commits; the final delivery tracker must
associate them with the resulting commit and rerun the repository gates.

## Platform and scope

The native host was macOS 26.6.2 (25G83), ARM64, with Rust/Cargo 1.90.0.
Language qualification used Node 26.8.2, Pyright 1.1.414,
typescript-language-server 6.0.2 and TypeScript 5.9.3. Tools were installed in
`/private/tmp/mellow-lsp-tools`, without changing the global installation.

The real-server tests launch the actual language servers over stdio and use
Mellow's completion parsing and shared buffer edit operations. Each test obtains
an auto-import, applies its primary edit and supporting import together, checks
one-step undo/redo, follows a definition to the helper file and receives a
diagnostic for an undefined name. These are source/core-buffer tests, not
interactive terminal completion tests. A live provider command was not returned
by either qualified fixture; synthetic tests check disclosure without execution.

A third real-server test starts Pyright and TypeScript together. Four alternating
cycles preserve both process IDs, each service receives an inactive unsaved
update, aggregate diagnostics retain both file identities, and disk originals
remain untouched. References in both unsaved documents include their call sites.
Pyright workspace-symbol search returns the symbol and file from its unsaved
document. This qualifies the service/model workflow over real stdio; it does not
qualify selecting those rows in a packaged terminal panel.

The search and navigation tests are native source tests and terminal rendering
fixtures. They do not establish performance on an SSH connection or slow remote
storage. Separate packaging acceptance reports are required for packaged-artifact
claims and other native architectures.

## Coverage and remaining gates

| Item | Status within this evidence | Current implementation and regression evidence | Remaining gate or deferred change |
| --- | --- | --- | --- |
| F08 asynchronous search/discovery | Verified | Generation-tagged workers, debounce, cancellation, progress, bounded scope and unsaved overlays. Tests cover cancellation, old generations, Git subprocess descendants retaining stdout, configuration validation, scope escape and bounded long lines. | Real SSH and remote-storage performance have not been measured. |
| F09 completion imports | Verified | Primary edit and supporting imports are validated together and applied in one undo group. Resolve preserves opaque provider data; stale, overlapping, reversed or malformed edits apply neither edit. Eight F09 regressions and both real Python/TypeScript workflows pass. Commands remain disclosed and unexecuted. | Packaged interactive completion on each supported terminal/platform is not established by these tests. Protocol snippets remain explicitly separate work. |
| F10 pooled language services | Verified | Client identity includes server configuration and workspace. Real Pyright/TypeScript retain both process IDs across four alternating cycles, receive inactive unsaved updates, retain named aggregate diagnostics and resolve references in both unsaved files. Pending documents and inactive-document versions also have synthetic regressions. | Interactive mixed-language switching and opening named workspace Problems in the packaged terminal remain separate native acceptance gates. |
| M07 navigation back/forward | Verified | Bounded history records file and cursor context. `audit_navigation_back_forward_returns_to_exact_file_and_cursor` checks exact restoration in both directions. Definition, references, project search, symbols and line-jump paths record navigation context. | Native packaged interaction for every entry path remains part of terminal acceptance. |
| M08 document symbols/outline | Verified | LSP hierarchy and file identity have parser coverage. Sixteen syntax-tree tests pass, including Rust/Python/Shell and JSON/YAML/Terraform fallback outlines. | Full document-symbol interoperability across the supported-server list is not qualified. |
| M09 workspace symbols | Verified | Real Pyright finds a symbol with exact file identity in an unsaved document. Requests cancel superseded same-kind requests; reversed replies cannot replace the newest query. Backspace widening requests fresh symbols, and empty workspace results cannot fall back to a current-file outline. | Selection/navigation through the packaged terminal UI remains to be exercised. |
| M10 project Problems | Verified | Diagnostics are retained per document and aggregated with file names across active and pooled services. The real mixed-server fixture replaces each file's diagnostics after inactive unsaved changes and retains both file identities. | Opening a selected diagnostic from the packaged terminal panel remains unqualified by these source/model tests. |
| M11 completion imports/resolution | Verified | Same implementation and evidence as F09. Completion provider commands cannot run; resolve errors or stale revisions cannot partially apply an item. | LSP snippet expansion is explicitly separate from Mellow's local snippet feature. |
| M12 warm services | Verified | Same implementation and real two-server switching, inactive unsaved diagnostics and references evidence as F10. Closing a document invalidates pending requests; reopening cannot resurrect an old completion. | Same interactive packaged-terminal gate as F10. |
| M13 search filters | Verified | Bounded `.mellow/search.json` include/exclude patterns and limits apply to disk and unsaved files. Worker reports show configured scope, filtered/oversized counts and truncation. A real 80×24 TestBackend render checks scope and neutral file-limit wording. Git discovery honors ignores; explicit path input remains deliberate. | Actual terminal mouse/keyboard filtering flows are part of packaged acceptance. |
| M14 responsive search/discovery | Verified | Same F08 worker evidence, with current-generation quick-open ranking and explicit-path completion. Cancelling a worker does not synchronously join it on the input thread. | Slow remote storage and SSH throughput are not represented by local synthetic files. |
| M15 replacement preview | Deferred | Existing current-buffer preview, literal replacement safety and one-undo Replace All remain covered by three passing regressions. | The audit labels this Improve. Reviewed multi-file replacement transactions and undo-after-save policy are not implemented by the current-buffer feature. Keep this scope visible; do not mark its expansion complete. |

## Focused corrections from review

- Search canonicalizes disk targets and refuses symlinks escaping the workspace.
  On Unix it opens the canonical target nonblocking with no-follow protection.
- LSP requests snapshot URI/revision. Rename and code-action responses also bind
  versions of other open documents, preventing stale inactive-file edits.
- A newer request cancels older requests of the same kind, including completion,
  resolve, references and workspace symbols. Reversed responses cannot replace
  the latest request on an unchanged document.
- Malformed workspace-edit containers reject the whole edit set. An unsolicited
  server `workspace/applyEdit` is explicitly refused because no user review
  authorized it. A null formatting result remains a valid no-op.
- Workspace-symbol Backspace changes now request the widened query, and the
  search panel shows effective include/exclude scope and all skipped counts.
- Definition, references, hover and signature responses bind the buffer identity,
  revision, cursor and editing mode. Esc cancels their requests in every mode;
  delayed replies cannot navigate, reopen panels or replace newer status text
  after a palette interaction, cursor move, edit or same-path buffer replacement.
  Cancellation leaves the expected document/workspace symbol panel lifecycle
  intact.
- Final review reproduced two further defects before fixing them: switching
  document/workspace symbol scope allowed an older response to replace the new
  panel, and synchronizing an unchanged inactive file emitted a duplicate-version
  full-text change. Symbol scopes now share latest-request invalidation, and
  synchronization skips documents already cached at identical version/content.

## Reproduction and results

Use the repository-pinned Rust 1.90.0 toolchain. On this host it was selected
with a command-scoped `PATH` prefix; no default toolchain was changed.

```sh
cargo test --locked --bin mellow lsp::tests
cargo test --locked --bin mellow search::tests
cargo test --locked --bin mellow workspace::tests
cargo test --locked --bin mellow syntax_tree::tests
cargo test --locked --bin mellow f09_
cargo test --locked --bin mellow audit_navigation_back_forward_returns_to_exact_file_and_cursor
cargo test --locked --bin mellow workspace_symbol_query_widening_discards_the_previous_result_subset
cargo test --locked --bin mellow empty_workspace_symbols_do_not_fall_back_to_current_file_outline
cargo test --locked --bin mellow lsp_interaction
cargo test --locked --bin mellow current_lsp_reference_intent_opens_its_results
cargo test --locked --bin mellow interaction_cancellation_drops_responses_without_cancelling_symbol_panels
cargo test --locked --bin mellow project_search_configured_scope_and_filtered_counts_are_visible_at_80_by_24
cargo test --locked --bin mellow goal_replace_
MELLOW_TEST_PYRIGHT=/private/tmp/mellow-lsp-tools/node_modules/.bin/pyright-langserver \
MELLOW_TEST_TYPESCRIPT_LSP=/private/tmp/mellow-lsp-tools/node_modules/.bin/typescript-language-server \
cargo test --locked --bin mellow real_ -- --ignored --nocapture --test-threads=1
```

| Run | Result | Local retained log |
| --- | --- | --- |
| LSP source module | 36 passed, 0 failed, 2 ignored real-server tests | `/private/tmp/mellow-lsp-source-tests.log` |
| Search source module | 16 passed, 0 failed | `/private/tmp/mellow-search-source-tests.log` |
| Workspace source module | 11 passed, 0 failed | `/private/tmp/mellow-workspace-source-tests.log` |
| Syntax outlines | 16 passed, 0 failed | `/private/tmp/mellow-outline-tests.log` |
| F09 app/protocol regressions | 8 passed, 0 failed | `/private/tmp/mellow-completion-regressions.log` |
| Navigation restoration | 1 passed, 0 failed | `/private/tmp/mellow-navigation-tests.log` |
| Current-buffer replacement | 3 passed, 0 failed | `/private/tmp/mellow-replacement-regressions.log` |
| Explicit ignored real-server workflows | 2 passed, 0 failed, 0 ignored | `/private/tmp/mellow-real-language-workflows.log` |
| Explicit real mixed-language pool/symbols/references | 1 passed, 0 failed, 0 ignored | `/private/tmp/mellow-real-mixed-language-workflow.log` |
| Abandoned interaction binding | 1 passed, 0 failed | `/private/tmp/mellow-lsp-intent-regressions.log` |
| All source binary tests after final language changes | 523 passed, 0 failed, 3 ignored | `/private/tmp/mellow-all-source-bin-language-final.log` |
| All three ignored real-language tests explicitly enabled | 3 passed, 0 failed, 0 ignored | `/private/tmp/mellow-real-language-final-workflows.log` |
| Clippy, all targets/features, warnings denied | Passed | `/private/tmp/mellow-language-final-clippy.log` |
| LSP source module after final review corrections | 39 passed, 0 failed, 3 ignored | `/private/tmp/mellow-final-review-lsp-tests.log` |
| Real-language workflows after final review corrections | 3 passed, 0 failed, 0 ignored | `/private/tmp/mellow-final-review-real-language.log` |
| Clippy after final review corrections | Passed | `/private/tmp/mellow-final-review-clippy.log` |

Additional individually run regressions for same-document reversed replies,
workspace-symbol query widening, configured-scope worker reporting, and both
search scope render tests passed. The empty-workspace-symbol regression failed
before its correction and passed afterward. The logs above are local evidence and are not
durable repository artifacts; CI must retain its own reports for the exact
committed candidate. Test groups overlap and their counts must not be summed as
unique tests.

The final combined live run retained Pyright PID 38643 and TypeScript PID 38644
in the mixed fixture. Separate process IDs are fixture evidence of reuse, not a
portable promise about those numeric IDs. `git diff --check` and Rust 1.90
formatting of the changed Rust files also passed; the lead engineer still owns
the final committed-candidate packaging and repository-wide acceptance gates.

The two final review regressions are
`switching_symbol_scope_invalidates_the_previous_panel_request` and
`unchanged_inactive_document_sync_is_idempotent`. Their original failures are
retained in `/private/tmp/mellow-symbol-scope-regression-before.log` and
`/private/tmp/mellow-idempotent-sync-regression-before.log`. Both pass with the
corrections, and the real mixed-server workflow was rerun because synchronization
behavior changed. Workflow actionlint 1.7.12 passed for both `release.yml` and
`candidate-artifacts.yml`; all 15 release and six candidate inline shell steps
also passed `bash -n`. These static checks do not establish CI execution.

## Observed compatibility limits

The unpinned TypeScript 7.0.2 package lacked the `tsserver.js` entry required by
typescript-language-server and failed initialization. The install guide pins
the successfully qualified TypeScript 5.9.3/server 6.0.2 pair.

Initial auto-import probes also exposed a tsserver export-cache failure when
completion was requested before initial document analysis. The qualified
fixture waits for the first document diagnostics before requesting auto-import.
That distinguishes protocol initialization from indexing readiness; it does not
prove that every server startup timing race is fixed. Mellow refuses a failed
resolve without applying edits, and users can request completion again after
analysis. Earlier failed version/timing probes remain in local qualification
logs rather than being described as successful live-server evidence.

The nonpublishing candidate workflow validates source and installed archives on
four native targets and retains reports. Creating that workflow is not evidence
that GitHub runners executed it. No release was published by this work.

## Packaged completion boundary correction

Actual 80×24 Python/TypeScript package interaction at `757d8b0` rejected a valid
first-line auto-import, although earlier real-server/core-buffer tests passed.
The app validator confused a zero-width insertion at a replacement boundary
with two ambiguous insertions at the same point. The narrow correction at
`cd5e747` permits start/end boundary insertions, retains true-overlap and duplicate
point refusal, and tests Unicode, atomic undo/redo and exact caret restoration.
The [LSP edit-array rules](https://github.com/microsoft/language-server-protocol/blob/gh-pages/_specifications/specification-3-14.md#textedit-1)
allow insertion followed by replacement at the same start position. The actual
packaged server interactions, including failure evidence, are recorded separately
from source tests and linked in PR #11.

## Actual extracted real-server keyboard workflows

The corrected `cd5e74754db95abfd622a3ab7608588e424102b1` macOS ARM64 archive
passed both 80×24 keyboard workflows with real Pyright 1.1.414 and
typescript-language-server 6.0.2/TypeScript 5.9.3. The tested binary SHA256 was
`f9d1699a7d75d62a6a444b1bbfdbf4ac5f1b805a4865c3bede73da8ddb765d2f`.
Each workflow explicitly trusted its isolated project, accepted an auto-import
completion, verified saved import and primary text, undid both edits together,
redid both, opened the helper through definition, navigated back, bracketed-pasted
an undefined name, observed its actual Problems error row, saved, and exited
through Ctrl+Q with exit code 0. Forced cleanup was not used as passing exit evidence.

[Results, provenance and qualification limits](evidence/real-lsp-cd5/verification.md),
[structured results](evidence/real-lsp-cd5/results.json) and the retained probe/screens
separate actual packaged UI evidence from earlier real-server core tests.
The probe retains the preceding packaged completion refusal and safely reaps owned
children. This establishes native macOS ARM64 PTY keyboard behavior, not native
screen-reader/Terminal-app, Intel/Linux real-LSP UI, SSH or live AI validation.

## Independent no-op completion review

Independent review found that empty supporting insertions were omitted from
Buffer's returned caret list, leaving the app's primary index misaligned. An
empty primary with a preceding import also needed its caret mapped, and an
all-no-op completion incorrectly rewrote the previous history caret. Four
regressions retained three original failures and one overlap-refusal control.
All 14 focused F09 tests pass after the narrow app correction: validate the whole
edit array first, align effective caret indices, map an empty primary from actual
preceding edit endpoints (including normalized CRLF), and update history/services
only for applied edits. Provider commands remain unexecuted and disclosed.
[RED](evidence/completion-noop-red.log) and [GREEN](evidence/completion-noop-green.log)
identify this source regression separately from packaged real-server workflows.

## Exact Unicode edit boundaries

The final adverse check reproduced a provider range inside an emoji replacing
the wrong grapheme. The shared LSP edit validator now requires both converted
endpoints to round-trip to their original UTF-16 offsets before any mutation or
no-op filtering. Whole-array refusal preserves text, revision and caret.
LSP offsets count UTF-16 units; a position inside a combining/ZWJ cluster can be
valid protocol input but is unsupported by Mellow's whole-grapheme edit model.
The visible refusal describes that limit instead of rounding and changing nearby
text. The regression covers primary and supporting positions inside emoji and
combining clusters; valid full-grapheme completion/import cases still pass.
