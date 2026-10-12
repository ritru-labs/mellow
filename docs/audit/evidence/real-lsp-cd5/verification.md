# Actual packaged real-language UI qualification

Exact candidate: `cd5e74754db95abfd622a3ab7608588e424102b1`.
Extracted macOS ARM64 binary SHA256: `f9d1699a7d75d62a6a444b1bbfdbf4ac5f1b805a4865c3bede73da8ddb765d2f`.
Rust compiler receipt: 1.90.0, aarch64-apple-darwin.

Native platform:

```
ProductName:		macOS
ProductVersion:		26.6.2
BuildVersion:		25G83
```

Actual 80x24 PTY tests used real Pyright 1.1.414 and typescript-language-server 6.0.2 with TypeScript 5.9.3, Node v26.8.2. The executable came from the extracted archive, not cargo run or a source-test fixture. Explicit Trust workspace execution review was accepted through the keyboard for each isolated project. Each language passed completion autoimport acceptance, exact saved primary+import content, one undo restoring the original `audit_sy` buffer, redo restoring both edits, keyboard definition opening the helper, Navigation Back returning to main, bracketed-pasted undefined name shown in an actual Problems error list row (main.py 5:1 / main.ts 4:1), save, and actual Ctrl+Q clean exit. Both direct editor children were reaped with exit code 0; neither passing test used forced cleanup as exit evidence.

Evidence: results.json, provenance.json, each workspace/screens.txt and terminal.raw in this directory. Experiment log: `/private/tmp/mellow-ui-lsp-cd5-final-experiment.log`. Reproducible private harness: `/private/tmp/mellow-packaged-real-lsp-pty.py`.

This verifies real-server packaged keyboard UI on native macOS ARM64. It does not establish Intel macOS or Linux real-LSP UI validation, native screen-reader accessibility, live AI providers, signing, notarization, or remote SSH behavior.

The earlier pre-fix packaged completion refusal is retained at `/private/tmp/mellow-ui-lsp-757-attempt3`. The boundary edit regression failed before the source fix and passed afterward. An earlier private harness unconditionally signaled a reaped process group; it was fixed to mark children reaped and signal only unreaped owned children. An older native wait observation lacks contemporaneous wait-channel evidence and is unclassified; it is not being counted as a product defect or environment blocker. Fresh standalone Git root queries on these nonrepository fixtures returned the expected exit 128 in approximately 8 ms.
