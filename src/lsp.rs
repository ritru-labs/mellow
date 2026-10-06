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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub filter_text: Option<String>,
    pub sort_text: Option<String>,
    pub insert_text: String,
    pub edit_range: Option<LspRange>,
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
    CompletionError {
        request_id: u64,
        error: String,
    },
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
    next_id: u64,
    initialized: bool,
    server_label: String,
    server_program: String,
    server_args: Vec<String>,
    opened_documents: HashMap<String, (i32, String)>,
    completion_trigger_characters: Vec<String>,
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
            next_id: 1,
            initialized: false,
            server_label: spec.label.to_owned(),
            server_program: spec.program.clone(),
            server_args: spec.args.clone(),
            opened_documents: HashMap::new(),
            completion_trigger_characters: Vec::new(),
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
                    "configuration": true
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
                            "documentationFormat": ["plaintext", "markdown"]
                        }
                    },
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
        Ok(Some(id))
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
        self.send_request(
            kind,
            method,
            json!({
                "textDocument": { "uri": self.document.uri },
                "position": position_json(position)
            }),
        )
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
        self.send_request(
            RequestKind::References,
            "textDocument/references",
            json!({
                "textDocument": { "uri": self.document.uri }, "position": position_json(position),
                "context": { "includeDeclaration": true }
            }),
        )?;
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
        self.edit_targets
            .insert(id, (self.document.uri.clone(), self.document.version));
    }

    fn close_path(&mut self, path: &Path) -> Result<()> {
        let uri = path_to_uri(path);
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
                && self.opened_documents.contains_key(uri)
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
            if kind == RequestKind::Completion {
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
                self.initialized = true;
                if self.send_notification("initialized", json!({})).is_err()
                    || self.did_open().is_err()
                {
                    events.push(ClientEvent::Error(
                        "failed to finish language server initialization".to_owned(),
                    ));
                } else {
                    events.push(ClientEvent::Ready);
                }
            }
            RequestKind::Completion => {
                events.push(ClientEvent::Completions(
                    id,
                    parse_completion_result(message.get("result").unwrap_or(&Value::Null)),
                ));
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
            RequestKind::Rename => events.push(ClientEvent::Rename(parse_workspace_edit(
                message.get("result").unwrap_or(&Value::Null),
                active,
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
            RequestKind::CodeAction => events.push(ClientEvent::CodeActions(parse_code_actions(
                message.get("result").unwrap_or(&Value::Null),
                active,
            ))),
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
    CompletionError(u64, String),
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
            self.stop();
            return;
        };

        let version = version_from_revision(revision);
        if let Some(client) = self.client.as_mut()
            && client.matches_server(&spec)
        {
            if let Err(error) = client.activate_document(spec.language_id, path, text, version) {
                self.last_error = Some(format!(
                    "{} document activation failed: {error}",
                    spec.label
                ));
            }
            return;
        }

        self.stop();
        match LspClient::spawn(&spec, workspace_root, path, text, version) {
            Ok(client) => self.client = Some(client),
            Err(error) => {
                self.last_error = Some(format!("{} unavailable: {error}", spec.label));
            }
        }
    }

    pub fn sync_document(&mut self, path: Option<&Path>, text: &str, revision: u64) {
        let Some(client) = self.client.as_mut() else {
            return;
        };
        if path != self.current_path.as_deref() {
            return;
        }

        if let Err(error) = client.did_change(text, version_from_revision(revision)) {
            self.last_error = Some(format!("language server sync failed: {error}"));
        }
    }

    pub fn close_document(&mut self, path: Option<&Path>) {
        let Some(path) = path else {
            return;
        };
        if let Some(client) = self.client.as_mut()
            && let Err(error) = client.close_path(path)
        {
            self.last_error = Some(format!("language server close failed: {error}"));
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

    pub fn completion_trigger_characters(&self) -> &[String] {
        self.client
            .as_ref()
            .map(|client| client.completion_trigger_characters.as_slice())
            .unwrap_or(&[])
    }

    pub fn request_definition(&mut self, position: LspPosition) -> Result<bool> {
        let Some(client) = self.client.as_mut() else {
            return Ok(false);
        };
        client.request_definition(position)
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
        let Some(client) = self.client.as_mut() else {
            return Vec::new();
        };

        let label = client.server_label.clone();
        let mut output = Vec::new();
        let mut exited = false;
        for event in client.poll() {
            match event {
                ClientEvent::Ready => {
                    output.push(LanguageServiceEvent::Ready(label.clone()));
                }
                ClientEvent::Diagnostics(uri, diagnostics) => {
                    if let Some(path) = uri_to_path(&uri) {
                        let is_current = self.current_path.as_ref().is_some_and(|current| {
                            std::fs::canonicalize(current).unwrap_or_else(|_| current.clone())
                                == std::fs::canonicalize(&path).unwrap_or(path.clone())
                        });
                        self.diagnostics_by_path.insert(path, diagnostics);
                        if is_current {
                            output.push(LanguageServiceEvent::DiagnosticsChanged);
                        }
                    }
                }
                ClientEvent::Completions(request_id, items) => {
                    output.push(LanguageServiceEvent::Completions { request_id, items });
                }
                ClientEvent::CompletionError(request_id, error) => {
                    output.push(LanguageServiceEvent::CompletionError { request_id, error });
                }
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
    Some(LspRange {
        start: parse_position(value.get("start")?)?,
        end: parse_position(value.get("end")?)?,
    })
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

            Some(CompletionItem {
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
    for edit in edits.as_array().into_iter().flatten() {
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
        if let Some(changes) = result.get("changes").and_then(Value::as_object) {
            for (uri, edits) in changes {
                let Some(path) = uri_to_path(uri) else {
                    has_resource_operations = true;
                    continue;
                };
                push_text_edits(&path, edits, &mut output)?;
            }
        }
        if let Some(document_changes) = result.get("documentChanges").and_then(Value::as_array) {
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

fn parse_code_actions(result: &Value, active: Option<(&str, i32)>) -> Vec<CodeActionItem> {
    result
        .as_array()
        .into_iter()
        .flatten()
        .take(200)
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.to_owned();
            let preview = item
                .get("edit")
                .map(|edit| parse_workspace_edit(edit, active))
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
sleep 6
"#;
        let spec = ServerSpec {
            program: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            language_id: "rust",
            label: "stalled-lsp",
        };

        let started = Instant::now();
        let mut client = LspClient::spawn(&spec, dir.path(), &document_path, &text, 1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !client.initialized && Instant::now() < deadline {
            client.poll(); // sends the 2 MiB didOpen the server never reads
            thread::sleep(Duration::from_millis(10));
        }
        assert!(client.initialized, "fake server never initialized");
        for version in 2..40 {
            client.did_change(&text, version).unwrap();
        }
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "editor calls blocked on the server for {:?}",
            started.elapsed()
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
