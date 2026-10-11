use std::{
    collections::{HashMap, VecDeque},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
};

use anyhow::{Context, Result};
use serde_json::{Value, json};

const MAX_COMPLETIONS: usize = 200;
const MAX_DIAGNOSTICS: usize = 500;
const MAX_LOCATIONS: usize = 500;
const MAX_TEXT_EDITS: usize = 1000;
const MAX_HOVER_CHARS: usize = 16_000;
const MAX_SIGNATURES: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspPosition {
    pub line: usize,
    pub character: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDiagnostic {
    pub range: LspRange,
    pub severity: DiagnosticSeverity,
    pub source: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub filter_text: Option<String>,
    pub sort_text: Option<String>,
    pub insert_text: String,
    pub edit_range: Option<LspRange>,
    pub additional_text_edits: Vec<CompletionTextEdit>,
    /// Retained for disclosure only. Completion acceptance never executes commands.
    pub command: Option<Value>,
    /// Original opaque item including `data`, required by completionItem/resolve.
    pub raw: Value,
    pub needs_resolve: bool,
    pub rejected: Option<String>,
    /// Document identity/version at request time; supporting edits use this snapshot.
    pub origin: Option<(PathBuf, i32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionTextEdit {
    pub range: LspRange,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolLocation {
    pub name: String,
    pub detail: Option<String>,
    pub kind: u64,
    pub path: PathBuf,
    pub range: LspRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionLocation {
    pub path: PathBuf,
    pub range: LspRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspTextEdit {
    pub path: PathBuf,
    pub range: LspRange,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkspaceEditPreview {
    pub edits: Vec<LspTextEdit>,
    pub has_resource_operations: bool,
    /// Why the whole edit set was refused (over the limit, malformed or for
    /// an older document version). A refused set has no edits: Mellow never
    /// applies part of a change.
    pub rejected: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeActionItem {
    pub title: String,
    pub rejected: Option<String>,
    pub edits: Vec<LspTextEdit>,
    pub has_command: bool,
    pub has_resource_operations: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanguageServiceEvent {
    Ready(String),
    DiagnosticsChanged,
    Completions {
        request_id: u64,
        items: Vec<CompletionItem>,
    },
    CompletionResolved {
        request_id: u64,
        item: Box<CompletionItem>,
    },
    CompletionError {
        request_id: u64,
        error: String,
    },
    Symbols(Vec<SymbolLocation>),
    Definition(Option<DefinitionLocation>),
    Hover(Option<String>),
    SignatureHelp(Vec<String>),
    References(Vec<DefinitionLocation>),
    RenamePreview(WorkspaceEditPreview),
    Formatting(Vec<LspTextEdit>),
    CodeActions(Vec<CodeActionItem>),
    Exited(String),
    Error(String),
}

#[derive(Debug, Clone)]
struct ServerSpec {
    program: String,
    args: Vec<String>,
    language_id: &'static str,
    label: &'static str,
}

#[derive(Debug, Clone)]
struct DocumentDescriptor {
    uri: String,
    language_id: String,
    version: i32,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestKind {
    Initialize,
    Completion,
    CompletionResolve,
    DocumentSymbols,
    WorkspaceSymbols,
    Definition,
    Hover,
    SignatureHelp,
    References,
    Rename,
    Formatting,
    CodeAction,
}

enum Incoming {
    Message(Value),
    Closed,
}

/// Most bytes allowed to wait for a server that is not reading its input.
/// Typing coalesces into one pending full-text change, so only a server
/// that has stopped reading for a long time gets here.
const MAX_PENDING_BYTES: usize = 32 * 1024 * 1024;
/// Largest single message accepted from a server.
const MAX_INCOMING_BYTES: usize = 64 * 1024 * 1024;

/// One framed message waiting for the server.
struct Outgoing {
    frame: Vec<u8>,
    /// Set for a full-text `didChange`: a newer change to the same
    /// document may replace it while it is still the last one queued.
    replaceable_for: Option<String>,
}

#[derive(Default)]
struct OutgoingQueue {
    messages: VecDeque<Outgoing>,
    bytes: usize,
    closed: bool,
    failed: Option<String>,
}

/// Writes to the server on its own thread. Writing on the editor thread
/// froze Mellow, Quit included, as soon as a busy or hung server let its
/// input pipe fill (64 KiB: one didOpen of a mid-sized file).
struct LspWriter {
    shared: Arc<(Mutex<OutgoingQueue>, Condvar)>,
}

impl LspWriter {
    fn start(mut sink: impl Write + Send + 'static, name: String) -> Result<Self> {
        let shared = Arc::new((Mutex::new(OutgoingQueue::default()), Condvar::new()));
        let queue = Arc::clone(&shared);
        thread::Builder::new()
            .name(name)
            .spawn(move || {
                let (lock, ready) = &*queue;
                loop {
                    let message = {
                        let mut state = lock.lock().unwrap_or_else(|poison| poison.into_inner());
                        while state.messages.is_empty() && !state.closed {
                            state = ready
                                .wait(state)
                                .unwrap_or_else(|poison| poison.into_inner());
                        }
                        let Some(message) = state.messages.pop_front() else {
                            break;
                        };
                        state.bytes -= message.frame.len();
                        message
                    };
                    if let Err(error) = sink.write_all(&message.frame).and_then(|()| sink.flush()) {
                        let mut state = lock.lock().unwrap_or_else(|poison| poison.into_inner());
                        state.failed = Some(format!("language server input closed: {error}"));
                        state.messages.clear();
                        state.bytes = 0;
                        break;
                    }
                }
            })
            .context("failed to start language server writer thread")?;
        Ok(Self { shared })
    }

    /// Queues a message without waiting for the server to read it.
    fn send(&self, frame: Vec<u8>, replaceable_for: Option<String>) -> Result<()> {
        let (lock, ready) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(reason) = &state.failed {
            anyhow::bail!("{reason}");
        }
        let replace = replaceable_for.is_some()
            && state
                .messages
                .back()
                .is_some_and(|last| last.replaceable_for == replaceable_for);
        if replace && let Some(last) = state.messages.pop_back() {
            state.bytes -= last.frame.len();
        }
        state.bytes += frame.len();
        state.messages.push_back(Outgoing {
            frame,
            replaceable_for,
        });
        if state.bytes > MAX_PENDING_BYTES {
            let reason = "language server stopped reading its input; it was stopped so \
                          editing stays responsive"
                .to_owned();
            state.failed = Some(reason.clone());
            state.messages.clear();
            state.bytes = 0;
            anyhow::bail!("{reason}");
        }
        ready.notify_one();
        Ok(())
    }

    /// Why the server can no longer be written to, if it cannot.
    fn failure(&self) -> Option<String> {
        let (lock, _) = &*self.shared;
        lock.lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .failed
            .clone()
    }

    #[cfg(test)]
    fn pending_bytes(&self) -> usize {
        let (lock, _) = &*self.shared;
        lock.lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .bytes
    }
}

impl Drop for LspWriter {
    fn drop(&mut self) {
        let (lock, ready) = &*self.shared;
        lock.lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .closed = true;
        ready.notify_one();
    }
}

struct LspClient {
    child: Child,
    writer: LspWriter,
    incoming: Receiver<Incoming>,
    pending: HashMap<u64, RequestKind>,
    /// Document (uri, version) each edit-producing request (format, rename,
    /// code action) was made for. Its edits are only valid for that text.
    edit_targets: HashMap<u64, (String, i32)>,
    completion_prefixes: HashMap<u64, String>,
    workspace_edit_versions: HashMap<u64, HashMap<String, i32>>,
    next_id: u64,
    initialized: bool,
    server_label: String,
    server_program: String,
    server_args: Vec<String>,
    opened_documents: HashMap<String, (i32, String)>,
    completion_trigger_characters: Vec<String>,
    completion_resolve: bool,
    workspace_root: PathBuf,
    queued_documents: HashMap<String, DocumentDescriptor>,
    /// Features the server announced (completion, rename, ...), for the
    /// health view.
    capabilities: Vec<&'static str>,
    /// Sent at initialize and again when the server asks for them.
    workspace_folders: Value,
    document: DocumentDescriptor,
}

impl LspClient {
    fn spawn(
        spec: &ServerSpec,
        workspace_root: &Path,
        document_path: &Path,
        text: &str,
        version: i32,
    ) -> Result<Self> {
        let mut child = ProcessCommand::new(&spec.program)
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to start {}", spec.label))?;

        let stdin = child
            .stdin
            .take()
            .context("language server stdin was not available")?;
        let writer = LspWriter::start(stdin, format!("mellow-lsp-write-{}", spec.language_id))?;
        let stdout = child
            .stdout
            .take()
            .context("language server stdout was not available")?;
        let (tx, rx) = mpsc::channel();

        thread::Builder::new()
            .name(format!("mellow-lsp-{}", spec.language_id))
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    match read_lsp_message(&mut reader) {
                        Ok(Some(message)) => {
                            if tx.send(Incoming::Message(message)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => {
                            let _ = tx.send(Incoming::Closed);
                            break;
                        }
                        Err(_) => {
                            let _ = tx.send(Incoming::Closed);
                            break;
                        }
                    }
                }
            })
            .context("failed to start language server reader thread")?;

        let document = DocumentDescriptor {
            uri: path_to_uri(document_path),
            language_id: spec.language_id.to_owned(),
            version,
            text: text.to_owned(),
        };

        let workspace_folders = json!([{
            "uri": path_to_uri(workspace_root),
            "name": workspace_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("workspace")
        }]);
        let mut client = Self {
            child,
            writer,
            incoming: rx,
            pending: HashMap::new(),
            edit_targets: HashMap::new(),
            completion_prefixes: HashMap::new(),
            workspace_edit_versions: HashMap::new(),
            next_id: 1,
            initialized: false,
            server_label: spec.label.to_owned(),
            server_program: spec.program.clone(),
            server_args: spec.args.clone(),
            opened_documents: HashMap::new(),
            completion_trigger_characters: Vec::new(),
            completion_resolve: false,
            workspace_root: std::fs::canonicalize(workspace_root)
                .unwrap_or_else(|_| workspace_root.to_path_buf()),
            queued_documents: HashMap::from([(document.uri.clone(), document.clone())]),
            capabilities: Vec::new(),
            workspace_folders: workspace_folders.clone(),
            document,
        };

        let initialize_params = json!({
            "processId": std::process::id(),
            "clientInfo": {
                "name": "Mellow",
                "version": env!("CARGO_PKG_VERSION")
            },
            "rootUri": path_to_uri(workspace_root),
            "capabilities": {
                "workspace": {
                    "workspaceFolders": true,
                    "configuration": true,
                    "symbol": { "dynamicRegistration": false }
                },
                "textDocument": {
                    "synchronization": {
                        "dynamicRegistration": false,
                        "willSave": false,
                        "didSave": true
                    },
                    "completion": {
                        "completionItem": {
                            "snippetSupport": false,
                            "documentationFormat": ["plaintext", "markdown"],
                            "resolveSupport": { "properties": ["additionalTextEdits", "command", "detail", "documentation"] }
                        }
                    },
                    "documentSymbol": { "dynamicRegistration": false, "hierarchicalDocumentSymbolSupport": true },
                    "definition": { "dynamicRegistration": false, "linkSupport": true },
                    "hover": { "dynamicRegistration": false, "contentFormat": ["markdown", "plaintext"] },
                    "signatureHelp": { "dynamicRegistration": false, "signatureInformation": { "documentationFormat": ["markdown", "plaintext"] } },
                    "references": { "dynamicRegistration": false },
                    "rename": { "dynamicRegistration": false, "prepareSupport": false },
                    "formatting": { "dynamicRegistration": false },
                    "codeAction": { "dynamicRegistration": false, "codeActionLiteralSupport": { "codeActionKind": { "valueSet": ["", "quickfix", "refactor", "source"] } } },
                    "publishDiagnostics": {
                        "relatedInformation": false
                    }
                }
            },
            "workspaceFolders": workspace_folders
        });
        client.send_request(RequestKind::Initialize, "initialize", initialize_params)?;
        Ok(client)
    }

    fn send_request(&mut self, kind: RequestKind, method: &str, params: Value) -> Result<u64> {
        // Position/version checks do not distinguish two queries made against
        // the same unchanged document. Only the latest request for an action
        // may update its panel or navigate, even when responses arrive reversed.
        if kind != RequestKind::Initialize {
            let superseded: Vec<_> = self
                .pending
                .iter()
                .filter_map(|(id, pending)| {
                    // Both symbol scopes share one panel and response event.
                    // Switching scope also supersedes the other scope's query.
                    (*pending == kind
                        || matches!(
                            (kind, *pending),
                            (RequestKind::DocumentSymbols, RequestKind::WorkspaceSymbols)
                                | (RequestKind::WorkspaceSymbols, RequestKind::DocumentSymbols)
                        ))
                    .then_some(*id)
                })
                .collect();
            for id in superseded {
                self.cancel_request(id);
            }
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.pending.insert(id, kind);
        self.send_value(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;
        Ok(id)
    }

    fn cancel_request(&mut self, id: u64) {
        self.pending.remove(&id);
        self.edit_targets.remove(&id);
        self.completion_prefixes.remove(&id);
        self.workspace_edit_versions.remove(&id);
        let _ = self.send_notification("$/cancelRequest", json!({"id":id}));
    }

    fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        self.send_value(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
    }

    fn send_value(&mut self, value: &Value) -> Result<()> {
        self.queue_value(value, None)
    }

    fn queue_value(&mut self, value: &Value, replaceable_for: Option<String>) -> Result<()> {
        let body = serde_json::to_vec(value).context("failed to encode language server message")?;
        let mut frame = Vec::with_capacity(body.len() + 32);
        write_lsp_frame(&mut frame, &body).context("failed to frame language server message")?;
        self.writer.send(frame, replaceable_for)
    }

    fn send_server_response(&mut self, id: Value, result: Value) -> Result<()> {
        self.send_value(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        }))
    }

    fn matches_server(&self, spec: &ServerSpec) -> bool {
        self.server_program == spec.program && self.server_args == spec.args
    }

    fn did_open(&mut self) -> Result<()> {
        self.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": self.document.uri,
                    "languageId": self.document.language_id,
                    "version": self.document.version,
                    "text": self.document.text
                }
            }),
        )?;
        self.opened_documents.insert(
            self.document.uri.clone(),
            (self.document.version, self.document.text.clone()),
        );
        Ok(())
    }

    fn activate_document(
        &mut self,
        language_id: &str,
        document_path: &Path,
        text: &str,
        version: i32,
    ) -> Result<()> {
        let uri = path_to_uri(document_path);
        let previous = self.opened_documents.get(&uri).cloned();
        self.document = DocumentDescriptor {
            uri,
            language_id: language_id.to_owned(),
            version,
            text: text.to_owned(),
        };
        if !self.initialized {
            self.queued_documents
                .insert(self.document.uri.clone(), self.document.clone());
            return Ok(());
        }
        match previous {
            None => self.did_open(),
            Some((previous_version, previous_text))
                if previous_version != version || previous_text != text =>
            {
                self.did_change(text, version)
            }
            Some(_) => Ok(()),
        }
    }

    fn did_change(&mut self, text: &str, version: i32) -> Result<()> {
        self.document.text = text.to_owned();
        self.document.version = version;
        if !self.initialized {
            self.queued_documents
                .insert(self.document.uri.clone(), self.document.clone());
            return Ok(());
        }

        // Full-text changes: a newer one may replace an older one the
        // server has not read yet (versions only need to increase).
        let uri = self.document.uri.clone();
        self.queue_value(
            &json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": {
                    "textDocument": {
                        "uri": self.document.uri,
                        "version": version
                    },
                    "contentChanges": [{
                        "text": text
                    }]
                }
            }),
            Some(uri),
        )?;
        self.opened_documents
            .insert(self.document.uri.clone(), (version, text.to_owned()));
        Ok(())
    }

    fn request_completion(
        &mut self,
        position: LspPosition,
        trigger_character: Option<&str>,
    ) -> Result<Option<u64>> {
        if !self.initialized {
            return Ok(None);
        }

        let context = if let Some(trigger_character) = trigger_character {
            json!({
                "triggerKind": 2,
                "triggerCharacter": trigger_character
            })
        } else {
            json!({
                "triggerKind": 1
            })
        };

        let id = self.send_request(
            RequestKind::Completion,
            "textDocument/completion",
            json!({
                "textDocument": {
                    "uri": self.document.uri
                },
                "position": position_json(position),
                "context": context
            }),
        )?;
        self.remember_edit_target(id);
        let line = self.document.text.lines().nth(position.line).unwrap_or("");
        let mut utf16 = 0;
        let before: String = line
            .chars()
            .take_while(|ch| {
                utf16 += ch.len_utf16();
                utf16 <= position.character
            })
            .collect();
        let prefix: String = before
            .chars()
            .rev()
            .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        self.completion_prefixes.insert(id, prefix);
        Ok(Some(id))
    }

    fn request_completion_resolve(&mut self, item: &CompletionItem) -> Result<Option<u64>> {
        if !self.initialized || !item.needs_resolve {
            return Ok(None);
        }
        let current = (uri_to_path(&self.document.uri), self.document.version);
        if !item.origin.as_ref().is_some_and(|(path, version)| {
            current.0.as_ref() == Some(path) && current.1 == *version
        }) {
            anyhow::bail!("Completion expired: the document changed; request completion again");
        }
        let id = self.send_request(
            RequestKind::CompletionResolve,
            "completionItem/resolve",
            item.raw.clone(),
        )?;
        self.remember_edit_target(id);
        Ok(Some(id))
    }

    fn request_document_symbols(&mut self) -> Result<bool> {
        if !self.initialized {
            return Ok(false);
        }
        let id = self.send_request(
            RequestKind::DocumentSymbols,
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": self.document.uri } }),
        )?;
        self.remember_edit_target(id);
        Ok(true)
    }

    fn request_workspace_symbols(&mut self, query: &str) -> Result<bool> {
        if !self.initialized {
            return Ok(false);
        }
        let id = self.send_request(
            RequestKind::WorkspaceSymbols,
            "workspace/symbol",
            json!({ "query": query }),
        )?;
        self.remember_edit_target(id);
        Ok(true)
    }

    fn request_definition(&mut self, position: LspPosition) -> Result<bool> {
        if !self.initialized {
            return Ok(false);
        }
        self.send_position_request(RequestKind::Definition, "textDocument/definition", position)?;
        Ok(true)
    }

    fn send_position_request(
        &mut self,
        kind: RequestKind,
        method: &str,
        position: LspPosition,
    ) -> Result<u64> {
        let id = self.send_request(
            kind,
            method,
            json!({
                "textDocument": { "uri": self.document.uri },
                "position": position_json(position)
            }),
        )?;
        self.remember_edit_target(id);
        Ok(id)
    }

    fn request_hover(&mut self, position: LspPosition) -> Result<bool> {
        if !self.initialized {
            return Ok(false);
        }
        self.send_position_request(RequestKind::Hover, "textDocument/hover", position)?;
        Ok(true)
    }

    fn request_signature_help(&mut self, position: LspPosition) -> Result<bool> {
        if !self.initialized {
            return Ok(false);
        }
        self.send_position_request(
            RequestKind::SignatureHelp,
            "textDocument/signatureHelp",
            position,
        )?;
        Ok(true)
    }

    fn request_references(&mut self, position: LspPosition) -> Result<bool> {
        if !self.initialized {
            return Ok(false);
        }
        let id = self.send_request(
            RequestKind::References,
            "textDocument/references",
            json!({
                "textDocument": { "uri": self.document.uri }, "position": position_json(position),
                "context": { "includeDeclaration": true }
            }),
        )?;
        self.remember_edit_target(id);
        Ok(true)
    }

    fn request_rename(&mut self, position: LspPosition, new_name: &str) -> Result<bool> {
        if !self.initialized {
            return Ok(false);
        }
        let id = self.send_request(RequestKind::Rename, "textDocument/rename", json!({
            "textDocument": { "uri": self.document.uri }, "position": position_json(position), "newName": new_name
        }))?;
        self.remember_edit_target(id);
        Ok(true)
    }

    fn request_formatting(&mut self, tab_size: usize, insert_spaces: bool) -> Result<bool> {
        if !self.initialized {
            return Ok(false);
        }
        let id = self.send_request(
            RequestKind::Formatting,
            "textDocument/formatting",
            json!({
                "textDocument": { "uri": self.document.uri },
                "options": { "tabSize": tab_size, "insertSpaces": insert_spaces }
            }),
        )?;
        self.remember_edit_target(id);
        Ok(true)
    }

    fn request_code_actions(&mut self, range: LspRange) -> Result<bool> {
        if !self.initialized {
            return Ok(false);
        }
        let id = self.send_request(RequestKind::CodeAction, "textDocument/codeAction", json!({
            "textDocument": { "uri": self.document.uri }, "range": range_json(range), "context": { "diagnostics": [] }
        }))?;
        self.remember_edit_target(id);
        Ok(true)
    }

    fn remember_edit_target(&mut self, id: u64) {
        if matches!(
            self.pending.get(&id),
            Some(RequestKind::Rename | RequestKind::CodeAction)
        ) {
            self.workspace_edit_versions.insert(
                id,
                self.opened_documents
                    .iter()
                    .map(|(uri, (version, _))| (uri.clone(), *version))
                    .collect(),
            );
        }
        self.edit_targets
            .insert(id, (self.document.uri.clone(), self.document.version));
    }

    fn close_path(&mut self, path: &Path) -> Result<()> {
        let uri = path_to_uri(path);
        // A reopened buffer can reset its revision. Forget requests for the
        // previous document lifetime even if its URI/version later match again.
        let ids: Vec<_> = self
            .edit_targets
            .iter()
            .filter_map(|(id, (target, _))| {
                (target == &uri
                    || self
                        .workspace_edit_versions
                        .get(id)
                        .is_some_and(|versions| versions.contains_key(&uri)))
                .then_some(*id)
            })
            .collect();
        for id in ids {
            self.cancel_request(id);
        }
        self.queued_documents.remove(&uri);
        if self.opened_documents.remove(&uri).is_none() || !self.initialized {
            return Ok(());
        }
        self.send_notification(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        )
    }

    fn close_documents(&mut self) {
        if !self.initialized {
            self.opened_documents.clear();
            return;
        }
        let uris: Vec<String> = self.opened_documents.keys().cloned().collect();
        self.opened_documents.clear();
        for uri in uris {
            let _ = self.send_notification(
                "textDocument/didClose",
                json!({ "textDocument": { "uri": uri } }),
            );
        }
    }

    fn poll(&mut self) -> Vec<ClientEvent> {
        let mut events = Vec::new();
        if let Some(reason) = self.writer.failure() {
            // Exited first so the status line ends on the reason.
            events.push(ClientEvent::Exited);
            events.push(ClientEvent::Error(reason));
            return events;
        }
        loop {
            match self.incoming.try_recv() {
                Ok(Incoming::Message(message)) => {
                    self.handle_message(message, &mut events);
                }
                Ok(Incoming::Closed) => {
                    events.push(ClientEvent::Exited);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    events.push(ClientEvent::Exited);
                    break;
                }
            }
        }
        events
    }

    fn handle_message(&mut self, message: Value, events: &mut Vec<ClientEvent>) {
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            if let Some(id) = message.get("id").cloned() {
                let result = match method {
                    "workspace/configuration" => {
                        let count = message
                            .pointer("/params/items")
                            .and_then(Value::as_array)
                            .map(Vec::len)
                            .unwrap_or(0);
                        Value::Array(vec![Value::Null; count])
                    }
                    // The same folders as at initialize: an empty list told
                    // the server the project had been closed.
                    "workspace/workspaceFolders" => self.workspace_folders.clone(),
                    "workspace/applyEdit" => json!({
                        "applied": false,
                        "failureReason": "Mellow requires an explicit user review before applying workspace edits"
                    }),
                    "client/registerCapability"
                    | "client/unregisterCapability"
                    | "window/workDoneProgress/create" => Value::Null,
                    _ => Value::Null,
                };
                let _ = self.send_server_response(id, result);
                return;
            }

            if method == "textDocument/publishDiagnostics"
                && let Some(uri) = message.pointer("/params/uri").and_then(Value::as_str)
                && uri_to_path(uri).is_some_and(|path| path.starts_with(&self.workspace_root))
                && !message
                    .pointer("/params/version")
                    .and_then(Value::as_i64)
                    .is_some_and(|version| {
                        self.opened_documents
                            .get(uri)
                            .is_some_and(|(current, _)| i64::from(*current) != version)
                    })
            {
                events.push(ClientEvent::Diagnostics(
                    uri.to_owned(),
                    parse_diagnostics(&message),
                ));
            }
            return;
        }

        let Some(id) = message.get("id").and_then(Value::as_u64) else {
            return;
        };
        let Some(kind) = self.pending.remove(&id) else {
            return;
        };
        let edit_target = self.edit_targets.remove(&id);
        let workspace_edit_versions = self.workspace_edit_versions.remove(&id).unwrap_or_default();
        let completion_prefix = self.completion_prefixes.remove(&id).unwrap_or_default();
        let target_is_current = edit_target.as_ref().is_some_and(|(uri, version)| {
            *uri == self.document.uri && *version == self.document.version
        });
        let active = Some((self.document.uri.as_str(), self.document.version));

        if let Some(error) = message.get("error") {
            let reason = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("language server request failed")
                .to_owned();
            if matches!(
                kind,
                RequestKind::Completion | RequestKind::CompletionResolve
            ) {
                events.push(ClientEvent::CompletionError(id, reason));
            } else {
                events.push(ClientEvent::Error(reason));
            }
            return;
        }

        match kind {
            RequestKind::Initialize => {
                self.completion_trigger_characters = message
                    .pointer("/result/capabilities/completionProvider/triggerCharacters")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                self.capabilities = announced_capabilities(
                    message
                        .pointer("/result/capabilities")
                        .unwrap_or(&Value::Null),
                );
                self.completion_resolve = message
                    .pointer("/result/capabilities/completionProvider/resolveProvider")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.initialized = true;
                let active_document = self.document.clone();
                let queued = std::mem::take(&mut self.queued_documents);
                let mut initialization_failed =
                    self.send_notification("initialized", json!({})).is_err();
                for document in queued.into_values() {
                    self.document = document;
                    initialization_failed |= self.did_open().is_err();
                }
                self.document = active_document;
                if initialization_failed {
                    events.push(ClientEvent::Error(
                        "failed to finish language server initialization".to_owned(),
                    ));
                } else {
                    events.push(ClientEvent::Ready);
                }
            }
            RequestKind::Completion | RequestKind::CompletionResolve if !target_is_current => {
                events.push(ClientEvent::CompletionError(
                    id,
                    "Completion expired: the file changed or you switched files".to_owned(),
                ));
            }
            RequestKind::Completion | RequestKind::CompletionResolve => {
                let result = message.get("result").unwrap_or(&Value::Null);
                let result = if kind == RequestKind::CompletionResolve {
                    json!([result])
                } else if !completion_prefix.is_empty() {
                    // Some servers send their whole global table. Filter before
                    // the result cap so imports after the first 200 entries survive.
                    let prefix = completion_prefix.to_lowercase();
                    let candidates = result
                        .as_array()
                        .or_else(|| result.get("items").and_then(Value::as_array));
                    Value::Array(
                        candidates
                            .into_iter()
                            .flatten()
                            .filter(|item| {
                                item.get("filterText")
                                    .or_else(|| item.get("label"))
                                    .and_then(Value::as_str)
                                    .is_some_and(|label| label.to_lowercase().starts_with(&prefix))
                            })
                            .cloned()
                            .collect(),
                    )
                } else {
                    result.clone()
                };
                let mut items = parse_completion_result(&result);
                for item in &mut items {
                    item.origin = edit_target
                        .as_ref()
                        .and_then(|(uri, version)| uri_to_path(uri).map(|path| (path, *version)));
                    item.needs_resolve = kind == RequestKind::Completion && self.completion_resolve;
                }
                if kind == RequestKind::CompletionResolve {
                    if let Some(item) = items.into_iter().next() {
                        events.push(ClientEvent::CompletionResolved(id, Box::new(item)));
                    } else {
                        events.push(ClientEvent::CompletionError(
                            id,
                            "Server returned an invalid resolved completion".to_owned(),
                        ));
                    }
                } else {
                    events.push(ClientEvent::Completions(id, items));
                }
            }
            RequestKind::DocumentSymbols | RequestKind::WorkspaceSymbols if !target_is_current => {
                events.push(ClientEvent::Error(
                    "Symbols discarded: the document changed or you switched files".to_owned(),
                ));
            }
            RequestKind::DocumentSymbols | RequestKind::WorkspaceSymbols => {
                events.push(ClientEvent::Symbols(parse_symbols(
                    message.get("result").unwrap_or(&Value::Null),
                    &self.document.uri,
                )))
            }
            RequestKind::Definition
            | RequestKind::Hover
            | RequestKind::SignatureHelp
            | RequestKind::References
                if !target_is_current =>
            {
                events.push(ClientEvent::Error(
                    "Language server result discarded: the document changed or you switched files"
                        .to_owned(),
                ))
            }
            RequestKind::Definition => events.push(ClientEvent::Definition(
                parse_definition_result(message.get("result").unwrap_or(&Value::Null)),
            )),
            RequestKind::Hover => events.push(ClientEvent::Hover(parse_hover_result(
                message.get("result").unwrap_or(&Value::Null),
            ))),
            RequestKind::SignatureHelp => events.push(ClientEvent::SignatureHelp(
                parse_signature_result(message.get("result").unwrap_or(&Value::Null)),
            )),
            RequestKind::References => events.push(ClientEvent::References(parse_locations(
                message.get("result").unwrap_or(&Value::Null),
            ))),
            RequestKind::Rename if !target_is_current => events.push(ClientEvent::Error(
                "Rename result discarded: the file changed or you switched files before it \
                 arrived. Rename again."
                    .to_owned(),
            )),
            RequestKind::Rename => events.push(ClientEvent::Rename(parse_workspace_edit_checked(
                message.get("result").unwrap_or(&Value::Null),
                active,
                &self.opened_documents,
                &workspace_edit_versions,
            ))),
            RequestKind::Formatting => {
                // Edits are positions in the text that was formatted. Apply
                // them only if that document is still active and unchanged;
                // never re-target them at whatever is open now.
                match edit_target {
                    Some((uri, _)) if target_is_current => {
                        match parse_document_edits(
                            &uri,
                            message.get("result").unwrap_or(&Value::Null),
                        ) {
                            Ok(edits) => events.push(ClientEvent::Formatting(edits)),
                            Err(reason) => events.push(ClientEvent::Error(format!(
                                "Formatting refused: {reason}; nothing was changed"
                            ))),
                        }
                    }
                    _ => events.push(ClientEvent::Error(
                        "Formatting result discarded: the file changed or you switched files \
                         before it arrived. Run Format again."
                            .to_owned(),
                    )),
                }
            }
            RequestKind::CodeAction if !target_is_current => events.push(ClientEvent::Error(
                "Code actions discarded: the file changed or you switched files before they \
                 arrived. Ask again."
                    .to_owned(),
            )),
            RequestKind::CodeAction => {
                events.push(ClientEvent::CodeActions(parse_code_actions_checked(
                    message.get("result").unwrap_or(&Value::Null),
                    active,
                    &self.opened_documents,
                    &workspace_edit_versions,
                )))
            }
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.close_documents();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

enum ClientEvent {
    Ready,
    Diagnostics(String, Vec<LspDiagnostic>),
    Completions(u64, Vec<CompletionItem>),
    CompletionResolved(u64, Box<CompletionItem>),
    CompletionError(u64, String),
    Symbols(Vec<SymbolLocation>),
    Definition(Option<DefinitionLocation>),
    Hover(Option<String>),
    SignatureHelp(Vec<String>),
    References(Vec<DefinitionLocation>),
    Rename(WorkspaceEditPreview),
    Formatting(Vec<LspTextEdit>),
    CodeActions(Vec<CodeActionItem>),
    Error(String),
    Exited,
}

#[derive(Default)]
pub struct LanguageService {
    client: Option<LspClient>,
    /// Inactive language services stay warm with their unsaved open documents.
    pooled_clients: Vec<LspClient>,
    current_path: Option<PathBuf>,
    current_language: String,
    diagnostics_by_path: HashMap<PathBuf, Vec<LspDiagnostic>>,
    last_error: Option<String>,
}

impl LanguageService {
    pub fn open_document(
        &mut self,
        workspace_root: &Path,
        path: Option<&Path>,
        language_name: &str,
        text: &str,
        revision: u64,
    ) {
        self.current_language = language_name.to_owned();
        self.current_path = path.map(Path::to_path_buf);
        self.last_error = None;

        let (Some(path), Some(spec)) = (path, server_spec(language_name)) else {
            if let Some(client) = self.client.take() {
                self.pooled_clients.push(client);
            }
            return;
        };
        self.activate_with_spec(workspace_root, path, &spec, text, revision);
    }

    fn activate_with_spec(
        &mut self,
        workspace_root: &Path,
        path: &Path,
        spec: &ServerSpec,
        text: &str,
        revision: u64,
    ) {
        let workspace =
            std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
        let matches =
            |client: &LspClient| client.matches_server(spec) && client.workspace_root == workspace;
        if !self.client.as_ref().is_some_and(matches) {
            if let Some(client) = self.client.take() {
                self.pooled_clients.push(client);
            }
            if let Some(index) = self.pooled_clients.iter().position(matches) {
                self.client = Some(self.pooled_clients.remove(index));
            }
        }
        let version = version_from_revision(revision);
        if let Some(client) = self.client.as_mut() {
            if let Err(error) = client.activate_document(spec.language_id, path, text, version) {
                self.last_error = Some(format!(
                    "{} document activation failed: {error}",
                    spec.label
                ));
            }
            return;
        }
        // Evict only an idle service; never discard an open unsaved document
        // merely because another language becomes active.
        const MAX_POOLED_SERVERS: usize = 8;
        if self.pooled_clients.len() >= MAX_POOLED_SERVERS {
            if let Some(index) = self.pooled_clients.iter().position(|client| {
                client.opened_documents.is_empty() && client.queued_documents.is_empty()
            }) {
                self.pooled_clients.remove(index);
            } else {
                self.last_error = Some("Eight language services have open files; close files before starting another service".to_owned());
                return;
            }
        }
        match LspClient::spawn(spec, workspace_root, path, text, version) {
            Ok(client) => self.client = Some(client),
            Err(error) => self.last_error = Some(format!("{} unavailable: {error}", spec.label)),
        }
    }

    pub fn sync_document(&mut self, path: Option<&Path>, text: &str, revision: u64) {
        let Some(path) = path else {
            return;
        };
        let uri = path_to_uri(path);
        let version = version_from_revision(revision);
        for client in self.client.iter_mut().chain(self.pooled_clients.iter_mut()) {
            if client.document.uri != uri
                && !client.opened_documents.contains_key(&uri)
                && !client.queued_documents.contains_key(&uri)
            {
                continue;
            }
            // Coordinated edits synchronize every open tab, including tabs
            // that did not change. Do not send duplicate-version didChange
            // messages for those documents or invalidate server analysis.
            if client
                .opened_documents
                .get(&uri)
                .is_some_and(|(previous_version, previous_text)| {
                    *previous_version == version && previous_text == text
                })
                || client
                    .queued_documents
                    .get(&uri)
                    .is_some_and(|document| document.version == version && document.text == text)
            {
                continue;
            }
            let previous_active = client.document.clone();
            if client.document.uri != uri {
                client.document = client
                    .queued_documents
                    .get(&uri)
                    .cloned()
                    .unwrap_or_else(|| DocumentDescriptor {
                        uri: uri.clone(),
                        language_id: previous_active.language_id.clone(),
                        version,
                        text: text.to_owned(),
                    });
            }
            let result = client.did_change(text, version);
            if previous_active.uri != uri {
                client.document = previous_active;
            }
            if let Err(error) = result {
                self.last_error = Some(format!("language server sync failed: {error}"));
            }
        }
    }

    pub fn close_document(&mut self, path: Option<&Path>) {
        let Some(path) = path else {
            return;
        };
        for client in self.client.iter_mut().chain(self.pooled_clients.iter_mut()) {
            if let Err(error) = client.close_path(path) {
                self.last_error = Some(format!("language server close failed: {error}"));
            }
        }
        let identity = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.diagnostics_by_path.retain(|candidate, _| {
            std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.clone()) != identity
        });
    }

    pub fn request_completion(
        &mut self,
        position: LspPosition,
        trigger_character: Option<&str>,
    ) -> Result<Option<u64>> {
        let Some(client) = self.client.as_mut() else {
            return Ok(None);
        };
        client.request_completion(position, trigger_character)
    }

    pub fn request_completion_resolve(&mut self, item: &CompletionItem) -> Result<Option<u64>> {
        let Some(client) = self.client.as_mut() else {
            return Ok(None);
        };
        client.request_completion_resolve(item)
    }

    pub fn completion_trigger_characters(&self) -> &[String] {
        self.client
            .as_ref()
            .map(|client| client.completion_trigger_characters.as_slice())
            .unwrap_or(&[])
    }

    pub fn request_document_symbols(&mut self) -> Result<bool> {
        self.client
            .as_mut()
            .map_or(Ok(false), LspClient::request_document_symbols)
    }

    pub fn request_workspace_symbols(&mut self, query: &str) -> Result<bool> {
        self.client
            .as_mut()
            .map_or(Ok(false), |client| client.request_workspace_symbols(query))
    }

    pub fn request_definition(&mut self, position: LspPosition) -> Result<bool> {
        let Some(client) = self.client.as_mut() else {
            return Ok(false);
        };
        client.request_definition(position)
    }

    pub fn cancel_interactions(&mut self) {
        for client in self.client.iter_mut().chain(self.pooled_clients.iter_mut()) {
            let ids: Vec<_> = client
                .pending
                .iter()
                .filter_map(|(id, kind)| {
                    matches!(
                        kind,
                        RequestKind::Definition
                            | RequestKind::References
                            | RequestKind::Hover
                            | RequestKind::SignatureHelp
                    )
                    .then_some(*id)
                })
                .collect();
            for id in ids {
                client.cancel_request(id);
            }
        }
    }

    pub fn request_hover(&mut self, position: LspPosition) -> Result<bool> {
        let Some(client) = self.client.as_mut() else {
            return Ok(false);
        };
        client.request_hover(position)
    }

    pub fn request_signature_help(&mut self, position: LspPosition) -> Result<bool> {
        let Some(client) = self.client.as_mut() else {
            return Ok(false);
        };
        client.request_signature_help(position)
    }

    pub fn request_references(&mut self, position: LspPosition) -> Result<bool> {
        let Some(client) = self.client.as_mut() else {
            return Ok(false);
        };
        client.request_references(position)
    }

    pub fn request_rename(&mut self, position: LspPosition, new_name: &str) -> Result<bool> {
        let Some(client) = self.client.as_mut() else {
            return Ok(false);
        };
        client.request_rename(position, new_name)
    }

    pub fn request_formatting(&mut self, tab_size: usize, insert_spaces: bool) -> Result<bool> {
        let Some(client) = self.client.as_mut() else {
            return Ok(false);
        };
        client.request_formatting(tab_size, insert_spaces)
    }

    pub fn request_code_actions(&mut self, range: LspRange) -> Result<bool> {
        let Some(client) = self.client.as_mut() else {
            return Ok(false);
        };
        client.request_code_actions(range)
    }

    pub fn poll(&mut self) -> Vec<LanguageServiceEvent> {
        let mut output = Vec::new();
        let mut exited_indices = Vec::new();
        for (index, client) in self.pooled_clients.iter_mut().enumerate() {
            for event in client.poll() {
                match event {
                    ClientEvent::Diagnostics(uri, diagnostics) => {
                        if let Some(path) = uri_to_path(&uri) {
                            self.diagnostics_by_path.insert(path, diagnostics);
                            output.push(LanguageServiceEvent::DiagnosticsChanged);
                        }
                    }
                    ClientEvent::Exited => exited_indices.push(index),
                    // Interactive responses from an inactive document must not
                    // update the panel or be re-targeted to another language.
                    _ => {}
                }
            }
        }
        exited_indices.sort_unstable();
        exited_indices.dedup();
        for index in exited_indices.into_iter().rev() {
            self.pooled_clients.remove(index);
        }
        let Some(client) = self.client.as_mut() else {
            return output;
        };
        let label = client.server_label.clone();
        let mut exited = false;
        for event in client.poll() {
            match event {
                ClientEvent::Ready => {
                    output.push(LanguageServiceEvent::Ready(label.clone()));
                }
                ClientEvent::Diagnostics(uri, diagnostics) => {
                    if let Some(path) = uri_to_path(&uri) {
                        self.diagnostics_by_path.insert(path, diagnostics);
                        output.push(LanguageServiceEvent::DiagnosticsChanged);
                    }
                }
                ClientEvent::Completions(request_id, items) => {
                    output.push(LanguageServiceEvent::Completions { request_id, items });
                }
                ClientEvent::CompletionResolved(request_id, item) => {
                    output.push(LanguageServiceEvent::CompletionResolved { request_id, item });
                }
                ClientEvent::CompletionError(request_id, error) => {
                    output.push(LanguageServiceEvent::CompletionError { request_id, error });
                }
                ClientEvent::Symbols(items) => output.push(LanguageServiceEvent::Symbols(items)),
                ClientEvent::Definition(location) => {
                    output.push(LanguageServiceEvent::Definition(location))
                }
                ClientEvent::Hover(contents) => output.push(LanguageServiceEvent::Hover(contents)),
                ClientEvent::SignatureHelp(signatures) => {
                    output.push(LanguageServiceEvent::SignatureHelp(signatures))
                }
                ClientEvent::References(locations) => {
                    output.push(LanguageServiceEvent::References(locations))
                }
                ClientEvent::Rename(edits) => {
                    output.push(LanguageServiceEvent::RenamePreview(edits))
                }
                ClientEvent::Formatting(edits) => {
                    output.push(LanguageServiceEvent::Formatting(edits))
                }
                ClientEvent::CodeActions(actions) => {
                    output.push(LanguageServiceEvent::CodeActions(actions))
                }
                ClientEvent::Error(error) => {
                    self.last_error = Some(error.clone());
                    output.push(LanguageServiceEvent::Error(error));
                }
                ClientEvent::Exited => {
                    exited = true;
                    output.push(LanguageServiceEvent::Exited(label.clone()));
                }
            }
        }

        if exited {
            self.client = None;
        }
        output
    }

    /// Workspace Problems, with explicit file identity and stable ordering.
    pub fn workspace_diagnostics(&self) -> Vec<(PathBuf, LspDiagnostic)> {
        let workspace = self
            .client
            .as_ref()
            .map(|client| client.workspace_root.as_path());
        let mut rows: Vec<_> = self
            .diagnostics_by_path
            .iter()
            .filter(|(path, _)| workspace.is_none_or(|root| path.starts_with(root)))
            .flat_map(|(path, diagnostics)| {
                diagnostics
                    .iter()
                    .map(|diagnostic| (path.clone(), diagnostic.clone()))
            })
            .collect();
        rows.sort_by(|(a, ad), (b, bd)| {
            a.cmp(b)
                .then(ad.range.start.line.cmp(&bd.range.start.line))
                .then(ad.range.start.character.cmp(&bd.range.start.character))
        });
        rows
    }

    pub fn stop_current(&mut self) {
        self.client.take();
    }

    #[cfg(test)]
    pub fn diagnostics(&self) -> &[LspDiagnostic] {
        self.current_path
            .as_ref()
            .and_then(|path| {
                self.diagnostics_by_path.get(path).or_else(|| {
                    let identity = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
                    self.diagnostics_by_path
                        .iter()
                        .find_map(|(candidate, diagnostics)| {
                            let candidate_identity = std::fs::canonicalize(candidate)
                                .unwrap_or_else(|_| candidate.clone());
                            (candidate_identity == identity).then_some(diagnostics)
                        })
                })
            })
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn is_ready(&self) -> bool {
        self.client
            .as_ref()
            .is_some_and(|client| client.initialized)
    }

    pub fn server_label(&self) -> Option<&str> {
        self.client
            .as_ref()
            .map(|client| client.server_label.as_str())
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn supported_for_current_language(&self) -> bool {
        server_spec(&self.current_language).is_some()
    }

    /// Everything the language-server status view shows for a file in
    /// `language` (the active buffer's).
    pub fn health(&self, language: &str) -> LanguageHealth {
        let spec = server_spec(language);
        // The running client only describes this file if it serves the
        // same language.
        let client = self
            .client
            .as_ref()
            .filter(|_| self.current_language == language);
        let installed = spec.as_ref().and_then(|spec| find_program(&spec.program));
        let state = match (&spec, &installed, client) {
            (None, _, _) => ServerState::NoServerForLanguage,
            (Some(_), None, _) => ServerState::NotInstalled,
            (Some(_), Some(_), Some(client)) if client.initialized => ServerState::Running,
            (Some(_), Some(_), Some(_)) => ServerState::Starting,
            (Some(_), Some(_), None) if self.last_error.is_some() => ServerState::Failed,
            (Some(_), Some(_), None) => ServerState::NotStarted,
        };
        LanguageHealth {
            language: language.to_owned(),
            server: spec.as_ref().map(|spec| spec.label.to_owned()),
            command: spec.as_ref().map(|spec| {
                std::iter::once(spec.program.clone())
                    .chain(spec.args.clone())
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
            installed,
            state,
            capabilities: client
                .map(|client| client.capabilities.clone())
                .unwrap_or_default(),
            last_error: self.last_error.clone(),
            install_hint: install_hint(language),
            override_variable: override_variable(language),
        }
    }

    pub fn stop(&mut self) {
        self.client.take();
        self.pooled_clients.clear();
        self.diagnostics_by_path.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    NoServerForLanguage,
    NotInstalled,
    NotStarted,
    Starting,
    Running,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageHealth {
    pub language: String,
    pub server: Option<String>,
    pub command: Option<String>,
    pub installed: Option<PathBuf>,
    pub state: ServerState,
    pub capabilities: Vec<&'static str>,
    pub last_error: Option<String>,
    pub install_hint: Option<&'static str>,
    pub override_variable: Option<&'static str>,
}

/// Human names for the capabilities a server advertised.
fn announced_capabilities(capabilities: &Value) -> Vec<&'static str> {
    [
        ("completionProvider", "completion"),
        ("hoverProvider", "hover"),
        ("signatureHelpProvider", "signature help"),
        ("definitionProvider", "go to definition"),
        ("referencesProvider", "references"),
        ("renameProvider", "rename"),
        ("documentFormattingProvider", "formatting"),
        ("codeActionProvider", "code actions"),
    ]
    .into_iter()
    .filter(|(key, _)| {
        capabilities
            .get(key)
            .is_some_and(|value| !matches!(value, Value::Null | Value::Bool(false)))
    })
    .map(|(_, name)| name)
    .collect()
}

/// Resolves a server program the way spawning it would: a path as given, or
/// the first match on PATH.
fn find_program(program: &str) -> Option<PathBuf> {
    let is_runnable = |path: &Path| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            path.metadata()
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        {
            path.is_file()
        }
    };
    if program.contains(std::path::MAIN_SEPARATOR) {
        let path = PathBuf::from(program);
        return is_runnable(&path).then_some(path);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|path| is_runnable(path))
}

fn install_hint(language_name: &str) -> Option<&'static str> {
    Some(match language_name {
        "Rust" => "rustup component add rust-analyzer",
        "Python" => "npm install -g pyright",
        "JavaScript" | "TypeScript" | "React JSX" | "React TSX" => {
            "npm install -g typescript typescript-language-server"
        }
        "Go" => "go install golang.org/x/tools/gopls@latest",
        "Terraform" => "brew install hashicorp/tap/terraform-ls (or your package manager)",
        "YAML" => "npm install -g yaml-language-server",
        "JSON" => "npm install -g vscode-langservers-extracted",
        "Shell" => "npm install -g bash-language-server",
        _ => return None,
    })
}

fn override_variable(language_name: &str) -> Option<&'static str> {
    Some(match language_name {
        "Rust" => "MELLOW_LSP_RUST",
        "Python" => "MELLOW_LSP_PYTHON",
        "JavaScript" | "TypeScript" | "React JSX" | "React TSX" => "MELLOW_LSP_TYPESCRIPT",
        "Go" => "MELLOW_LSP_GO",
        "Terraform" => "MELLOW_LSP_TERRAFORM",
        "YAML" => "MELLOW_LSP_YAML",
        "JSON" => "MELLOW_LSP_JSON",
        "Shell" => "MELLOW_LSP_SHELL",
        _ => return None,
    })
}

fn server_spec(language_name: &str) -> Option<ServerSpec> {
    // Keys are the documented MELLOW_LSP_* names; the pre-rename MELLOW_LSP_*
    // names are still read.
    let env_override = |key: &str, default: &str| {
        crate::brand::env_var(key.trim_start_matches(crate::brand::ENV_PREFIX))
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| default.to_owned())
    };

    match language_name {
        "Rust" => Some(ServerSpec {
            program: env_override("MELLOW_LSP_RUST", "rust-analyzer"),
            args: Vec::new(),
            language_id: "rust",
            label: "rust-analyzer",
        }),
        "Python" => Some(ServerSpec {
            program: env_override("MELLOW_LSP_PYTHON", "pyright-langserver"),
            args: vec!["--stdio".to_owned()],
            language_id: "python",
            label: "Pyright",
        }),
        "JavaScript" | "TypeScript" | "React JSX" | "React TSX" => Some(ServerSpec {
            program: env_override("MELLOW_LSP_TYPESCRIPT", "typescript-language-server"),
            args: vec!["--stdio".to_owned()],
            language_id: if matches!(language_name, "TypeScript" | "React TSX") {
                "typescript"
            } else {
                "javascript"
            },
            label: "TypeScript language server",
        }),
        "Go" => Some(ServerSpec {
            program: env_override("MELLOW_LSP_GO", "gopls"),
            args: Vec::new(),
            language_id: "go",
            label: "gopls",
        }),
        "Terraform" => Some(ServerSpec {
            program: env_override("MELLOW_LSP_TERRAFORM", "terraform-ls"),
            args: vec!["serve".to_owned()],
            language_id: "terraform",
            label: "terraform-ls",
        }),
        "YAML" => Some(ServerSpec {
            program: env_override("MELLOW_LSP_YAML", "yaml-language-server"),
            args: vec!["--stdio".to_owned()],
            language_id: "yaml",
            label: "YAML language server",
        }),
        "JSON" => Some(ServerSpec {
            program: env_override("MELLOW_LSP_JSON", "vscode-json-language-server"),
            args: vec!["--stdio".to_owned()],
            language_id: "json",
            label: "JSON language server",
        }),
        "Shell" => Some(ServerSpec {
            program: env_override("MELLOW_LSP_SHELL", "bash-language-server"),
            args: vec!["start".to_owned()],
            language_id: "shellscript",
            label: "bash-language-server",
        }),
        _ => None,
    }
}

fn version_from_revision(revision: u64) -> i32 {
    i32::try_from(revision.min(i32::MAX as u64)).unwrap_or(i32::MAX)
}

fn position_json(position: LspPosition) -> Value {
    json!({ "line": position.line, "character": position.character })
}

fn range_json(range: LspRange) -> Value {
    json!({ "start": position_json(range.start), "end": position_json(range.end) })
}

fn parse_position(value: &Value) -> Option<LspPosition> {
    Some(LspPosition {
        line: usize::try_from(value.get("line")?.as_u64()?).ok()?,
        character: usize::try_from(value.get("character")?.as_u64()?).ok()?,
    })
}

fn parse_range(value: &Value) -> Option<LspRange> {
    let start = parse_position(value.get("start")?)?;
    let end = parse_position(value.get("end")?)?;
    ((start.line, start.character) <= (end.line, end.character)).then_some(LspRange { start, end })
}

fn parse_diagnostics(message: &Value) -> Vec<LspDiagnostic> {
    message
        .pointer("/params/diagnostics")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_DIAGNOSTICS)
        .filter_map(|diagnostic| {
            let severity = match diagnostic.get("severity").and_then(Value::as_u64) {
                Some(1) => DiagnosticSeverity::Error,
                Some(2) => DiagnosticSeverity::Warning,
                Some(3) => DiagnosticSeverity::Information,
                Some(4) => DiagnosticSeverity::Hint,
                _ => DiagnosticSeverity::Information,
            };
            Some(LspDiagnostic {
                range: parse_range(diagnostic.get("range")?)?,
                severity,
                source: diagnostic
                    .get("source")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                message: diagnostic.get("message")?.as_str()?.to_owned(),
            })
        })
        .collect()
}

fn parse_completion_result(result: &Value) -> Vec<CompletionItem> {
    let items = if let Some(items) = result.as_array() {
        items
    } else {
        result
            .get("items")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    };

    items
        .iter()
        .take(MAX_COMPLETIONS)
        .filter_map(|item| {
            let label = item.get("label")?.as_str()?.to_owned();
            let detail = item
                .get("detail")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let filter_text = item
                .get("filterText")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let sort_text = item
                .get("sortText")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let text_edit = item.get("textEdit");
            let edit_range = text_edit.and_then(|edit| {
                edit.get("range")
                    .or_else(|| edit.get("replace"))
                    .and_then(parse_range)
            });
            let insert_text = text_edit
                .and_then(|edit| edit.get("newText"))
                .and_then(Value::as_str)
                .or_else(|| item.get("insertText").and_then(Value::as_str))
                .unwrap_or(&label)
                .to_owned();

            let mut additional_text_edits = Vec::new();
            let mut rejected = None;
            if let Some(edits) = item.get("additionalTextEdits") {
                match edits.as_array() {
                    Some(edits) if edits.len() <= MAX_TEXT_EDITS => {
                        for edit in edits {
                            match (edit.get("range").and_then(parse_range), edit.get("newText").and_then(Value::as_str)) {
                                (Some(range), Some(text)) => additional_text_edits.push(CompletionTextEdit { range, new_text: text.to_owned() }),
                                _ => rejected = Some("Malformed supporting completion edit; nothing was changed".to_owned()),
                            }
                        }
                    }
                    _ => rejected = Some("Supporting completion edit limit exceeded or malformed; nothing was changed".to_owned()),
                }
            }
            if item.get("insertTextFormat").and_then(Value::as_u64) == Some(2) {
                rejected = Some("Server returned a snippet despite snippetSupport=false".to_owned());
            }
            if text_edit.is_some() && (edit_range.is_none()
                || text_edit.and_then(|edit| edit.get("newText")).and_then(Value::as_str).is_none()) {
                rejected = Some("Malformed primary completion edit; nothing was changed".to_owned());
            }
            if rejected.is_some() { additional_text_edits.clear(); }
            Some(CompletionItem {
                additional_text_edits,
                command: item.get("command").cloned(),
                raw: item.clone(),
                needs_resolve: false,
                rejected,
                origin: None,
                label,
                detail,
                filter_text,
                sort_text,
                insert_text,
                edit_range,
            })
        })
        .collect()
}

fn parse_symbols(result: &Value, document_uri: &str) -> Vec<SymbolLocation> {
    fn collect(
        items: &[Value],
        document_uri: &str,
        output: &mut Vec<SymbolLocation>,
        depth: usize,
    ) {
        if depth > 32 {
            return;
        }
        for item in items {
            if output.len() >= MAX_LOCATIONS {
                return;
            }
            let location = item.get("location");
            let uri = location
                .and_then(|location| location.get("uri"))
                .and_then(Value::as_str)
                .unwrap_or(document_uri);
            let range = location
                .and_then(|location| location.get("range"))
                .or_else(|| item.get("selectionRange"))
                .or_else(|| item.get("range"))
                .and_then(parse_range);
            if let (Some(name), Some(path), Some(range)) = (
                item.get("name").and_then(Value::as_str),
                uri_to_path(uri),
                range,
            ) {
                output.push(SymbolLocation {
                    name: name.to_owned(),
                    detail: item
                        .get("detail")
                        .or_else(|| item.get("containerName"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    kind: item.get("kind").and_then(Value::as_u64).unwrap_or(0),
                    path,
                    range,
                });
            }
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                collect(children, document_uri, output, depth + 1);
            }
        }
    }
    let mut output = Vec::new();
    if let Some(items) = result.as_array() {
        collect(items, document_uri, &mut output, 0);
    }
    output
}

fn parse_definition_result(result: &Value) -> Option<DefinitionLocation> {
    let candidate = if let Some(array) = result.as_array() {
        array.first()?
    } else {
        result
    };

    let uri = candidate
        .get("uri")
        .or_else(|| candidate.get("targetUri"))?
        .as_str()?;
    let range = candidate
        .get("range")
        .or_else(|| candidate.get("targetSelectionRange"))
        .and_then(parse_range)?;

    Some(DefinitionLocation {
        path: uri_to_path(uri)?,
        range,
    })
}

fn parse_hover_result(result: &Value) -> Option<String> {
    let contents = result.get("contents")?;
    let mut parts = Vec::new();
    match contents {
        Value::String(text) => parts.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                if let Some(text) = item
                    .as_str()
                    .or_else(|| item.get("value").and_then(Value::as_str))
                {
                    parts.push(text.to_owned());
                }
            }
        }
        Value::Object(_) => {
            if let Some(text) = contents.get("value").and_then(Value::as_str) {
                parts.push(text.to_owned());
            }
        }
        _ => {}
    }
    let joined = parts.join("\n\n");
    (!joined.is_empty()).then(|| joined.chars().take(MAX_HOVER_CHARS).collect())
}

fn parse_signature_result(result: &Value) -> Vec<String> {
    result
        .get("signatures")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_SIGNATURES)
        .filter_map(|item| item.get("label")?.as_str().map(str::to_owned))
        .collect()
}

fn parse_locations(result: &Value) -> Vec<DefinitionLocation> {
    result
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_LOCATIONS)
        .filter_map(|item| {
            let path = uri_to_path(item.get("uri")?.as_str()?)?;
            let range = parse_range(item.get("range")?)?;
            Some(DefinitionLocation { path, range })
        })
        .collect()
}

/// Appends one server edit list; any malformed entry or exceeding the limit
/// refuses the whole set.
fn push_text_edits(
    path: &Path,
    edits: &Value,
    output: &mut Vec<LspTextEdit>,
) -> std::result::Result<(), String> {
    let edits = edits
        .as_array()
        .ok_or("the language server sent a malformed edit list")?;
    for edit in edits {
        if output.len() >= MAX_TEXT_EDITS {
            return Err(format!(
                "the language server proposed more than {MAX_TEXT_EDITS} edits"
            ));
        }
        let (Some(range), Some(new_text)) = (
            edit.get("range").and_then(parse_range),
            edit.get("newText").and_then(Value::as_str),
        ) else {
            return Err("the language server sent a malformed edit".to_owned());
        };
        output.push(LspTextEdit {
            path: path.to_path_buf(),
            range,
            new_text: new_text.to_owned(),
        });
    }
    Ok(())
}

fn parse_document_edits(
    uri: &str,
    result: &Value,
) -> std::result::Result<Vec<LspTextEdit>, String> {
    let path = uri_to_path(uri).ok_or("the language server used an unsupported file URI")?;
    // Formatting's null result means there are no edits (per LSP).
    if result.is_null() {
        return Ok(Vec::new());
    }
    let mut output = Vec::new();
    push_text_edits(&path, result, &mut output)?;
    Ok(output)
}

/// `active` is the open document's (uri, version); edits addressed to an
/// older version of it are refused.
fn parse_workspace_edit(result: &Value, active: Option<(&str, i32)>) -> WorkspaceEditPreview {
    let mut output = Vec::new();
    let mut has_resource_operations = false;
    let mut collect = || -> std::result::Result<(), String> {
        if !result.is_null() && !result.is_object() {
            return Err("the language server sent a malformed workspace edit".to_owned());
        }
        if let Some(changes) = result.get("changes") {
            let changes = changes
                .as_object()
                .ok_or("the language server sent malformed workspace changes")?;
            for (uri, edits) in changes {
                let Some(path) = uri_to_path(uri) else {
                    has_resource_operations = true;
                    continue;
                };
                push_text_edits(&path, edits, &mut output)?;
            }
        }
        if let Some(document_changes) = result.get("documentChanges") {
            let document_changes = document_changes
                .as_array()
                .ok_or("the language server sent malformed document changes")?;
            for change in document_changes {
                let Some(uri) = change.pointer("/textDocument/uri").and_then(Value::as_str) else {
                    has_resource_operations = true;
                    continue;
                };
                let Some(path) = uri_to_path(uri) else {
                    has_resource_operations = true;
                    continue;
                };
                let Some(edits) = change.get("edits") else {
                    has_resource_operations = true;
                    continue;
                };
                let version = change
                    .pointer("/textDocument/version")
                    .and_then(Value::as_i64);
                if let (Some(version), Some((active_uri, active_version))) = (version, active)
                    && uri == active_uri
                    && version != i64::from(active_version)
                {
                    return Err(format!(
                        "the edits are for an older version of {}",
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string())
                    ));
                }
                push_text_edits(&path, edits, &mut output)?;
            }
        }
        Ok(())
    };
    let rejected = collect().err();
    if rejected.is_some() {
        output.clear();
    }
    WorkspaceEditPreview {
        edits: output,
        has_resource_operations,
        rejected,
    }
}

/// An edit response may include inactive open documents. Validate both the
/// server's stated versions and documents changed while the request was pending.
fn parse_workspace_edit_checked(
    result: &Value,
    active: Option<(&str, i32)>,
    current: &HashMap<String, (i32, String)>,
    requested: &HashMap<String, i32>,
) -> WorkspaceEditPreview {
    let mut preview = parse_workspace_edit(result, active);
    for change in result
        .get("documentChanges")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(uri), Some(version)) = (
            change.pointer("/textDocument/uri").and_then(Value::as_str),
            change
                .pointer("/textDocument/version")
                .and_then(Value::as_i64),
        ) && current
            .get(uri)
            .is_some_and(|(now, _)| i64::from(*now) != version)
        {
            preview.rejected =
                Some("Workspace edits target an older version of an open file".to_owned());
        }
    }
    for edit in &preview.edits {
        let uri = path_to_uri(&edit.path);
        if let Some(previous) = requested.get(&uri)
            && current.get(&uri).is_none_or(|(now, _)| now != previous)
        {
            preview.rejected = Some("Workspace edits discarded: an affected open file changed or closed while the request was pending".to_owned());
        }
    }
    if preview.rejected.is_some() {
        preview.edits.clear();
    }
    preview
}

#[cfg(test)]
fn parse_code_actions(result: &Value, active: Option<(&str, i32)>) -> Vec<CodeActionItem> {
    parse_code_actions_checked(result, active, &HashMap::new(), &HashMap::new())
}

fn parse_code_actions_checked(
    result: &Value,
    active: Option<(&str, i32)>,
    current: &HashMap<String, (i32, String)>,
    requested: &HashMap<String, i32>,
) -> Vec<CodeActionItem> {
    result
        .as_array()
        .into_iter()
        .flatten()
        .take(200)
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.to_owned();
            let preview = item
                .get("edit")
                .map(|edit| parse_workspace_edit_checked(edit, active, current, requested))
                .unwrap_or_default();
            Some(CodeActionItem {
                title,
                rejected: preview.rejected,
                edits: preview.edits,
                has_command: item.get("command").is_some(),
                has_resource_operations: preview.has_resource_operations,
            })
        })
        .collect()
}

fn write_lsp_frame(writer: &mut impl Write, body: &[u8]) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)
}

fn read_lsp_message(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(None);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line
            .strip_prefix("Content-Length:")
            .or_else(|| line.strip_prefix("content-length:"))
        {
            content_length = value.trim().parse::<usize>().ok();
        }
    }

    let Some(length) = content_length else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "LSP frame missing Content-Length",
        ));
    };
    // The length comes from the server; never allocate whatever it claims.
    if length > MAX_INCOMING_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("LSP frame of {length} bytes is over the {MAX_INCOMING_BYTES}-byte limit"),
        ));
    }

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub fn path_to_uri(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let text = absolute.to_string_lossy();
    let mut encoded = String::with_capacity(text.len() + 8);
    for byte in text.as_bytes() {
        let ch = char::from(*byte);
        if ch.is_ascii_alphanumeric() || matches!(ch, '/' | ':' | '-' | '_' | '.' | '~') {
            encoded.push(ch);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("file://{encoded}")
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    let mut bytes = Vec::with_capacity(encoded.len());
    let raw = encoded.as_bytes();
    let mut index = 0usize;
    while index < raw.len() {
        if raw[index] == b'%' && index + 2 < raw.len() {
            let hex = std::str::from_utf8(&raw[index + 1..index + 3]).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            bytes.push(raw[index]);
            index += 1;
        }
    }
    String::from_utf8(bytes).ok().map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn real_stdio_lsp_lifecycle_reaches_initialized_state() {
        use std::{
            fs, thread,
            time::{Duration, Instant},
        };

        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let document_path = dir.path().join("demo.rs");
        fs::write(&document_path, "fn main() {}\n").unwrap();

        let script = r#"
length=''
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\r')
  [ -z "$line" ] && break
  case "$line" in
    Content-Length:*) length=${line#Content-Length: } ;;
  esac
done
dd bs=1 count="$length" of=/dev/null 2>/dev/null
body='{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"textDocumentSync":1}}}'
printf 'Content-Length: %s\r\n\r\n%s' "__BODY_LEN__" "$body"
sleep 1
"#;

        let spec = ServerSpec {
            program: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), script.replace("__BODY_LEN__", "${#body}")],
            language_id: "rust",
            label: "fake-lsp",
        };

        let mut client =
            LspClient::spawn(&spec, dir.path(), &document_path, "fn main() {}\n", 1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut ready = false;

        while Instant::now() < deadline {
            if client
                .poll()
                .iter()
                .any(|event| matches!(event, ClientEvent::Ready))
            {
                ready = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert!(ready, "fake stdio language server never initialized");
        assert!(client.initialized);
    }

    /// Audit: a server that answered `initialize` and then stopped reading
    /// (busy indexing, hung) froze the editor, Quit included, once a
    /// didOpen/didChange filled its 64 KiB input pipe.
    #[cfg(unix)]
    #[test]
    fn a_server_that_stops_reading_never_blocks_the_editor() {
        use std::{
            fs, thread,
            time::{Duration, Instant},
        };

        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let document_path = dir.path().join("big.rs");
        let text = "pub fn f() -> u64 { 1 }\n".repeat(80_000); // ~2 MiB
        fs::write(&document_path, &text).unwrap();

        let script = r#"
length=''
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\r')
  [ -z "$line" ] && break
  case "$line" in
    Content-Length:*) length=${line#Content-Length: } ;;
  esac
done
dd bs=1 count="$length" of=/dev/null 2>/dev/null
body='{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}'
printf 'Content-Length: %s\r\n\r\n%s' "${#body}" "$body"
sleep 20
"#;
        let spec = ServerSpec {
            program: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            language_id: "rust",
            label: "stalled-lsp",
        };

        let mut client = LspClient::spawn(&spec, dir.path(), &document_path, &text, 1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !client.initialized && Instant::now() < deadline {
            client.poll(); // sends the 2 MiB didOpen the server never reads
            thread::sleep(Duration::from_millis(10));
        }
        assert!(client.initialized, "fake server never initialized");
        // Only the editor's calls are timed: blocking would last until the
        // server exits (~20 s), while encoding 38 large changes on a slow CI
        // machine takes a few seconds at most.
        let calls = Instant::now();
        for version in 2..40 {
            client.did_change(&text, version).unwrap();
        }
        assert!(
            calls.elapsed() < Duration::from_secs(10),
            "editor calls blocked on the server for {:?}",
            calls.elapsed()
        );
        // Typing coalesces: at most the unread didOpen plus one change wait.
        assert!(
            client.writer.pending_bytes() < 3 * text.len() + 64 * 1024,
            "{} bytes pending",
            client.writer.pending_bytes()
        );
    }

    #[cfg(unix)]
    #[test]
    fn pending_changes_coalesce_and_a_full_queue_stops_the_server() {
        use std::os::unix::net::UnixStream;

        let (sink, _unread) = UnixStream::pair().unwrap();
        let writer = LspWriter::start(sink, "test-writer".to_owned()).unwrap();
        let uri = Some("file:///a.rs".to_owned());
        // Fill the socket so later messages stay queued.
        writer.send(vec![b'x'; 4 * 1024 * 1024], None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let base = writer.pending_bytes();

        writer.send(vec![1; 1000], uri.clone()).unwrap();
        writer.send(vec![2; 1000], uri.clone()).unwrap();
        assert_eq!(
            writer.pending_bytes(),
            base + 1000,
            "newer change replaced older"
        );
        writer.send(vec![3; 10], None).unwrap();
        writer.send(vec![4; 1000], uri.clone()).unwrap();
        assert_eq!(
            writer.pending_bytes(),
            base + 2010,
            "a change after another message is kept, never reordered"
        );

        let mut result = Ok(());
        for _ in 0..40 {
            result = writer.send(vec![0; 1024 * 1024], None);
            if result.is_err() {
                break;
            }
        }
        assert!(result.is_err(), "queue must be bounded");
        assert!(writer.failure().unwrap().contains("stopped reading"));
        assert!(writer.send(vec![0; 1], None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn polish_completion_uses_server_trigger_characters_and_request_ids() {
        use std::{fs, thread, time::Duration};
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let document = dir.path().join("demo.rs");
        let log = dir.path().join("messages.log");
        fs::write(&document, "fn main() {}\n").unwrap();

        let script_path = dir.path().join("fake-completion-lsp.sh");
        fs::write(
            &script_path,
            r#"#!/bin/sh
log="$1"
while true; do
  length=''
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in
      Content-Length:*) length=${line#Content-Length: } ;;
    esac
  done
  [ -z "$length" ] && exit 0
  body=$(dd bs=1 count="$length" 2>/dev/null)
  printf '%s\n' "$body" >> "$log"
  case "$body" in
    *'"method":"initialize"'*)
      response='{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"textDocumentSync":1,"completionProvider":{"triggerCharacters":[".",":"]}}}}'
      printf 'Content-Length: %s\r\n\r\n%s' "${#response}" "$response"
      ;;
  esac
done
"#,
        )
        .unwrap();

        let spec = ServerSpec {
            program: "/bin/sh".to_owned(),
            args: vec![
                script_path.to_string_lossy().into_owned(),
                log.to_string_lossy().into_owned(),
            ],
            language_id: "rust",
            label: "fake-completion-lsp",
        };

        let mut client =
            LspClient::spawn(&spec, dir.path(), &document, "fn main() {}\n", 1).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline && !client.initialized {
            let _ = client.poll();
            thread::sleep(Duration::from_millis(10));
        }
        assert!(client.initialized);
        assert_eq!(client.completion_trigger_characters, vec![".", ":"]);

        let request_id = client
            .request_completion(
                LspPosition {
                    line: 0,
                    character: 3,
                },
                Some("."),
            )
            .unwrap()
            .unwrap();
        assert_eq!(request_id, 2);

        let message_deadline = std::time::Instant::now() + Duration::from_secs(2);
        let messages = loop {
            let messages = fs::read_to_string(&log).unwrap_or_default();
            if messages.contains("\"method\":\"textDocument/completion\"")
                && messages.contains("\"triggerKind\":2")
                && messages.contains("\"triggerCharacter\":\".\"")
                && messages.contains("\"id\":2")
            {
                break messages;
            }
            assert!(
                std::time::Instant::now() < message_deadline,
                "fake completion server did not observe the request: {messages}"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert!(messages.contains("\"method\":\"textDocument/completion\""));
        assert!(messages.contains("\"triggerKind\":2"));
        assert!(messages.contains("\"triggerCharacter\":\".\""));
        assert!(messages.contains("\"id\":2"));
    }

    #[cfg(unix)]
    #[test]
    fn goal16_same_server_keeps_multiple_documents_open_without_restart() {
        use std::{fs, thread, time::Duration};
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let one = dir.path().join("one.rs");
        let two = dir.path().join("two.rs");
        let log = dir.path().join("messages.log");
        fs::write(&one, "fn one() {}\n").unwrap();
        fs::write(&two, "fn two() {}\n").unwrap();

        let script_path = dir.path().join("fake-lsp.sh");
        fs::write(
            &script_path,
            r#"#!/bin/sh
log="$1"
while true; do
  length=''
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in
      Content-Length:*) length=${line#Content-Length: } ;;
    esac
  done
  [ -z "$length" ] && exit 0
  body=$(dd bs=1 count="$length" 2>/dev/null)
  printf '%s\n' "$body" >> "$log"
  case "$body" in
    *'"method":"initialize"'*)
      response='{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"textDocumentSync":1}}}'
      printf 'Content-Length: %s\r\n\r\n%s' "${#response}" "$response"
      ;;
  esac
done
"#,
        )
        .unwrap();

        let spec = ServerSpec {
            program: "/bin/sh".to_owned(),
            args: vec![
                script_path.to_string_lossy().into_owned(),
                log.to_string_lossy().into_owned(),
            ],
            language_id: "rust",
            label: "fake-multidoc-lsp",
        };

        let mut client = LspClient::spawn(&spec, dir.path(), &one, "fn one() {}\n", 1).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline && !client.initialized {
            let _ = client.poll();
            thread::sleep(Duration::from_millis(10));
        }
        assert!(client.initialized);

        client
            .activate_document("rust", &two, "fn two() {}\n", 1)
            .unwrap();
        client
            .activate_document("rust", &one, "fn one() {}\n", 1)
            .unwrap();
        client
            .did_change("fn one() { println!(\"one\"); }\n", 2)
            .unwrap();
        assert_eq!(client.opened_documents.len(), 2);
        client.close_path(&two).unwrap();
        assert_eq!(client.opened_documents.len(), 1);

        let message_deadline = std::time::Instant::now() + Duration::from_secs(2);
        let messages = loop {
            let messages = fs::read_to_string(&log).unwrap_or_default();
            if messages
                .matches("\"method\":\"textDocument/didOpen\"")
                .count()
                >= 2
                && messages
                    .matches("\"method\":\"textDocument/didChange\"")
                    .count()
                    >= 1
                && messages
                    .matches("\"method\":\"textDocument/didClose\"")
                    .count()
                    >= 1
            {
                break messages;
            }
            assert!(
                std::time::Instant::now() < message_deadline,
                "fake language server did not observe all lifecycle messages: {messages}"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(messages.matches("\"method\":\"initialize\"").count(), 1);
        assert_eq!(
            messages
                .matches("\"method\":\"textDocument/didOpen\"")
                .count(),
            2
        );
        assert_eq!(
            messages
                .matches("\"method\":\"textDocument/didChange\"")
                .count(),
            1
        );
        assert_eq!(
            messages
                .matches("\"method\":\"textDocument/didClose\"")
                .count(),
            1
        );
    }

    #[test]
    fn goal16_diagnostics_are_retained_per_open_document() {
        let one = PathBuf::from("/tmp/one.rs");
        let two = PathBuf::from("/tmp/two.rs");
        let diagnostic_one = LspDiagnostic {
            range: LspRange {
                start: LspPosition {
                    line: 0,
                    character: 0,
                },
                end: LspPosition {
                    line: 0,
                    character: 1,
                },
            },
            severity: DiagnosticSeverity::Warning,
            source: Some("fake".to_owned()),
            message: "one".to_owned(),
        };
        let diagnostic_two = LspDiagnostic {
            message: "two".to_owned(),
            ..diagnostic_one.clone()
        };

        let mut service = LanguageService::default();
        service
            .diagnostics_by_path
            .insert(one.clone(), vec![diagnostic_one]);
        service
            .diagnostics_by_path
            .insert(two.clone(), vec![diagnostic_two]);

        service.current_path = Some(one);
        assert_eq!(service.diagnostics()[0].message, "one");
        service.current_path = Some(two);
        assert_eq!(service.diagnostics()[0].message, "two");
    }

    #[test]
    fn oversized_incoming_frames_are_refused_without_allocating() {
        let mut reader = Cursor::new(b"Content-Length: 99999999999\r\n\r\n{}".to_vec());
        let error = read_lsp_message(&mut reader).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn lsp_framing_round_trip_decodes_json_rpc_message() {
        let value = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": {"ok": true}
        });
        let body = serde_json::to_vec(&value).unwrap();
        let mut framed = Vec::new();
        write_lsp_frame(&mut framed, &body).unwrap();

        let parsed = read_lsp_message(&mut Cursor::new(framed)).unwrap().unwrap();
        assert_eq!(parsed, value);
    }

    #[cfg(unix)]
    fn real_server_completion_fixture(
        program_variable: &str,
        language_id: &'static str,
        extension: &str,
    ) {
        use std::time::{Duration, Instant};
        let program =
            std::env::var(program_variable).expect("set the explicit real-server executable path");
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let document = root.join(format!("main.{extension}"));
        let helper = if extension == "py" {
            "def audit_symbol() -> int:\n    return 42\n"
        } else {
            "export function audit_symbol(): number { return 42; }\n"
        };
        std::fs::write(root.join(format!("helper.{extension}")), helper).unwrap();
        let text = "audit_sy";
        std::fs::write(&document, text).unwrap();
        if extension == "py" {
            std::fs::write(
                root.join("pyrightconfig.json"),
                r#"{"include":["."],"typeCheckingMode":"basic"}"#,
            )
            .unwrap();
        } else {
            std::fs::write(root.join("tsconfig.json"), r#"{"compilerOptions":{"strict":true,"module":"commonjs","target":"ES2020"},"include":["*.ts"]}"#).unwrap();
        }
        let spec = ServerSpec {
            program,
            args: vec!["--stdio".to_owned()],
            language_id,
            label: "real language server",
        };
        let mut client = LspClient::spawn(&spec, &root, &document, text, 1).unwrap();
        client
            .activate_document(
                language_id,
                &root.join(format!("helper.{extension}")),
                helper,
                1,
            )
            .unwrap();
        client
            .activate_document(language_id, &document, text, 1)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(45);
        while !client.initialized {
            for event in client.poll() {
                if let ClientEvent::Error(error) = event {
                    panic!("initialization error: {error}");
                }
            }
            assert!(Instant::now() < deadline, "server initialization timed out");
            thread::sleep(Duration::from_millis(20));
        }
        // Initialize only establishes the protocol. Wait for the synthetic
        // document's first diagnostics so auto-import checks start after indexing.
        let mut indexed = false;
        while !indexed {
            for event in client.poll() {
                if let ClientEvent::Diagnostics(uri, _) = event {
                    indexed |= uri == path_to_uri(&document);
                }
            }
            assert!(
                Instant::now() < deadline,
                "server did not finish initial document analysis"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let mut last_labels = Vec::new();
        let item = 'completion: loop {
            let id = client
                .request_completion(
                    LspPosition {
                        line: 0,
                        character: text.len(),
                    },
                    None,
                )
                .unwrap()
                .unwrap();
            loop {
                let events = client.poll();
                let mut finished = false;
                for event in events {
                    match event {
                        ClientEvent::Completions(request_id, items) if request_id == id => {
                            last_labels = items.iter().map(|item| item.label.clone()).collect();
                            if let Some(item) =
                                items.into_iter().find(|item| item.label == "audit_symbol")
                            {
                                break 'completion item;
                            }
                            finished = true;
                        }
                        ClientEvent::CompletionError(request_id, error) if request_id == id => {
                            panic!("completion error: {error}")
                        }
                        _ => {}
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "server did not offer the workspace auto-import; labels: {:?}",
                    last_labels
                );
                if finished {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            thread::sleep(Duration::from_millis(100));
        };
        let item = if item.needs_resolve {
            let id = client.request_completion_resolve(&item).unwrap().unwrap();
            'resolve: loop {
                for event in client.poll() {
                    match event {
                        ClientEvent::CompletionResolved(request_id, item) if request_id == id => {
                            break 'resolve *item;
                        }
                        ClientEvent::CompletionError(request_id, error) if request_id == id => {
                            panic!("resolve error: {error}")
                        }
                        _ => {}
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "server did not resolve the completion"
                );
                thread::sleep(Duration::from_millis(20));
            }
        } else {
            item
        };
        assert!(
            item.rejected.is_none(),
            "completion rejected: {:?}",
            item.rejected
        );
        assert!(
            !item.additional_text_edits.is_empty(),
            "real server did not supply supporting import edits: {:?}",
            item.raw
        );
        assert!(
            item.additional_text_edits
                .iter()
                .any(|edit| edit.new_text.contains("helper")),
            "supporting import did not reference helper: {:?}",
            item.additional_text_edits
        );
        assert_eq!(item.origin, Some((document.clone(), 1)));
        // Exercise the retained edit set through Mellow's shared atomic edit and
        // history operation, rather than only inspecting the protocol response.
        let mut buffer = crate::buffer::Buffer::open(Some(document.clone())).unwrap();
        let primary = item.edit_range.unwrap_or(LspRange {
            start: LspPosition {
                line: 0,
                character: 0,
            },
            end: LspPosition {
                line: 0,
                character: text.len(),
            },
        });
        let mut edit_set = vec![CompletionTextEdit {
            range: primary,
            new_text: item.insert_text.clone(),
        }];
        edit_set.extend(item.additional_text_edits.clone());
        let replacements: Vec<_> = edit_set
            .iter()
            .map(|edit| {
                (
                    crate::cursor::Cursor::new(
                        edit.range.start.line,
                        buffer.grapheme_col_for_utf16(
                            edit.range.start.line,
                            edit.range.start.character,
                        ),
                    ),
                    crate::cursor::Cursor::new(
                        edit.range.end.line,
                        buffer
                            .grapheme_col_for_utf16(edit.range.end.line, edit.range.end.character),
                    ),
                    edit.new_text.clone(),
                )
            })
            .collect();
        let mut cursor = crate::cursor::Cursor::new(0, text.len());
        buffer.replace_ranges_with_cursors(&replacements, cursor);
        let completed = buffer.contents();
        assert!(completed.contains("helper") && completed.contains("audit_symbol"));
        assert!(buffer.undo(&mut cursor));
        assert_eq!(
            buffer.contents(),
            text,
            "one undo must revert both completion and import"
        );
        assert!(buffer.redo(&mut cursor));
        assert_eq!(buffer.contents(), completed);

        let program = if extension == "py" {
            "from helper import audit_symbol\naudit_symbol()\naudit_missing_name\n"
        } else {
            "import { audit_symbol } from \"./helper\";\naudit_symbol();\naudit_missing_name;\n"
        };
        client.did_change(program, 2).unwrap();
        client
            .request_definition(LspPosition {
                line: 1,
                character: 3,
            })
            .unwrap();
        let mut definition_seen = false;
        let mut diagnostic_seen = false;
        let navigation_deadline = Instant::now() + Duration::from_secs(30);
        while !(definition_seen && diagnostic_seen) {
            for event in client.poll() {
                match event {
                    ClientEvent::Definition(Some(location)) => {
                        assert_eq!(location.path, root.join(format!("helper.{extension}")));
                        assert_eq!(location.range.start.line, 0);
                        definition_seen = true;
                    }
                    ClientEvent::Diagnostics(uri, diagnostics) if uri == path_to_uri(&document) => {
                        diagnostic_seen |= diagnostics.iter().any(|diagnostic| {
                            diagnostic.message.contains("audit_missing_name")
                                && diagnostic.range.start.line == 2
                        });
                    }
                    ClientEvent::Error(error) => panic!("real server workflow error: {error}"),
                    _ => {}
                }
            }
            assert!(
                Instant::now() < navigation_deadline,
                "real server did not complete navigation/diagnostics: definition={definition_seen}, diagnostic={diagnostic_seen}"
            );
            thread::sleep(Duration::from_millis(20));
        }
        eprintln!(
            "real {language_id} auto-import retained {} supporting edits; command retained={} (never executed)",
            item.additional_text_edits.len(),
            item.command.is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires an installed real Pyright executable via MELLOW_TEST_PYRIGHT"]
    fn real_pyright_completion_resolves_workspace_auto_import() {
        real_server_completion_fixture("MELLOW_TEST_PYRIGHT", "python", "py");
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires an installed real TypeScript language server via MELLOW_TEST_TYPESCRIPT_LSP"]
    fn real_typescript_completion_resolves_workspace_auto_import() {
        real_server_completion_fixture("MELLOW_TEST_TYPESCRIPT_LSP", "typescript", "ts");
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires real Pyright and TypeScript server paths via MELLOW_TEST_PYRIGHT and MELLOW_TEST_TYPESCRIPT_LSP"]
    fn real_mixed_language_pool_preserves_unsaved_diagnostics_and_processes() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let python = root.join("one.py");
        let typescript = root.join("two.ts");
        let python_original = "audit_python_original_missing\n";
        let typescript_original =
            "const audit_ts_value: number = audit_typescript_original_missing;\n";
        std::fs::write(&python, python_original).unwrap();
        std::fs::write(&typescript, typescript_original).unwrap();
        std::fs::write(
            root.join("pyrightconfig.json"),
            r#"{"include":["*.py"],"typeCheckingMode":"basic"}"#,
        )
        .unwrap();
        std::fs::write(root.join("tsconfig.json"), r#"{"compilerOptions":{"strict":true,"module":"commonjs","target":"ES2020"},"include":["*.ts"]}"#).unwrap();
        let py = ServerSpec {
            program: std::env::var("MELLOW_TEST_PYRIGHT").expect("set real Pyright path"),
            args: vec!["--stdio".to_owned()],
            language_id: "python",
            label: "real Pyright",
        };
        let ts = ServerSpec {
            program: std::env::var("MELLOW_TEST_TYPESCRIPT_LSP")
                .expect("set real TypeScript server path"),
            args: vec!["--stdio".to_owned()],
            language_id: "typescript",
            label: "real TypeScript",
        };
        let mut service = LanguageService::default();
        service.activate_with_spec(&root, &python, &py, python_original, 0);
        let py_pid = service.client.as_ref().unwrap().child.id();
        service.activate_with_spec(&root, &typescript, &ts, typescript_original, 0);
        let ts_pid = service.client.as_ref().unwrap().child.id();
        assert_ne!(py_pid, ts_pid);
        let deadline = Instant::now() + Duration::from_secs(45);
        let wait_for = |service: &mut LanguageService, py_name: &str, ts_name: &str| {
            loop {
                for event in service.poll() {
                    assert!(
                        !matches!(
                            event,
                            LanguageServiceEvent::Exited(_) | LanguageServiceEvent::Error(_)
                        ),
                        "real language service failed: {event:?}"
                    );
                }
                let problems = service.workspace_diagnostics();
                if problems
                    .iter()
                    .any(|(path, d)| path == &python && d.message.contains(py_name))
                    && problems
                        .iter()
                        .any(|(path, d)| path == &typescript && d.message.contains(ts_name))
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "named workspace diagnostics did not arrive: {problems:?}"
                );
                thread::sleep(Duration::from_millis(20));
            }
        };
        wait_for(
            &mut service,
            "audit_python_original_missing",
            "audit_typescript_original_missing",
        );
        let python_unsaved = "audit_python_unsaved_missing\n";
        service.sync_document(Some(&python), python_unsaved, 1);
        wait_for(
            &mut service,
            "audit_python_unsaved_missing",
            "audit_typescript_original_missing",
        );
        service.activate_with_spec(&root, &python, &py, python_unsaved, 1);
        let typescript_unsaved =
            "const audit_ts_value: number = audit_typescript_unsaved_missing;\naudit_ts_value;\n";
        service.sync_document(Some(&typescript), typescript_unsaved, 1);
        wait_for(
            &mut service,
            "audit_python_unsaved_missing",
            "audit_typescript_unsaved_missing",
        );
        for _ in 0..4 {
            service.activate_with_spec(&root, &typescript, &ts, typescript_unsaved, 1);
            assert_eq!(service.client.as_ref().unwrap().child.id(), ts_pid);
            assert_eq!(
                service.client.as_ref().unwrap().opened_documents[&path_to_uri(&typescript)].1,
                typescript_unsaved
            );
            service.activate_with_spec(&root, &python, &py, python_unsaved, 1);
            assert_eq!(service.client.as_ref().unwrap().child.id(), py_pid);
            assert_eq!(
                service.client.as_ref().unwrap().opened_documents[&path_to_uri(&python)].1,
                python_unsaved
            );
            assert_eq!(service.pooled_clients.len(), 1);
        }
        assert_eq!(std::fs::read_to_string(&python).unwrap(), python_original);
        assert_eq!(
            std::fs::read_to_string(&typescript).unwrap(),
            typescript_original
        );
        let problems = service.workspace_diagnostics();
        assert!(
            !problems
                .iter()
                .any(|(_, d)| d.message.contains("original_missing"))
        );
        eprintln!(
            "real mixed pool retained Pyright pid {py_pid}, TypeScript pid {ts_pid}; both inactive unsaved diagnostic updates retained file identity"
        );

        // Pyright supports workspace symbols. This checks real protocol/file
        // identity separately from terminal selection or display acceptance.
        let symbol_text =
            "def audit_workspace_symbol():\n    return 42\n\naudit_workspace_symbol()\n";
        service.sync_document(Some(&python), symbol_text, 2);
        let mut saw_analysis = false;
        while !saw_analysis {
            for event in service.poll() {
                saw_analysis |= matches!(event, LanguageServiceEvent::DiagnosticsChanged)
                    && service
                        .diagnostics_by_path
                        .get(&python)
                        .is_some_and(Vec::is_empty);
            }
            assert!(
                Instant::now() < deadline,
                "symbol document analysis timed out"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            service
                .request_workspace_symbols("audit_workspace_symbol")
                .unwrap()
        );
        'symbols: loop {
            for event in service.poll() {
                if let LanguageServiceEvent::Symbols(symbols) = event {
                    assert!(
                        symbols
                            .iter()
                            .any(|symbol| symbol.name == "audit_workspace_symbol"
                                && symbol.path == python),
                        "real workspace symbol identity missing: {symbols:?}"
                    );
                    break 'symbols;
                }
            }
            assert!(Instant::now() < deadline, "workspace symbols timed out");
            thread::sleep(Duration::from_millis(20));
        }
        for (path, spec, text, revision, position, use_row) in [
            (
                &python,
                &py,
                symbol_text,
                2,
                LspPosition {
                    line: 0,
                    character: 10,
                },
                3,
            ),
            (
                &typescript,
                &ts,
                typescript_unsaved,
                1,
                LspPosition {
                    line: 0,
                    character: 10,
                },
                1,
            ),
        ] {
            service.activate_with_spec(&root, path, spec, text, revision);
            assert!(service.request_references(position).unwrap());
            'references: loop {
                for event in service.poll() {
                    if let LanguageServiceEvent::References(locations) = event {
                        assert!(
                            locations.iter().any(|location| location.path == *path
                                && location.range.start.line == use_row),
                            "real references lost unsaved callsite/file identity: {locations:?}"
                        );
                        break 'references;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "real unsaved references timed out"
                );
                thread::sleep(Duration::from_millis(20));
            }
        }
    }

    #[cfg(unix)]
    fn inert_test_spec(identity: &str) -> ServerSpec {
        ServerSpec {
            program: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "cat >/dev/null".to_owned(),
                identity.to_owned(),
            ],
            language_id: "python",
            label: "fixture",
        }
    }

    #[cfg(unix)]
    fn initialize_test_client(client: &mut LspClient) {
        client.handle_message(json!({"id": 1, "result": {"capabilities": {"completionProvider": {"resolveProvider": true}}}}), &mut Vec::new());
        assert!(client.initialized);
    }

    #[cfg(unix)]
    #[test]
    fn f10_two_services_stay_warm_and_sync_inactive_unsaved_documents() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let one = root.join("one.py");
        let two = root.join("two.ts");
        let mut service = LanguageService::default();
        let a = inert_test_spec("python");
        let b = inert_test_spec("typescript");
        service.activate_with_spec(&root, &one, &a, "first", 0);
        let first_pid = service.client.as_ref().unwrap().child.id();
        initialize_test_client(service.client.as_mut().unwrap());
        service.activate_with_spec(&root, &two, &b, "second", 0);
        let second_pid = service.client.as_ref().unwrap().child.id();
        initialize_test_client(service.client.as_mut().unwrap());
        service.sync_document(Some(&one), "unsaved first", 1);
        for _ in 0..3 {
            service.activate_with_spec(&root, &one, &a, "unsaved first", 1);
            assert_eq!(service.client.as_ref().unwrap().child.id(), first_pid);
            assert_eq!(
                service.client.as_ref().unwrap().opened_documents[&path_to_uri(&one)].1,
                "unsaved first"
            );
            service.activate_with_spec(&root, &two, &b, "second", 0);
            assert_eq!(service.client.as_ref().unwrap().child.id(), second_pid);
        }
        service.close_document(Some(&one));
        assert!(
            !service.pooled_clients[0]
                .opened_documents
                .contains_key(&path_to_uri(&one))
        );
    }

    #[cfg(unix)]
    #[test]
    fn f10_documents_opened_before_initialize_are_all_delivered() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let one = root.join("one.py");
        let two = root.join("two.py");
        let mut client =
            LspClient::spawn(&inert_test_spec("python"), &root, &one, "first", 1).unwrap();
        client
            .activate_document("python", &two, "second", 2)
            .unwrap();
        initialize_test_client(&mut client);
        assert_eq!(client.opened_documents.len(), 2);
        assert_eq!(client.opened_documents[&path_to_uri(&one)].1, "first");
        assert_eq!(client.opened_documents[&path_to_uri(&two)].1, "second");
        assert_eq!(client.document.uri, path_to_uri(&two));
    }

    #[cfg(unix)]
    #[test]
    fn f09_resolve_preserves_import_and_rejects_stale_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.py");
        let mut client =
            LspClient::spawn(&inert_test_spec("python"), dir.path(), &path, "Pat", 1).unwrap();
        initialize_test_client(&mut client);
        let id = client
            .request_completion(
                LspPosition {
                    line: 0,
                    character: 3,
                },
                None,
            )
            .unwrap()
            .unwrap();
        let mut events = Vec::new();
        client.handle_message(
            json!({"id": id, "result": [{"label": "Path", "data": {"ticket": 19}}]}),
            &mut events,
        );
        let ClientEvent::Completions(_, items) = events.remove(0) else {
            panic!("expected completion");
        };
        assert!(items[0].needs_resolve);
        assert_eq!(items[0].raw["data"]["ticket"], 19);
        let resolve_id = client
            .request_completion_resolve(&items[0])
            .unwrap()
            .unwrap();
        client.handle_message(json!({"id": resolve_id, "result": {
            "label": "Path", "data": {"ticket": 19},
            "additionalTextEdits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "from pathlib import Path\n"}]
        }}), &mut events);
        let ClientEvent::CompletionResolved(_, item) = events.remove(0) else {
            panic!("expected resolved item");
        };
        assert_eq!(
            item.additional_text_edits[0].new_text,
            "from pathlib import Path\n"
        );
        assert!(!item.needs_resolve);
        let stale_id = client
            .request_completion_resolve(&items[0])
            .unwrap()
            .unwrap();
        client.did_change("changed", 2).unwrap();
        assert!(client.request_completion_resolve(&items[0]).is_err());
        client.handle_message(
            json!({"id": stale_id, "result": {"label": "Path"}}),
            &mut events,
        );
        assert!(matches!(events[0], ClientEvent::CompletionError(_, _)));
    }

    #[test]
    fn symbols_keep_hierarchy_and_workspace_file_identity() {
        let range =
            json!({"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 3}});
        let symbols = parse_symbols(
            &json!([{"name": "parent", "kind": 5, "selectionRange": range, "children": [{"name": "child", "kind": 6, "selectionRange": range}]}]),
            "file:///tmp/demo.yaml",
        );
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[1].name, "child");
        assert_eq!(symbols[1].path, PathBuf::from("/tmp/demo.yaml"));
        let workspace = parse_symbols(
            &json!([{"name": "module", "location": {"uri": "file:///tmp/other.ts", "range": range}}]),
            "file:///tmp/current.ts",
        );
        assert_eq!(workspace[0].path, PathBuf::from("/tmp/other.ts"));
    }

    #[test]
    fn f09_completion_keeps_supporting_import_and_opaque_resolve_data() {
        let result = json!([{
            "label": "Path", "insertText": "Path", "data": {"ticket": 19},
            "additionalTextEdits": [{"range": {
                "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}
            }, "newText": "from pathlib import Path\n"}],
            "command": {"title": "finish", "command": "server.private.finish", "arguments": [19]}
        }]);
        let items = parse_completion_result(&result);
        let retained = format!("{:?}", items[0]);
        assert!(
            retained.contains("from pathlib import Path"),
            "supporting import was lost: {retained}"
        );
        assert!(
            retained.contains("server.private.finish"),
            "command was silently lost"
        );
        assert!(retained.contains("ticket"), "resolve identity was lost");
    }

    #[test]
    fn latest_requests_win_when_same_document_replies_arrive_in_reverse() {
        let mut client = silent_client(Path::new("/tmp/mellow-a.rs"));
        let kinds = [
            RequestKind::WorkspaceSymbols,
            RequestKind::DocumentSymbols,
            RequestKind::Definition,
            RequestKind::Hover,
            RequestKind::SignatureHelp,
            RequestKind::References,
            RequestKind::Completion,
            RequestKind::CompletionResolve,
        ];
        for kind in kinds {
            let old = client
                .send_request(kind, "fixture", json!({"query":"alpha"}))
                .unwrap();
            client.remember_edit_target(old);
            let latest = client
                .send_request(kind, "fixture", json!({"query":"beta"}))
                .unwrap();
            client.remember_edit_target(latest);
            assert!(
                !client.pending.contains_key(&old),
                "old {kind:?} request was not invalidated"
            );
            assert!(!client.edit_targets.contains_key(&old));
            let mut events = Vec::new();
            client.handle_message(json!({"id":latest,"result":null}), &mut events);
            let count = events.len();
            client.handle_message(json!({"id":old,"result":null}), &mut events);
            assert_eq!(
                events.len(),
                count,
                "older {kind:?} response replaced the latest result"
            );
        }
    }

    #[test]
    fn switching_symbol_scope_invalidates_the_previous_panel_request() {
        let mut client = silent_client(Path::new("/tmp/symbol-scope.rs"));
        for (old_kind, latest_kind) in [
            (RequestKind::WorkspaceSymbols, RequestKind::DocumentSymbols),
            (RequestKind::DocumentSymbols, RequestKind::WorkspaceSymbols),
        ] {
            let old = client
                .send_request(old_kind, "fixture", Value::Null)
                .unwrap();
            client.remember_edit_target(old);
            let latest = client
                .send_request(latest_kind, "fixture", Value::Null)
                .unwrap();
            client.remember_edit_target(latest);
            let mut events = Vec::new();
            client.handle_message(json!({"id":latest,"result":null}), &mut events);
            assert!(matches!(events.as_slice(), [ClientEvent::Symbols(_)]));
            client.handle_message(json!({"id":old,"result":null}), &mut events);
            assert_eq!(
                events.len(),
                1,
                "older {old_kind:?} reply replaced {latest_kind:?} panel"
            );
            assert!(!client.edit_targets.contains_key(&old));
        }
    }

    #[test]
    fn unchanged_inactive_document_sync_is_idempotent() {
        let inactive = Path::new("/tmp/inactive-sync.rs");
        let mut client = silent_client(inactive);
        client.did_open().unwrap();
        let original = client.document.text.clone();
        let active = DocumentDescriptor {
            uri: path_to_uri(Path::new("/tmp/active-sync.rs")),
            language_id: "rust".to_owned(),
            version: 1,
            text: "fn active() {}".to_owned(),
        };
        client.document = active.clone();
        // A queue without a writer thread lets us observe exactly whether a
        // change was sent, independently of subprocess pipe scheduling.
        client.writer = LspWriter {
            shared: Arc::new((Mutex::new(OutgoingQueue::default()), Condvar::new())),
        };
        let mut service = LanguageService {
            client: Some(client),
            ..LanguageService::default()
        };
        service.sync_document(Some(inactive), &original, 1);
        assert_eq!(
            service.client.as_ref().unwrap().writer.pending_bytes(),
            0,
            "unchanged document emitted a duplicate-version didChange"
        );
        let changed = "fn inactive_updated() {}";
        service.sync_document(Some(inactive), changed, 2);
        let client = service.client.as_ref().unwrap();
        assert!(client.writer.pending_bytes() > 0);
        assert_eq!(
            client.opened_documents[&path_to_uri(inactive)],
            (2, changed.to_owned())
        );
        assert_eq!(client.document.uri, active.uri);
        assert_eq!(client.document.text, active.text);
        assert_eq!(client.document.version, active.version);
    }

    #[cfg(unix)]
    #[test]
    fn interaction_cancellation_drops_responses_without_cancelling_symbol_panels() {
        let mut client = silent_client(Path::new("/tmp/cancel.rs"));
        let mut cancelled = Vec::new();
        for kind in [
            RequestKind::Definition,
            RequestKind::References,
            RequestKind::Hover,
            RequestKind::SignatureHelp,
        ] {
            let id = client.send_request(kind, "fixture", Value::Null).unwrap();
            client.remember_edit_target(id);
            cancelled.push(id);
        }
        let symbols = client
            .send_request(
                RequestKind::WorkspaceSymbols,
                "workspace/symbol",
                Value::Null,
            )
            .unwrap();
        let mut service = LanguageService {
            client: Some(client),
            ..LanguageService::default()
        };
        service.cancel_interactions();
        let client = service.client.as_mut().unwrap();
        assert!(client.pending.contains_key(&symbols));
        for id in cancelled {
            assert!(!client.pending.contains_key(&id));
            assert!(!client.edit_targets.contains_key(&id));
            let mut events = Vec::new();
            client.handle_message(json!({"id":id,"result":null}), &mut events);
            assert!(events.is_empty());
        }
    }

    #[test]
    fn formatting_null_result_means_no_edits() {
        assert!(
            parse_document_edits("file:///tmp/a.rs", &Value::Null)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn f10_malformed_workspace_edit_containers_never_apply_a_partial_set() {
        for edit in [
            json!({"changes":{"file:///tmp/a.rs":[text_edit(0)], "file:///tmp/b.rs":null}}),
            json!({"changes":[], "documentChanges":[{"textDocument":{"uri":"file:///tmp/a.rs","version":null},"edits":[text_edit(0)]}]}),
            json!({"changes":{"file:///tmp/a.rs":[text_edit(0)]}, "documentChanges":{}}),
        ] {
            let preview = parse_workspace_edit(&edit, None);
            assert!(
                preview.rejected.is_some() && preview.edits.is_empty(),
                "malformed edit was partially accepted: {edit}"
            );
        }
    }

    #[test]
    fn f10_close_and_reopen_cannot_resurrect_pending_completion() {
        let path = Path::new("/tmp/mellow-a.rs");
        let mut client = silent_client(path);
        let id = client
            .request_completion(
                LspPosition {
                    line: 0,
                    character: 0,
                },
                None,
            )
            .unwrap()
            .unwrap();
        client.close_path(path).unwrap();
        client
            .activate_document("rust", path, "new document with reset revision", 1)
            .unwrap();
        let mut events = Vec::new();
        client.handle_message(
            json!({"id":id,"result":[{"label":"old_item"}]}),
            &mut events,
        );
        assert!(
            events.is_empty(),
            "closed-document requests must stay cancelled after reopen"
        );
        assert!(!client.pending.contains_key(&id));
        assert!(!client.edit_targets.contains_key(&id));
    }

    #[test]
    fn f10_unsolicited_server_edits_receive_an_explicit_refusal() {
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let mut client = silent_client(Path::new("/tmp/mellow-a.rs"));
        client.writer =
            LspWriter::start(Capture(Arc::clone(&bytes)), "capture".to_owned()).unwrap();
        let mut events = Vec::new();
        client.handle_message(json!({"id":"server-edit","method":"workspace/applyEdit","params":{"edit":{"changes":{"file:///tmp/mellow-a.rs":[text_edit(0)]}}}}), &mut events);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let response = loop {
            let snapshot = bytes.lock().unwrap().clone();
            if !snapshot.is_empty() {
                break read_lsp_message(&mut std::io::Cursor::new(snapshot))
                    .unwrap()
                    .unwrap();
            }
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(std::time::Duration::from_millis(1));
        };
        assert_eq!(response["id"], "server-edit");
        assert_eq!(response["result"]["applied"], false);
        assert!(
            response["result"]["failureReason"]
                .as_str()
                .unwrap()
                .contains("review")
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ClientEvent::Rename(_) | ClientEvent::Formatting(_)))
        );
    }

    #[test]
    fn f10_workspace_edits_refuse_changed_inactive_documents_and_stale_versions() {
        let a = Path::new("/tmp/mellow-a.rs");
        let b = Path::new("/tmp/mellow-b.rs");
        let mut client = silent_client(a);
        client.activate_document("rust", b, "old b", 1).unwrap();
        client.activate_document("rust", a, "active a", 1).unwrap();
        client
            .request_rename(
                LspPosition {
                    line: 0,
                    character: 0,
                },
                "renamed",
            )
            .unwrap();
        let id = client.next_id - 1;
        client.activate_document("rust", b, "new b", 2).unwrap();
        client.activate_document("rust", a, "active a", 1).unwrap();
        let mut events = Vec::new();
        client.handle_message(
            json!({"id":id,"result":{"changes":{path_to_uri(b):[text_edit(0)]}}}),
            &mut events,
        );
        let ClientEvent::Rename(preview) = events.remove(0) else {
            panic!("expected preview");
        };
        assert!(preview.edits.is_empty() && preview.rejected.is_some());
        client
            .request_code_actions(LspRange {
                start: LspPosition {
                    line: 0,
                    character: 0,
                },
                end: LspPosition {
                    line: 0,
                    character: 0,
                },
            })
            .unwrap();
        let id = client.next_id - 1;
        client.handle_message(json!({"id":id,"result":[{"title":"stale import","edit":{"documentChanges":[{"textDocument":{"uri":path_to_uri(b),"version":1},"edits":[text_edit(0)]}]}}]}), &mut events);
        let ClientEvent::CodeActions(actions) = events.remove(0) else {
            panic!("expected code actions");
        };
        assert!(actions[0].edits.is_empty() && actions[0].rejected.is_some());
    }

    #[test]
    fn f09_primary_completion_requires_text_and_an_ordered_range() {
        let range = json!({"start":{"line":0,"character":0},"end":{"line":0,"character":2}});
        let items = parse_completion_result(&json!([
            {"label":"fallback must not replace text", "textEdit":{"range":range}},
            {"label":"reversed", "textEdit":{"range":{"start":{"line":1,"character":0},"end":{"line":0,"character":0}},"newText":"wrong"}}
        ]));
        assert!(items.iter().all(|item| item.rejected.is_some()));
    }

    #[test]
    fn f10_late_navigation_replies_cannot_change_the_new_document() {
        let mut client = silent_client(Path::new("/tmp/mellow-a.rs"));
        let position = LspPosition {
            line: 0,
            character: 0,
        };
        let mut ids = Vec::new();
        client.request_definition(position).unwrap();
        ids.push(client.next_id - 1);
        client.request_hover(position).unwrap();
        ids.push(client.next_id - 1);
        client.request_signature_help(position).unwrap();
        ids.push(client.next_id - 1);
        client.request_references(position).unwrap();
        ids.push(client.next_id - 1);
        client
            .activate_document("rust", Path::new("/tmp/mellow-b.rs"), "other", 2)
            .unwrap();
        let mut events = Vec::new();
        for id in ids {
            client.handle_message(json!({"id":id,"result":null}), &mut events);
        }
        assert_eq!(events.len(), 4);
        assert!(
            events
                .iter()
                .all(|event| matches!(event, ClientEvent::Error(_)))
        );
        assert!(client.edit_targets.is_empty());
    }

    #[test]
    fn f09_malformed_supporting_edit_refuses_the_whole_completion() {
        let result =
            json!([{"label": "unsafe", "additionalTextEdits": [{"newText": "import unsafe"}]}]);
        let items = parse_completion_result(&result);
        assert!(format!("{:?}", items[0]).contains("Malformed supporting completion edit"));
    }

    #[test]
    fn completion_parser_honors_text_edit_range_and_plain_insert_text() {
        let result = json!({
            "items": [{
                "label": "println!",
                "detail": "macro",
                "filterText": "print",
                "sortText": "001",
                "insertText": "ignored",
                "textEdit": {
                    "range": {
                        "start": {"line": 4, "character": 2},
                        "end": {"line": 4, "character": 5}
                    },
                    "newText": "println!"
                }
            }]
        });

        let items = parse_completion_result(&result);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert_text, "println!");
        assert_eq!(items[0].filter_text.as_deref(), Some("print"));
        assert_eq!(items[0].sort_text.as_deref(), Some("001"));
        assert_eq!(
            items[0].edit_range,
            Some(LspRange {
                start: LspPosition {
                    line: 4,
                    character: 2
                },
                end: LspPosition {
                    line: 4,
                    character: 5
                }
            })
        );
    }

    #[test]
    fn diagnostics_parser_preserves_severity_source_and_range() {
        let message = json!({
            "params": {
                "uri": "file:///tmp/demo.rs",
                "diagnostics": [{
                    "range": {
                        "start": {"line": 2, "character": 4},
                        "end": {"line": 2, "character": 8}
                    },
                    "severity": 2,
                    "source": "rust-analyzer",
                    "message": "unused variable"
                }]
            }
        });

        let diagnostics = parse_diagnostics(&message);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Warning);
        assert_eq!(diagnostics[0].source.as_deref(), Some("rust-analyzer"));
        assert_eq!(diagnostics[0].message, "unused variable");
    }

    #[test]
    fn definition_parser_accepts_location_links_and_unicode_file_uris() {
        let result = json!([{
            "targetUri": "file:///tmp/My%20Project/%E6%BC%A2.rs",
            "targetSelectionRange": {
                "start": {"line": 10, "character": 3},
                "end": {"line": 10, "character": 7}
            }
        }]);

        let location = parse_definition_result(&result).unwrap();
        assert_eq!(location.path, PathBuf::from("/tmp/My Project/漢.rs"));
        assert_eq!(location.range.start.line, 10);
    }

    #[test]
    fn goal12_hover_signature_and_references_are_bounded_and_parsed() {
        let hover = json!({"contents":{"kind":"markdown","value":"**Vec<T>** docs"}});
        assert_eq!(
            parse_hover_result(&hover).as_deref(),
            Some("**Vec<T>** docs")
        );
        let sig = json!({"signatures":[{"label":"push(&mut self, value: T)"},{"label":"push(T)"}]});
        assert_eq!(parse_signature_result(&sig).len(), 2);
        let refs = json!([{"uri":"file:///tmp/demo.rs","range":{"start":{"line":2,"character":4},"end":{"line":2,"character":7}}}]);
        let locations = parse_locations(&refs);
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, PathBuf::from("/tmp/demo.rs"));
    }

    #[test]
    fn goal12_workspace_edits_and_code_actions_never_hide_commands() {
        let edit = json!({"changes":{"file:///tmp/demo.rs":[{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":3}},"newText":"renamed"}]}});
        let preview = parse_workspace_edit(&edit, None);
        assert_eq!(preview.edits.len(), 1);
        assert_eq!(preview.edits[0].new_text, "renamed");
        assert!(!preview.has_resource_operations);
        let actions = parse_code_actions(
            &json!([{"title":"Fix it","edit":edit},{"title":"Run tool","command":{"command":"dangerous","title":"Run"}}]),
            None,
        );
        assert_eq!(actions.len(), 2);
        assert!(!actions[0].has_command);
        assert!(actions[1].has_command);
        assert!(actions[1].edits.is_empty());
    }

    #[test]
    fn goal16_document_changes_text_edits_are_supported_but_resource_ops_are_flagged() {
        let value = json!({
            "documentChanges": [
                {
                    "textDocument": {"uri": "file:///tmp/one.rs", "version": 2},
                    "edits": [{
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 3}
                        },
                        "newText": "new"
                    }]
                },
                {
                    "kind": "rename",
                    "oldUri": "file:///tmp/old.rs",
                    "newUri": "file:///tmp/new.rs"
                }
            ]
        });
        let preview = parse_workspace_edit(&value, None);
        assert_eq!(preview.edits.len(), 1);
        assert_eq!(preview.edits[0].path, PathBuf::from("/tmp/one.rs"));
        assert!(preview.has_resource_operations);
    }

    #[test]
    fn goal12_formatting_edits_are_tied_to_active_document() {
        let result = json!([{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":3}},"newText":"fn main() {}"}]);
        let edits = parse_document_edits("file:///tmp/demo.rs", &result).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].path, PathBuf::from("/tmp/demo.rs"));
    }

    fn text_edit(line: u64) -> Value {
        json!({"range":{"start":{"line":line,"character":0},"end":{"line":line,"character":1}},"newText":"x"})
    }

    /// Audit: over-limit or malformed proposals must apply zero edits and say
    /// why, never a silently truncated subset.
    #[test]
    fn oversized_or_malformed_edit_sets_are_refused_whole() {
        let many: Vec<Value> = (0..=MAX_TEXT_EDITS as u64).map(text_edit).collect();
        let preview = parse_workspace_edit(&json!({"changes":{"file:///tmp/a.rs": many}}), None);
        assert!(preview.edits.is_empty());
        assert!(preview.rejected.unwrap().contains("more than"));

        let broken = json!([text_edit(0), {"range": null, "newText": "y"}, text_edit(2)]);
        let preview = parse_workspace_edit(
            &json!({"changes":{"file:///tmp/a.rs": broken.clone()}}),
            None,
        );
        assert!(preview.edits.is_empty());
        assert!(preview.rejected.unwrap().contains("malformed"));
        assert!(parse_document_edits("file:///tmp/a.rs", &broken).is_err());

        let actions = parse_code_actions(
            &json!([{"title":"Too big","edit":{"changes":{"file:///tmp/a.rs": broken}}}]),
            None,
        );
        assert!(actions[0].edits.is_empty() && actions[0].rejected.is_some());
    }

    #[test]
    fn edits_for_an_older_version_of_the_open_document_are_refused() {
        let change = |uri: &str, version: i64| json!({"documentChanges":[{"textDocument":{"uri":uri,"version":version},"edits":[text_edit(0)]}]});
        let active = Some(("file:///tmp/open.rs", 5));
        let stale = parse_workspace_edit(&change("file:///tmp/open.rs", 4), active);
        assert!(stale.edits.is_empty() && stale.rejected.unwrap().contains("older version"));
        let current = parse_workspace_edit(&change("file:///tmp/open.rs", 5), active);
        assert_eq!(current.edits.len(), 1);
        let other_file = parse_workspace_edit(&change("file:///tmp/other.rs", 1), active);
        assert_eq!(
            other_file.edits.len(),
            1,
            "other files' versions are the server's"
        );
    }

    #[test]
    fn late_rename_and_code_action_replies_are_discarded_after_a_switch() {
        let a = Path::new("/tmp/mellow-a.rs");
        let b = Path::new("/tmp/mellow-b.rs");
        let mut client = silent_client(a);
        let position = LspPosition {
            line: 0,
            character: 3,
        };
        assert!(client.request_rename(position, "renamed").unwrap());
        let rename_id = client.next_id - 1;
        let range = LspRange {
            start: position,
            end: position,
        };
        assert!(client.request_code_actions(range).unwrap());
        let action_id = client.next_id - 1;
        client
            .activate_document("rust", b, "fn b(){}\n", 1)
            .unwrap();

        let mut events = Vec::new();
        client.handle_message(
            json!({"jsonrpc":"2.0","id":rename_id,"result":{"changes":{"file:///tmp/mellow-a.rs":[text_edit(0)]}}}),
            &mut events,
        );
        client.handle_message(
            json!({"jsonrpc":"2.0","id":action_id,"result":[]}),
            &mut events,
        );
        assert!(
            matches!(&events[..], [ClientEvent::Error(rename), ClientEvent::Error(action)]
                if rename.contains("Rename") && action.contains("Code actions")),
        );
        assert!(client.edit_targets.is_empty());
    }

    /// A client whose "server" never answers, so tests can inject replies.
    fn silent_client(document: &Path) -> LspClient {
        let spec = ServerSpec {
            program: "sleep".to_owned(),
            args: vec!["60".to_owned()],
            language_id: "rust",
            label: "test server",
        };
        let mut client =
            LspClient::spawn(&spec, Path::new("/tmp"), document, "fn a(){}\n", 1).unwrap();
        client.initialized = true;
        client
    }

    fn format_reply(id: u64) -> Value {
        json!({"jsonrpc":"2.0","id":id,"result":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":8}},"newText":"fn a() {}"}]})
    }

    fn formatting_events(client: &mut LspClient, id: u64) -> Vec<ClientEvent> {
        let mut events = Vec::new();
        client.handle_message(format_reply(id), &mut events);
        events
    }

    /// Audit F3: a late formatting reply must never land on another file or
    /// on text that changed after the request.
    #[test]
    fn audit_f3_late_formatting_reply_only_applies_to_its_unchanged_document() {
        let a = Path::new("/tmp/mellow-a.rs");
        let b = Path::new("/tmp/mellow-b.rs");

        let mut client = silent_client(a);
        assert!(client.request_formatting(4, true).unwrap());
        let id = client.next_id - 1;
        let events = formatting_events(&mut client, id);
        assert!(
            matches!(&events[..], [ClientEvent::Formatting(edits)] if edits[0].path == a),
            "an on-time reply formats the requested file"
        );

        assert!(client.request_formatting(4, true).unwrap());
        let id = client.next_id - 1;
        client
            .activate_document("rust", b, "fn b(){}\n", 1)
            .unwrap();
        let events = formatting_events(&mut client, id);
        assert!(
            matches!(&events[..], [ClientEvent::Error(reason)] if reason.contains("discarded")),
            "switching A -> B must not apply A's edits to B"
        );

        client
            .activate_document("rust", a, "fn a(){}\n", 1)
            .unwrap();
        assert!(client.request_formatting(4, true).unwrap());
        let id = client.next_id - 1;
        client
            .activate_document("rust", a, "fn a(){ 1 }\n", 2)
            .unwrap();
        let events = formatting_events(&mut client, id);
        assert!(
            matches!(&events[..], [ClientEvent::Error(_)]),
            "edits computed for version 1 must not apply to version 2"
        );
        assert!(client.edit_targets.is_empty());
    }

    #[test]
    fn health_reports_capabilities_install_state_and_setup_hints() {
        let capabilities = announced_capabilities(&json!({
            "completionProvider": {"triggerCharacters": ["."]},
            "renameProvider": true,
            "hoverProvider": false,
            "documentFormattingProvider": {}
        }));
        assert_eq!(capabilities, ["completion", "rename", "formatting"]);

        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("fake-server");
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        assert!(
            find_program(program.to_str().unwrap()).is_none(),
            "not executable"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(
                find_program(program.to_str().unwrap()),
                Some(program.clone())
            );
        }
        assert!(find_program("definitely-not-a-real-mellow-server").is_none());

        let service = LanguageService::default();
        let health = service.health("Plain Text");
        assert_eq!(health.state, ServerState::NoServerForLanguage);
        assert!(health.install_hint.is_none());

        let health = service.health("Terraform");
        assert_eq!(health.override_variable, Some("MELLOW_LSP_TERRAFORM"));
        if health.installed.is_none() {
            assert_eq!(health.state, ServerState::NotInstalled);
            assert!(health.install_hint.unwrap().contains("terraform-ls"));
        } else {
            assert_eq!(health.state, ServerState::NotStarted);
        }
    }

    #[test]
    fn language_server_specs_cover_primary_mellow_languages() {
        for language in [
            "Rust",
            "Python",
            "TypeScript",
            "JavaScript",
            "Go",
            "Terraform",
            "YAML",
            "JSON",
            "Shell",
        ] {
            assert!(
                server_spec(language).is_some(),
                "{language} should map to an LSP"
            );
        }
        assert!(server_spec("Plain Text").is_none());
    }

    #[test]
    fn file_uri_round_trip_preserves_spaces_and_unicode() {
        let path = PathBuf::from("/tmp/Mellow Project/తెలుగు.rs");
        let uri = path_to_uri(&path);
        assert!(uri.starts_with("file:///"));
        assert_eq!(uri_to_path(&uri), Some(path));
    }
}
