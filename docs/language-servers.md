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
| JavaScript, TypeScript | typescript-language-server | `npm install -g typescript typescript-language-server` | `MELLOW_LSP_TYPESCRIPT` |
| Go | gopls | `go install golang.org/x/tools/gopls@latest` | `MELLOW_LSP_GO` |
| Terraform | terraform-ls | `brew install hashicorp/tap/terraform-ls` (or your package manager) | `MELLOW_LSP_TERRAFORM` |
| YAML | yaml-language-server | `npm install -g yaml-language-server` | `MELLOW_LSP_YAML` |
| JSON | vscode-json-language-server | `npm install -g vscode-langservers-extracted` | `MELLOW_LSP_JSON` |
| Shell | bash-language-server | `npm install -g bash-language-server` | `MELLOW_LSP_SHELL` |

The server must be on your `PATH`. To use a different program, set the variable,
for example `MELLOW_LSP_PYTHON=basedpyright-langserver`.

## What you see

- The status line shows whether the server is starting, ready or missing, and
  how to install a missing one.
- **Problems** (`Ctrl+P`, then "Show problems", or click "N problems" in the
  status line) lists syntax errors and server diagnostics together.
- Edits a server suggests across several files open as unsaved changes you can
  review and undo together; nothing is written to disk behind your back.
- A server that hangs or stops reading is stopped, so typing never freezes.

## Limits

Language servers are supported but not yet tested end to end against every live
server. Files over 5 MiB open in a reduced mode without parsing or servers, so
editing stays fast.
