# Language servers

Language servers give Mellow completion while you type, problems (errors and
warnings) and go to definition (`F12`). Mellow does not bundle them: install the
ones you need, and Mellow starts them when you open a matching file.

Syntax highlighting works without any server, for Rust, Python, JSON, Shell,
YAML and Terraform/HCL.

| Language | Server | Install | Use another program |
| --- | --- | --- | --- |
| Rust | rust-analyzer | `rustup component add rust-analyzer` | `MELLOW_LSP_RUST` |
| Python | Pyright | `npm install -g pyright` | `MELLOW_LSP_PYTHON` |
| JavaScript, TypeScript | typescript-language-server | `npm install -g typescript@5.9.3 typescript-language-server@6.0.2` | `MELLOW_LSP_TYPESCRIPT` |
| Go | gopls | `go install golang.org/x/tools/gopls@latest` | `MELLOW_LSP_GO` |
| Terraform | terraform-ls | `brew install hashicorp/tap/terraform-ls` (or your package manager) | `MELLOW_LSP_TERRAFORM` |
| YAML | yaml-language-server | `npm install -g yaml-language-server` | `MELLOW_LSP_YAML` |
| JSON | vscode-json-language-server | `npm install -g vscode-langservers-extracted` | `MELLOW_LSP_JSON` |
| Shell | bash-language-server | `npm install -g bash-language-server` | `MELLOW_LSP_SHELL` |

The server must be on your `PATH`. To use a different program, set the variable,
for example `MELLOW_LSP_PYTHON=basedpyright-langserver`.

The TypeScript versions above are the qualified pair. TypeScript 7.0.2 does
not provide the `tsserver.js` required by this server and failed initialization
in the audit qualification. An unpinned TypeScript installation can therefore
select an incompatible version.

## What you see

- The status line shows whether the server is starting, ready or missing, and
  how to install a missing one.
- **Problems** (`Ctrl+P`, then "Show problems", or click "N problems" in the
  status line) lists syntax errors and server diagnostics together.
- Edits a server suggests across several files open as unsaved changes you can
  review and undo together; nothing is written to disk behind your back.
- Completion can add supporting imports in the same undo step. Mellow resolves
  items when required, rejects stale or overlapping edits, and never executes
  a completion's provider command. Provider snippets remain unsupported;
  Mellow's local snippets are a separate feature.
- Services for different languages stay warm while their documents are open.
  Workspace Problems retain file names, and **Workspace symbols** searches the
  active language server's workspace. **Go to symbol** also has local outlines.
- A server that hangs or stops reading is stopped, so typing never freezes.

## Limits

Pyright 1.1.414 and typescript-language-server 6.0.2 with TypeScript 5.9.3 passed
native macOS ARM64 source/core-buffer and real stdio protocol tests for
completion imports, one-step undo/redo, definitions and diagnostics. These were
not packaged terminal UI tests or a full supported-server matrix. See the
[language verification evidence](audit/language-verification.md).

The real mixed-server source test keeps Pyright and TypeScript warm across four
alternating cycles, synchronizes inactive unsaved edits, checks aggregate
diagnostics and references for both file names, and finds an unsaved workspace
symbol through Pyright. Selecting these results in a packaged terminal remains
a separate acceptance gate.

Initialization establishes the protocol before indexing necessarily finishes.
If a completion fails during initial indexing, wait for analysis and request it
again; a failed resolution applies no edits. Files over 5 MiB use reduced
intelligence.
