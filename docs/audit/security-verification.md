# Focused security, save and process verification

Candidate branch: `audit/mellow-20261011`. Starting/source HEAD at this review:
`9e42e17e8307d0171525bf325efbaf8d85178383`. Results below describe the working
candidate; HEAD alone does not contain these uncommitted changes. The parent
tracker records the eventual commit and archive identities.

Host: native macOS 26.6.2 (25G83), ARM64. Final focused commands use Rust
`1.90.0 (1159e78c4 2025-09-14)` through a command-scoped PATH. The initial module
suites are native macOS source unit/protocol tests, not signed distribution or
paid-provider qualification. The Linux VM addendum below is separate evidence.

## Executed checks

```sh
export PATH=/Users/rukmini/.rustup/toolchains/1.90.0-aarch64-apple-darwin/bin:$PATH
cargo test --locked --bin mellow buffer::tests -- --nocapture
cargo test --locked --bin mellow format::tests -- --nocapture
cargo test --locked --bin mellow ai:: -- --nocapture
cargo test --locked --bin mellow claude:: -- --nocapture
cargo test --locked --bin mellow file_ops:: -- --nocapture
cargo test --locked --bin mellow execution_trust -- --nocapture
cargo test --locked --bin mellow format_on_save -- --nocapture
cargo check --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

| Focused suite | Native result |
| --- | --- |
| Buffer/save | 42 passed, zero failed/ignored |
| Formatter/process | 13 passed, zero failed/ignored; inherited-pipe test reaches the ten-second deadline |
| AI/privacy/streaming | 28 passed, zero failed/ignored |
| Claude protocol | 4 passed, zero failed/ignored |
| Recoverable trash | 9 passed, zero failed/ignored |
| Outside-root execution boundary | 3 passed, zero failed/ignored |
| Existing format-on-save workflows | 5 passed, zero failed/ignored (including one settings test) |

The module suites total **96 tests**; the final outside-root pass ran an
additional **8 tests**. `cargo check --locked --all-targets`
passed after the cancellable APIs and clipboard wiring compiled. Pinned
all-target Clippy and full formatting checks subsequently passed too, as did
`git diff --check`. These checks must be rerun after the final candidate changes.
Release build and packaging were not executed in this focused review; their
results belong to the candidate's overall verification record. Intermediate
failures during concurrent edits are not substituted for passing final runs.

A direct Tokio dependency exposes the runtime already present transitively via
Reqwest. `cargo check --offline` generated the lock update: one addition to
Mellow's dependency list, no package-version changes. Cargo.lock was not edited
by hand.

## Regression fixtures and the failures they guard

These test names identify the exact regression scenarios. Original-release
red reproductions of F01–F04 are in the supplied audit. This focused session
ran the current regression suite; it did not re-execute every test against the
original release or claim an independently recorded red run for every new test.

| Defect/boundary | Exact regression | Original or adverse behavior exercised |
| --- | --- | --- |
| F01 | `shell_commands_reject_every_terminal_control_before_cleaning` | All C0, DEL and C1 characters, including controls that trimming previously hid, are refused before a suggestion can be reviewed/copied. |
| F02 | `redirects_never_receive_the_claude_key`, `ai_requests_do_not_follow_redirects_to_another_host`, `debug_output_never_shows_the_api_key` | Two loopback endpoints and dummy keys; redirects receive zero connections; Debug omits stored keys and URL credentials/query secrets. |
| F03 | `atomic_save_preserves_extended_attributes_owner_group_and_mode`, `atomic_save_preserves_native_acl_and_nondefault_group` | Native Darwin xattr, ACL and supplementary-group preservation on an atomic replacement. The alternate group branch ran without the unavailable-group message. |
| F03 | `saving_a_hard_link_refuses_without_mutating_either_name` | Hard-link divergence is prevented by refusal with Save As guidance; both names, original inode and dirty buffer remain intact. |
| F03 race | `save_baseline_cannot_adopt_a_writer_after_the_conflict_check` | A writer changes the file between the conflict check and save setup; the accepted baseline is retained and the later version is not overwritten. |
| F03 race | `saved_snapshot_is_bound_to_the_written_inode_not_a_later_path` | A replacement path cannot be adopted as if its bytes were the editor's saved version. |
| F03 fallback | `in_place_fallback_refuses_a_replaced_file_or_new_symlink_before_truncation` | Replaced content and a newly introduced symlink are refused before truncation. |
| F03 Save As | `publishing_a_new_file_never_replaces_one_that_appeared_meanwhile` | A destination appearing after initial inspection is retained. Unsupported exclusive atomic publication fails instead of falling back to check-plus-rename. |
| F04 | `exited_formatter_with_inherited_output_pipe_obeys_deadline` | A direct child exits while a descendant retains stdout/stderr; partial output is rejected and the operation ends at its single deadline. |
| F04 | `input_backpressure_obeys_one_deadline`, `output_is_bounded_before_acceptance`, `cancellation_kills_the_formatter_and_its_descendant` | Blocked stdin, endless output and cancellation; a delayed descendant cannot create its marker. |
| Process output | `bounded_tool_runner_preserves_failed_task_output_and_empty_clipboards`, `formatter_error_never_injects_terminal_control_characters` | Failed tasks retain stdout/stderr for diagnostics; an empty successful clipboard is valid; formatter status text strips terminal controls. |
| M31 | `cancellation_aborts_waiting_for_response_headers`, `cancellation_aborts_the_inflight_transport_and_closes_its_socket` | Cancellation during send/header wait and a silent body returns within one second and the local server observes socket EOF. |
| M31 | `streaming_openai_reports_provider_usage_and_preserves_unicode_deltas`, `streaming_claude_merges_usage_and_refuses_cut_off_text` | UTF-8 split across tiny writes, LF/CRLF SSE frames, provider usage and refusal of length-limited text. |
| M31/M30 | `streaming_missing_completion_marker_never_accepts_partial_proposals` | A partial edit proposal is never accepted without a completion marker; already reported usage survives the error. |
| M31 | `stream_errors_and_limits_withhold_credentials_and_reject_hostile_shell_controls` | Credential-echoing error events, response caps and a streamed CR in a shell command are rejected. |
| M30 | `json_usage_and_inline_output_follow_the_same_bounded_transport` | Non-streaming JSON fallback reports actual supplied usage; Claude cached input tokens are included; absent usage remains unknown. |
| M29 | `additive_deny_globs_match_paths_and_defaults_cannot_be_removed`, `user_local_policy_is_bounded_and_resolves_file_aliases_fail_closed` | User globs, aliases, malformed patterns, excessive size, nonprivate ownership/write permissions and a symlink policy file. Defaults cannot be removed. |
| M29 | `staged_alias_privacy_uses_the_repository_root_not_process_cwd` | Git diff paths resolve against the actual repository before canonical privacy checks, even when the editor's process cwd differs. |
| Trash | `trash_survives_restart_and_restore_never_overwrites_new_work`, `exclusive_rename_never_replaces_a_directory_or_dangling_symlink` | New destination work, directories and dangling symlinks survive attempted restore/rename. |
| Trash | `restore_refuses_changed_manifest_or_item_after_review`, `restore_refuses_a_replaced_original_parent` | A reviewed entry or original parent cannot silently change identity before restore. |
| Trash | `trash_manifest_never_follows_symlinks_or_reads_unbounded_data`, `trash_refuses_symlink_root_and_nonprivate_entries`, `trash_rejects_acl_access_even_when_mode_bits_are_private` | Manifests are bounded/non-symlink/regular/private; directories must be private and user owned. Darwin ACL grants are refused even when POSIX mode is 0700. |

## User-owned additive privacy policy

Optional file: `settings::config_root()/ai-deny.conf` (normally
`~/.config/mellow/ai-deny.conf`; XDG overrides apply). Missing file means the
built-in policy remains active. It is never loaded from the project.

Example:

```text
# Additional exclusions; defaults remain mandatory
*.private
private/**
**/customer?.json
```

One case-sensitive glob per line. Blank lines and lines starting with `#` are
ignored. `*` matches within one path component, `?` matches one character in a
component, `**` crosses directories, and `**/` can match zero directories.
Patterns match complete suffixes starting at a path-component boundary, against
both the displayed path and canonical existing target. Leading `/` is ignored;
there is no allow/negation rule. Use `/` separators. Bracket classes, brace
expansion, backslash escapes, control characters and leading `!` are unsupported
and cause refusal, rather than silently weakening the policy.

Limits: UTF-8 regular file, at most 64 KiB, 256 patterns, 1024 bytes per pattern.
On Unix it must be owned by the current user, not writable by group/others and
not a symlink. Read/parse/ownership failures refuse context before network use.
Git drafting checks staged paths relative to the repository root and checks the
full diff before truncating its displayed payload. Defaults and credential-like
text checks remain mandatory. This is a conservative deny policy, not proof that
all possible secrets or hard-link aliases can be identified.

## Transport, save and trust limits

HTTP redirects remain disabled for both transport families. Each request caps
its output-token request; total HTTP envelopes are limited to 512 KiB and parsed
text to 64,000 characters. SSE preview deltas are data only: a completed, valid
proposal still requires ordinary review/acceptance before a buffer edit. Local
transport cancellation drops pending HTTP futures with a 25 ms polling interval.
A system DNS worker may finish separately; runtime shutdown does not block on it.
Provider-side processing or billing may already have occurred.

Usage is exactly what the provider reports, including partial usage available
before errors. Missing input/output fields remain unknown. These observations
cannot establish a monetary ceiling, refund or provider compliance with a token
request. Parent-owned session request caps and post-response token budgets must
be qualified by their own app tests.

Safe save preserves supported metadata or refuses publication. Hard links are
refused with an independent Save As route. Save As requires exclusive publication;
filesystems without that support are refused. A bind-mount in-place fallback
retains a recovery copy but is not crash-atomic. The final conflict-check to rename
window cannot provide compare-and-swap serialization against an uncooperative
external writer. Process groups terminate cooperating descendants; a tool that
intentionally detaches into another session is not sandboxed.

Trash performs an exclusive same-filesystem move; cross-device moves fail with
the source retained, with no copy/delete fallback. Restore validates the reviewed
manifest/item and original parent identities and refuses existing destinations.
Directory/manifest trust checks are not a sandbox against an attacker already
running as the same OS user. Manifest corruption blocks listing with an explicit
error; the stored item is retained for manual repair.

The focused app review identified stale clipboard-focus/progress boundaries;
these were sent to the parent for implementation and regression coverage. Their
final disposition belongs in the overall tracker.

The outside-root execution fix is implemented in the formatter and language
service entry points. Workspace trust applies only when the existing target and
displayed parent both resolve within the canonical workspace. New Save As paths
require an existing canonical parent within it; unresolved/dangling aliases and
component-prefix siblings are refused. Outside files remain editable and save
through the core disk-save path, with formatting restricted. Active outside
documents stop the current language service and are excluded from multi-tab
synchronization. There is no persistent trust expansion or OS sandbox claim.

`execution_trust_does_not_follow_outside_files_or_symlink_aliases` covers a direct
outside Rust document and an in-root symlink to it: a dummy formatter's marker
is never created, no service document is opened, the edit is saved and the status
explains the restriction. The service assertions prove refusal before
`open_document`; they do not claim a packaged dummy-server process run.
`execution_trust_allows_new_save_as_inside_the_canonical_root` proves that a
new in-root Save As still invokes the dummy formatter and clears dirty state.
`execution_trust_refuses_unresolved_and_component_prefix_paths` checks sibling
prefixes, missing parents, dangling aliases, outside symlink parents and an
outside-parent symlink back into a trusted file. The existing rustfmt fixture
now declares its trusted workspace explicitly. These three tests and the five
existing format-on-save tests passed on the native host with pinned Rust 1.90.
Path checks cannot prevent a hostile same-user process from replacing paths
between validation and a tool launch, and trusted tools can still access
dependencies, global configuration and services beyond the workspace.

## Optional actual Linux file-mount gate

`saving_a_bind_mounted_file_rewrites_it_in_place` is now explicitly ignored in
ordinary Linux unit runs. The previous version returned success when mounting
was unavailable; the optional gate instead requires
`MELLOW_BIND_MOUNT_FILE=/fixture/mounted.txt` and an actual single-file Docker
bind mount with exactly `mellow-bind-mount-fixture: original\n` as its contents.
Use only a disposable synthetic host file: this test changes its bytes and mode.
Run the exact test with `--ignored --exact --nocapture` inside the Linux image.

The test directly observes replacement rename returning `EBUSY`, then checks
Mellow's `SaveMode::InPlace`, saved bytes, cleared dirty state and settled
conflict snapshot. Device, inode, owner, group and mode remain unchanged. User
xattrs are checked if the mounted filesystem supports them; unsupported xattrs
produce an explicit unexercised-branch message. Temporary save files must be
absent. This buffer-level gate does not exercise the app recovery journal.

Executed result: **1 passed, zero failed/ignored** on Linux
`7.0.12-linuxkit aarch64`, in Docker's Linux VM on the macOS ARM64 host. Rust
was `1.90.0 (1159e78c4 2025-09-14)`, host
`aarch64-unknown-linux-gnu`. Image
`mellow-audit-20261011-mellow-dev:latest` had ID
`sha256:9fa16bd142e0ce7d51d0345e260a3cc00cdc4cbdbd315a5b291e910c22463fcb`.
The mounted VirtioFS host file did **not** support user xattrs; the test printed
the exact unexercised-branch message. The preserved inode/mode checks and actual
`EBUSY` fallback ran, and a host-side read confirmed
`updated mellow-bind-mount-fixture: original\n` and mode `0640`.

A separate Linux VM `cargo test --locked --bin mellow buffer::tests --
--nocapture` run completed **41 passed, zero failed, 1 ignored**. The ignored
test was the explicit mount gate above, which passed in its separate invocation.
`atomic_save_preserves_linux_extended_attributes` passed on the container's
ordinary temporary filesystem; this does not fill the unsupported mounted
VirtioFS xattr branch.

The successful invocation used a disposable `mktemp` host file with the marker
above, mounted individually at `/fixture/mounted.txt`, and these command arguments:

```sh
docker run --rm \
  -e CARGO_HOME=/cargo -e CARGO_TARGET_DIR=/target \
  -e MELLOW_BIND_MOUNT_FILE=/fixture/mounted.txt \
  --mount type=bind,source="$PWD",target=/workspace \
  --mount type=bind,source="$mellow_mount_fixture",target=/fixture/mounted.txt \
  -v mellow-audit-20261011_mellow-cargo-registry:/cargo/registry \
  -v mellow-audit-20261011_mellow-cargo-git:/cargo/git \
  -v mellow-audit-20261011_mellow-target:/target \
  -w /workspace mellow-audit-20261011-mellow-dev:latest \
  cargo test --locked --bin mellow \
  buffer::tests::saving_a_bind_mounted_file_rewrites_it_in_place \
  -- --ignored --exact --nocapture
```

This verifies source behavior in an actual Linux-kernel file mount, not Linux
hardware, an extracted distribution executable or native Linux ACL behavior.

## Remaining qualification gates

- Exact packaged executable PTY/security regressions and executable/archive digests.
- Mounted VirtioFS user xattrs remain unsupported; normal Linux filesystem xattrs, actual POSIX ACL preservation and cross-owner refusal have separate final packaged VM evidence in DELIVERY.md.
- Native macOS Intel and Linux x64/ARM64 candidate gates.
- Cross-owner preservation where the current user cannot apply ownership; current tests exercised same owner plus supplementary group.
- Live AI accounts/provider compatibility and provider-side cancellation/billing behavior.
- Native signing/notarization and distribution install paths, governed by release authorization.
- Real terminal/SSH/tmux, glyph shaping and assistive-technology evaluation; source/PTY text tests cannot establish those claims.

## Final packaged Linux metadata evaluation

The accepted static musl ARM64 archive from clean `917238f` preserved a real
`system.posix_acl_access` ACL with named UID65534, owner/group/mode0640 and
content across atomic inode replacement in an 80×24 PTY. A child UID65534
then tried to save a root-owned0666 synthetic file in a writable directory:
Mellow refused ownership loss, retaining original bytes/inode/device/uid/gid/mode
and no temporary remnants. Explicit error wording was captured at240×24.
This executes Linux in Docker's ARM64 VM, not macOS or physical Linux hardware.
[Raw log](evidence/linux-vm-packaged-metadata.log) and
[reproducible probe](evidence/linux-vm-packaged-metadata-probe.py) retain exact
archive/binary digests, platform, observer corrections and final zero exit.
