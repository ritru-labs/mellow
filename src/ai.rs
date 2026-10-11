use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::claude;

const MAX_CONTEXT_CHARS: usize = 32_000;
const MAX_RESPONSE_CHARS: usize = 64_000;
const MAX_INSTRUCTION_CHARS: usize = 8_000;
const MAX_ENVELOPE_BYTES: usize = 512 * 1024;

/// Conservative default privacy boundary. Refuse rather than silently change
/// selected text: a redacted selection cannot safely be replaced by a model.
pub fn context_problem(path: &str, text: &str) -> Option<&'static str> {
    if excluded_path(path)
        || fs::canonicalize(path).is_ok_and(|canonical| excluded_path(&canonical.to_string_lossy()))
    {
        return Some("this file is excluded from AI context by the default privacy policy");
    }
    match configured_deny_problem(&crate::settings::config_root().join("ai-deny.conf"), path) {
        Ok(true) => return Some("this file is excluded from AI context by ai-deny.conf"),
        Err(_) => {
            return Some(
                "AI privacy policy could not be read or parsed; fix ai-deny.conf before sending context",
            );
        }
        Ok(false) => {}
    }
    if secret_like(text) {
        return Some("context appears to contain a credential; remove it before asking AI");
    }
    None
}

fn configured_deny_problem(policy_path: &Path, path: &str) -> Result<bool> {
    use std::io::Read;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = match options.open(policy_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > 65536 {
        bail!("AI deny policy must be a regular file of at most 64 KiB");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            bail!("AI deny policy must be owned by this user and not writable by other users");
        }
    }
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        bail!("AI deny policy exceeds 64 KiB");
    }
    let patterns = deny_patterns(std::str::from_utf8(&bytes)?)?;
    let canonical = fs::canonicalize(path).ok();
    Ok(deny_matches(&patterns, path)
        || canonical.is_some_and(|canonical| deny_matches(&patterns, &canonical.to_string_lossy())))
}

fn deny_patterns(text: &str) -> Result<Vec<regex::Regex>> {
    let mut patterns = Vec::new();
    for line in text.lines() {
        let pattern = line.trim();
        if pattern.is_empty() || pattern.starts_with('#') {
            continue;
        }
        if patterns.len() >= 256 || pattern.len() > 1024 {
            bail!("AI deny policy pattern limit exceeded");
        }
        if pattern
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '[' | ']' | '{' | '}' | '\\'))
            || pattern.starts_with('!')
        {
            bail!("unsupported AI deny glob; only *, ** and ? wildcards are supported");
        }
        let mut expression = String::from("(?:^|/)");
        let mut chars = pattern.trim_start_matches('/').chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '*' if chars.peek() == Some(&'*') => {
                    chars.next();
                    if chars.peek() == Some(&'/') {
                        chars.next();
                        expression.push_str("(?:.*/)?");
                    } else {
                        expression.push_str(".*");
                    }
                }
                '*' => expression.push_str("[^/]*"),
                '?' => expression.push_str("[^/]"),
                _ => expression.push_str(&regex::escape(&ch.to_string())),
            }
        }
        expression.push('$');
        patterns.push(regex::Regex::new(&expression)?);
    }
    Ok(patterns)
}

fn deny_matches(patterns: &[regex::Regex], path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let normalized = path.replace('\\', "/");
    patterns.iter().any(|pattern| pattern.is_match(&normalized))
}

fn excluded_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    let name = normalized.rsplit('/').next().unwrap_or("");
    name == ".env"
        || (name.starts_with(".env.") && !name.ends_with(".example") && !name.ends_with(".sample"))
        || matches!(
            name,
            "credentials"
                | "id_rsa"
                | "id_ed25519"
                | "id_ecdsa"
                | "id_dsa"
                | "secrets.json"
                | "secrets.yaml"
                | "secrets.yml"
                | "secrets.toml"
                | "secrets.conf"
        )
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || normalized.starts_with(".ssh/")
        || normalized.starts_with(".aws/")
        || normalized.contains("/.ssh/")
        || normalized.contains("/.aws/")
}

fn secret_like(text: &str) -> bool {
    static SECRETS: OnceLock<regex::Regex> = OnceLock::new();
    SECRETS.get_or_init(|| regex::Regex::new(
        r#"(?im)-----BEGIN (?:[A-Z ]*PRIVATE KEY)-----|\b(?:AKIA|ASIA)[A-Z0-9]{16}\b|\bgh[pousr]_[A-Za-z0-9]{20,}\b|(?:api[_-]?key|access[_-]?token|auth[_-]?token|secret|password)["']?\s*[=:]\s*["']?[^\s"',;}]{3,}"#
    ).expect("built-in credential pattern is valid")).is_match(text)
}

fn check_instruction(instruction: &str) -> Result<()> {
    if instruction.trim().is_empty() {
        bail!("AI instruction cannot be empty");
    }
    if instruction.chars().count() > MAX_INSTRUCTION_CHARS {
        bail!("AI instruction is too large (limit {MAX_INSTRUCTION_CHARS} characters)");
    }
    if let Some(problem) = context_problem("", instruction) {
        bail!("AI context refused: {problem}");
    }
    Ok(())
}

pub(crate) fn bounded_output(text: String) -> Result<String> {
    if text.chars().count() > MAX_RESPONSE_CHARS {
        bail!("AI reply is too long (limit {MAX_RESPONSE_CHARS} characters); nothing was changed");
    }
    Ok(text)
}

/// Provider error bodies can echo credentials. Never include them in a status
/// message, terminal capture or panic; status codes are enough to diagnose.
pub(crate) fn provider_error(
    config: &AiProviderConfig,
    status: reqwest::StatusCode,
) -> anyhow::Error {
    anyhow::anyhow!(
        "{} returned {status}; provider error details withheld to protect credentials",
        config.provider.short_label()
    )
}

/// Keys and code only travel over HTTPS; plain HTTP is allowed for a model
/// running on this computer (Ollama and similar).
pub fn endpoint_problem(endpoint: &str) -> Option<&'static str> {
    if endpoint.chars().any(char::is_control) {
        return Some("The provider address must not contain control characters");
    }
    let Ok(url) = reqwest::Url::parse(endpoint) else {
        return Some("The address must be a valid https:// URL (http:// only for loopback)");
    };
    if !url.username().is_empty() || url.password().is_some() {
        return Some("Use the separate API key field; credentials in an endpoint URL are refused");
    }
    if url.fragment().is_some() {
        return Some("The provider address must not contain a fragment");
    }
    if let Some(rest) = endpoint.strip_prefix("http://") {
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
        let name = if host.starts_with('[') {
            host.split(']').next().unwrap_or("").trim_start_matches('[')
        } else {
            host.split(':').next().unwrap_or("")
        };
        // A real address check: "127." as a prefix also matched host names
        // such as 127.example.com, sending the key over plain HTTP.
        let loopback = name.eq_ignore_ascii_case("localhost")
            || name
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        return (!loopback).then_some("Use https:// (plain http:// is only allowed for localhost)");
    }
    (!endpoint.starts_with("https://")).then_some("The address must start with https://")
}
const ASK_TIMEOUT: Duration = Duration::from_secs(120);
const INLINE_TIMEOUT: Duration = Duration::from_secs(20);

/// Where requests go. Claude uses the Anthropic Messages API; the others
/// speak the OpenAI-compatible chat-completions protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiProvider {
    Claude,
    OpenAi,
    Gemini,
    Ollama,
    Custom,
}

impl AiProvider {
    pub const ALL: [Self; 5] = [
        Self::Claude,
        Self::OpenAi,
        Self::Gemini,
        Self::Ollama,
        Self::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude (Anthropic)",
            Self::OpenAi => "OpenAI",
            Self::Gemini => "Gemini (Google)",
            Self::Ollama => "Ollama (runs on this computer)",
            Self::Custom => "Other (OpenAI-compatible)",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::OpenAi => "OpenAI",
            Self::Gemini => "Gemini",
            Self::Ollama => "Ollama",
            Self::Custom => "AI",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
            Self::Ollama => "ollama",
            Self::Custom => "custom",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|provider| provider.id() == id.trim().to_ascii_lowercase())
    }

    pub fn default_endpoint(self) -> &'static str {
        match self {
            Self::Claude => claude::MESSAGES_ENDPOINT,
            Self::OpenAi => "https://api.openai.com/v1/chat/completions",
            // Google's OpenAI-compatible endpoint, so the shared adapter works.
            Self::Gemini => {
                "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
            }
            Self::Ollama => "http://localhost:11434/v1/chat/completions",
            Self::Custom => "",
        }
    }

    /// Prefilled model name; empty when the user must pick one.
    pub fn default_model(self) -> &'static str {
        match self {
            Self::Claude => claude::DEFAULT_MODEL,
            Self::Gemini => "gemini-2.5-flash",
            _ => "",
        }
    }

    pub fn needs_api_key(self) -> bool {
        matches!(self, Self::Claude | Self::OpenAi | Self::Gemini)
    }

    /// Environment variable that already holds this provider's key, if any.
    pub fn key_env_var(self) -> Option<&'static str> {
        match self {
            Self::Claude => Some("ANTHROPIC_API_KEY"),
            Self::OpenAi => Some("OPENAI_API_KEY"),
            Self::Gemini => Some("GEMINI_API_KEY"),
            _ => None,
        }
    }

    fn infer(endpoint: &str) -> Self {
        if endpoint.contains("api.anthropic.com") {
            Self::Claude
        } else if endpoint.contains("api.openai.com") {
            Self::OpenAi
        } else if endpoint.contains("generativelanguage.googleapis.com") {
            Self::Gemini
        } else if endpoint.contains(":11434") {
            Self::Ollama
        } else {
            Self::Custom
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AiProviderConfig {
    pub provider: AiProvider,
    pub endpoint: String,
    pub model: String,
    pub api_key: Option<String>,
    /// Ask for grey suggestions after a typing pause. Off unless chosen,
    /// because it sends nearby code without an explicit request.
    pub inline_suggestions: bool,
}

// The key must never reach a log or a panic message, so Debug hides it.
impl std::fmt::Debug for AiProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiProviderConfig")
            .field("provider", &self.provider)
            .field("endpoint_host", &endpoint_host(&self.endpoint))
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("inline_suggestions", &self.inline_suggestions)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiRequest {
    pub instruction: String,
    pub path: String,
    pub language: String,
    pub context: String,
    pub allow_replacement: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiProposal {
    pub summary: String,
    pub replacement: Option<String>,
}

/// Code around the cursor for a typing suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineRequest {
    pub path: String,
    pub language: String,
    pub before: String,
    pub after: String,
}

/// Deletes the saved AI setup, including any stored key.
pub fn remove_config() -> Result<()> {
    let path = config_path();
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

/// The host an endpoint sends requests (and the key) to, for display.
pub fn endpoint_host(endpoint: &str) -> Option<&str> {
    let rest = endpoint.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    (!host.is_empty()).then_some(host)
}

pub fn config_path() -> PathBuf {
    if cfg!(test) {
        // Unit tests must never touch the user's real AI setup, nor each
        // other's: tests run in parallel threads.
        let thread = format!("{:?}", std::thread::current().id())
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>();
        return env::temp_dir().join(format!(
            "mellow-test-ai-{}-{thread}.conf",
            std::process::id()
        ));
    }
    if let Some(path) = crate::brand::env_var_os("AI_CONFIG") {
        return PathBuf::from(path);
    }
    crate::settings::config_root().join("ai.conf")
}

impl AiProviderConfig {
    /// Environment variables win (for scripts and CI); otherwise the file
    /// written by the in-app setup.
    pub fn load() -> Result<Option<Self>> {
        if let Some(config) = Self::from_env()? {
            return Ok(Some(config));
        }
        Self::load_from(&config_path())
    }

    pub fn from_env() -> Result<Option<Self>> {
        let Some(endpoint) = crate::brand::env_var("AI_ENDPOINT")
            .ok()
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(None);
        };
        if let Some(problem) = endpoint_problem(&endpoint) {
            bail!("MELLOW_AI_ENDPOINT: {problem}");
        }
        let model = crate::brand::env_var("AI_MODEL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .context("MELLOW_AI_MODEL is required when MELLOW_AI_ENDPOINT is configured")?;
        let provider = crate::brand::env_var("AI_PROVIDER")
            .ok()
            .and_then(|id| AiProvider::from_id(&id))
            .unwrap_or_else(|| AiProvider::infer(&endpoint));
        let api_key = crate::brand::env_var("AI_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| provider.key_env_var().and_then(|name| env::var(name).ok()))
            .filter(|value| !value.trim().is_empty());
        let inline_suggestions = matches!(
            crate::brand::env_var("AI_INLINE").as_deref(),
            Ok("1" | "on" | "true")
        );
        Ok(Some(Self {
            provider,
            endpoint,
            model,
            api_key,
            inline_suggestions,
        }))
    }

    pub fn load_from(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let mut provider = None;
        let mut endpoint = None;
        let mut model = None;
        let mut api_key = None;
        let mut inline_suggestions = false;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().to_owned();
            match key.trim() {
                "provider" => provider = AiProvider::from_id(&value),
                "endpoint" => endpoint = Some(value),
                "model" => model = Some(value),
                "api_key" => api_key = Some(value).filter(|value| !value.is_empty()),
                "inline_suggestions" => {
                    inline_suggestions = matches!(value.as_str(), "true" | "on" | "1")
                }
                _ => {}
            }
        }
        let Some(endpoint) = endpoint.filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let provider = provider.unwrap_or_else(|| AiProvider::infer(&endpoint));
        let Some(model) = model.filter(|value| !value.is_empty()) else {
            bail!("{} has no model", path.display());
        };
        let api_key = api_key.or_else(|| {
            provider
                .key_env_var()
                .and_then(|name| env::var(name).ok())
                .filter(|value| !value.trim().is_empty())
        });
        Ok(Some(Self {
            provider,
            endpoint,
            model,
            api_key,
            inline_suggestions,
        }))
    }

    /// Writes the setup to `path`, readable only by the current user, since
    /// it may hold an API key.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let text = format!(
            "# Mellow AI setup. Written by Settings > AI; safe to edit.\n\
             provider = {}\nendpoint = {}\nmodel = {}\napi_key = {}\ninline_suggestions = {}\n",
            self.provider.id(),
            self.endpoint,
            self.model,
            self.api_key.as_deref().unwrap_or(""),
            self.inline_suggestions
        );
        // Write a private temporary file and rename it over the old one, so
        // an interrupted save never leaves a half-written key file.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| -> Result<()> {
            let mut file = options
                .open(&temp)
                .with_context(|| format!("failed to write {}", temp.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            use std::io::Write;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temp, path)
                .with_context(|| format!("failed to replace {}", path.display()))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    /// Short description for the status line and setup screen.
    pub fn summary(&self) -> String {
        format!("{} · {}", self.provider.short_label(), self.model)
    }
}

const ASK_SYSTEM: &str = concat!(
    "You are Mellow's optional code assistant inside a terminal text editor. ",
    "Never claim to execute commands, tools, Git, shell, or files. ",
    "Return ONLY JSON with keys summary and replacement. ",
    "summary is a concise plain-text answer for the user. ",
    "replacement must be null unless a replacement for the selected text is appropriate; ",
    "when present it is the complete new text for the selection, with no Markdown fences. ",
    "Do not wrap the JSON in Markdown."
);

const INLINE_SYSTEM: &str = concat!(
    "You complete code in a text editor. The user message contains the text before ",
    "the cursor and the text after it. Reply with ONLY the text to insert at the cursor: ",
    "no explanation, no Markdown fences, no repetition of existing text. Prefer finishing ",
    "the current line or adding a few lines. Reply with nothing if no confident completion exists."
);

#[cfg(test)]
pub fn request(config: &AiProviderConfig, request: &AiRequest) -> Result<AiProposal> {
    Ok(request_cancellable(
        config,
        request,
        &std::sync::atomic::AtomicBool::new(false),
        16_000,
        None,
    )?
    .value)
}

pub fn request_cancellable(
    config: &AiProviderConfig,
    request: &AiRequest,
    cancelled: &std::sync::atomic::AtomicBool,
    max_output_tokens: u32,
    progress: Option<&std::sync::mpsc::Sender<String>>,
) -> Result<AiResponse<AiProposal>> {
    check_instruction(&request.instruction)?;
    if let Some(problem) = context_problem(&request.path, &request.context) {
        bail!("AI context refused: {problem}");
    }

    if let Some(problem) = endpoint_problem(&config.endpoint) {
        bail!("AI address refused: {problem}");
    }
    // Never send part of a selection: the reply would replace all of it.
    let context_chars = request.context.chars().count();
    if context_chars > MAX_CONTEXT_CHARS {
        bail!(
            "selection is too large for AI ({context_chars} characters; limit {MAX_CONTEXT_CHARS}). Select a smaller part"
        );
    }
    let context = &request.context;
    let user_payload = json!({
        "instruction": request.instruction,
        "file": request.path,
        "language": request.language,
        "context": context,
        "replacement_allowed": request.allow_replacement,
    })
    .to_string();

    let result = exchange_cancellable(
        config,
        &claude::Exchange {
            system: ASK_SYSTEM,
            user: &user_payload,
            max_tokens: max_output_tokens,
            effort: "medium",
            timeout: ASK_TIMEOUT,
        },
        cancelled,
        progress,
    )?;
    Ok(AiResponse {
        value: parse_proposal(&result.value, request.allow_replacement)
            .map_err(|error| with_usage(error, result.usage))?,
        usage: result.usage,
    })
}

/// A short continuation for the cursor position, or an empty string.
pub fn complete_inline_cancellable(
    config: &AiProviderConfig,
    request: &InlineRequest,
    cancelled: &std::sync::atomic::AtomicBool,
    max_output_tokens: u32,
    progress: Option<&std::sync::mpsc::Sender<String>>,
) -> Result<AiResponse<String>> {
    if let Some(problem) = endpoint_problem(&config.endpoint) {
        bail!("AI address refused: {problem}");
    }
    let before: String = request
        .before
        .chars()
        .rev()
        .take(6_000)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let after: String = request.after.chars().take(2_000).collect();
    if let Some(problem) =
        context_problem(&request.path, &before).or_else(|| context_problem(&request.path, &after))
    {
        bail!("AI context refused: {problem}");
    }
    let user = format!(
        "File: {}\nLanguage: {}\n<before_cursor>\n{}</before_cursor>\n<after_cursor>\n{}</after_cursor>",
        request.path, request.language, before, after
    );
    let result = exchange_cancellable(
        config,
        &claude::Exchange {
            system: INLINE_SYSTEM,
            user: &user,
            max_tokens: max_output_tokens.min(1024),
            effort: "low",
            timeout: INLINE_TIMEOUT,
        },
        cancelled,
        progress,
    )?;
    Ok(AiResponse {
        value: clean_inline(&result.value),
        usage: result.usage,
    })
}

const COMMIT_SYSTEM: &str = concat!(
    "You write Git commit messages from a staged diff. ",
    "Reply with the commit message only: no code fences, quotes or commentary. ",
    "Line 1 is an imperative summary under 72 characters. ",
    "Optionally add a blank line and a short body of plain sentences about why. ",
    "Describe only what the diff changes."
);
/// Longer diffs are cut so one request stays small; the model is told.
const COMMIT_DIFF_CHARS: usize = 12_000;

/// Drafts a commit message for staged changes. Nothing is committed here;
/// the caller shows the draft for review.
#[cfg(test)]
pub fn commit_message(config: &AiProviderConfig, diff: &str) -> Result<String> {
    Ok(commit_message_cancellable(
        config,
        diff,
        &std::env::current_dir()?,
        &std::sync::atomic::AtomicBool::new(false),
        1024,
        None,
    )?
    .value)
}

pub fn commit_message_cancellable(
    config: &AiProviderConfig,
    diff: &str,
    workspace: &Path,
    cancelled: &std::sync::atomic::AtomicBool,
    max_output_tokens: u32,
    progress: Option<&std::sync::mpsc::Sender<String>>,
) -> Result<AiResponse<String>> {
    if let Some(problem) = context_problem("", diff) {
        bail!("AI context refused: {problem}");
    }
    if secret_like(diff)
        || diff.lines().any(|line| {
            let path = line
                .strip_prefix("+++ ")
                .or_else(|| line.strip_prefix("--- "))
                .or_else(|| line.strip_prefix("rename from "))
                .or_else(|| line.strip_prefix("rename to "));
            let Some(path) = path else {
                return false;
            };
            let path = if path.starts_with('"') {
                match serde_json::from_str::<String>(path) {
                    Ok(path) => path,
                    // Git can also use C-style octal escapes. Refuse ambiguous
                    // quoted paths rather than bypass the deny policy.
                    Err(_) => return true,
                }
            } else {
                path.to_owned()
            };
            let path = path
                .strip_prefix("a/")
                .or_else(|| path.strip_prefix("b/"))
                .unwrap_or(&path);
            context_problem(path, "").is_some()
                || context_problem(&workspace.join(path).to_string_lossy(), "").is_some()
        })
    {
        bail!(
            "AI context refused: staged diff includes excluded files or credentials; review and remove them first"
        );
    }
    if let Some(problem) = endpoint_problem(&config.endpoint) {
        bail!("AI address refused: {problem}");
    }
    let shown: String = diff.chars().take(COMMIT_DIFF_CHARS).collect();
    let note = if diff.chars().count() > COMMIT_DIFF_CHARS {
        "\n(diff truncated)"
    } else {
        ""
    };
    let user = format!("Staged diff:\n{shown}{note}");
    let result = exchange_cancellable(
        config,
        &claude::Exchange {
            system: COMMIT_SYSTEM,
            user: &user,
            max_tokens: max_output_tokens.min(1024),
            effort: "low",
            timeout: ASK_TIMEOUT,
        },
        cancelled,
        progress,
    )?;
    Ok(AiResponse {
        value: clean_commit_message(&result.value),
        usage: result.usage,
    })
}

const SHELL_SYSTEM: &str = concat!(
    "You turn a request into one shell command for a POSIX shell (bash or zsh). ",
    "Reply with the command only: one line, no code fences, no explanation, no leading prompt. ",
    "Never use sudo. Never delete or overwrite files unless the request clearly asks for it."
);
const SHELL_TIMEOUT: Duration = Duration::from_secs(60);

/// Turns a plain-language request into one shell command for read-only review.
/// The editor never inserts the response into an interactive PTY.
#[cfg(test)]
pub fn shell_command(config: &AiProviderConfig, request: &str) -> Result<String> {
    Ok(shell_command_cancellable(
        config,
        request,
        &std::sync::atomic::AtomicBool::new(false),
        256,
        None,
    )?
    .value)
}

pub fn shell_command_cancellable(
    config: &AiProviderConfig,
    request: &str,
    cancelled: &std::sync::atomic::AtomicBool,
    max_output_tokens: u32,
    progress: Option<&std::sync::mpsc::Sender<String>>,
) -> Result<AiResponse<String>> {
    check_instruction(request)?;
    let result = exchange_cancellable(
        config,
        &claude::Exchange {
            system: SHELL_SYSTEM,
            user: request,
            max_tokens: max_output_tokens.min(256),
            effort: "low",
            timeout: SHELL_TIMEOUT,
        },
        cancelled,
        progress,
    )?;
    Ok(AiResponse {
        value: clean_shell_command(&result.value)
            .map_err(|error| with_usage(error, result.usage))?,
        usage: result.usage,
    })
}

/// Commands are data for review, never PTY input. Reject controls before
/// trimming: CR/LF, ESC, NUL and C1 can submit or alter an interactive terminal.
fn clean_shell_command(text: &str) -> Result<String> {
    if text.chars().any(char::is_control) {
        bail!("Control bytes refused in AI shell command; nothing was inserted");
    }
    let line = text.trim().trim_matches('`').trim();
    let line = line.strip_prefix("$ ").unwrap_or(line).trim();
    if line.is_empty() {
        bail!("the AI did not suggest a command");
    }
    Ok(line.to_owned())
}

#[cfg(test)]
mod shell_command_tests {
    use super::clean_shell_command;

    #[test]
    fn shell_commands_reject_every_terminal_control_before_cleaning() {
        for control in (0u32..=31).chain(127..=159) {
            let hostile = format!("printf audit{}# ignored", char::from_u32(control).unwrap());
            assert!(
                clean_shell_command(&hostile).is_err(),
                "accepted U+{control:04X}"
            );
        }
        assert!(clean_shell_command("echo safe\necho unsafe").is_err());
        assert!(clean_shell_command("echo safe\r").is_err());
        assert_eq!(
            clean_shell_command("$ printf 'hello'").unwrap(),
            "printf 'hello'"
        );
    }

    #[test]
    fn keeps_one_command_and_drops_fences_and_prompts() {
        assert_eq!(clean_shell_command("$ du -sh *").unwrap(), "du -sh *");
        assert!(clean_shell_command("\n\n").is_err());
    }
}

/// Removes code fences and wrapping quotes from a drafted message.
fn clean_commit_message(text: &str) -> String {
    let body: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .collect();
    body.join("\n").trim().trim_matches('"').trim().to_owned()
}

#[cfg(test)]
mod commit_message_tests {
    use super::clean_commit_message;

    #[test]
    fn strips_fences_and_quotes_from_a_drafted_message() {
        let raw = "```\n\"Add staged diff reader\n\nRead the index without locks.\"\n```\n";
        assert_eq!(
            clean_commit_message(raw),
            "Add staged diff reader\n\nRead the index without locks."
        );
    }
}

/// Strips fences and limits a suggestion to a few lines.
fn clean_inline(text: &str) -> String {
    let mut body = text.trim_end_matches(['\n', '\r', ' ']).to_owned();
    if body.trim_start().starts_with("```") {
        body = body
            .lines()
            .filter(|line| !line.trim_start().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n");
    }
    body.lines().take(8).collect::<Vec<_>>().join("\n")
}

pub(crate) fn connection_error(config: &AiProviderConfig, error: &reqwest::Error) -> anyhow::Error {
    if error.is_timeout() {
        anyhow::anyhow!("{} took too long to answer", config.provider.short_label())
    } else if error.is_connect() {
        anyhow::anyhow!(
            "could not reach {} at {} (offline?)",
            config.provider.short_label(),
            endpoint_host(&config.endpoint).unwrap_or("configured provider")
        )
    } else {
        anyhow::anyhow!(
            "request to {} failed; transport details withheld to protect credentials",
            config.provider.short_label()
        )
    }
}

fn parse_proposal(content: &str, allow_replacement: bool) -> Result<AiProposal> {
    let trimmed = content.trim();
    let start = trimmed
        .find('{')
        .context("AI response did not contain JSON")?;
    let end = trimmed
        .rfind('}')
        .context("AI response did not contain complete JSON")?;
    let candidate = &trimmed[start..=end];

    let value: Value = serde_json::from_str(candidate).context("AI response JSON is invalid")?;
    let summary = value
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("AI response received")
        .chars()
        .take(MAX_RESPONSE_CHARS)
        .collect::<String>();
    let replacement = if allow_replacement {
        value
            .get("replacement")
            .and_then(Value::as_str)
            .map(str::to_owned)
    } else {
        None
    };
    // A clipped replacement would look complete; refuse it instead.
    if let Some(text) = &replacement
        && text.chars().count() > MAX_RESPONSE_CHARS
    {
        bail!("AI replacement is too long ({MAX_RESPONSE_CHARS}+ characters); nothing was changed");
    }
    Ok(AiProposal {
        summary,
        replacement,
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AiUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug)]
pub struct AiResponse<T> {
    pub value: T,
    pub usage: AiUsage,
}

#[derive(Debug)]
struct AiTransportFailure {
    usage: AiUsage,
    message: String,
}
impl std::fmt::Display for AiTransportFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for AiTransportFailure {}
fn with_usage(error: anyhow::Error, usage: AiUsage) -> anyhow::Error {
    AiTransportFailure {
        usage,
        message: error.to_string(),
    }
    .into()
}
/// Usage observed before a cancellation, refusal or malformed final response.
/// Missing fields remain unknown; this is not a refund or billing guarantee.
pub fn error_usage(error: &anyhow::Error) -> AiUsage {
    error
        .downcast_ref::<AiTransportFailure>()
        .map(|error| error.usage)
        .unwrap_or_default()
}

async fn await_cancellable<F: std::future::Future>(
    future: F,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<F::Output> {
    let mut future = std::pin::pin!(future);
    loop {
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            bail!("AI request cancelled; nothing was changed");
        }
        // Timing out a borrowed pinned future leaves it pending for the next
        // interval; cancellation drops the actual send/read future and client.
        if let Ok(result) = tokio::time::timeout(Duration::from_millis(25), &mut future).await {
            return Ok(result);
        }
    }
}

pub(crate) fn exchange_cancellable(
    config: &AiProviderConfig,
    exchange: &claude::Exchange<'_>,
    cancelled: &std::sync::atomic::AtomicBool,
    progress: Option<&std::sync::mpsc::Sender<String>>,
) -> Result<AiResponse<String>> {
    if let Some(problem) = endpoint_problem(&config.endpoint) {
        bail!("AI address refused: {problem}");
    }
    if !(1..=16_384).contains(&exchange.max_tokens) {
        bail!("AI output token limit must be between 1 and 16384");
    }
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        bail!("AI request cancelled; nothing was changed");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not start AI transport")?;
    let observed = std::cell::Cell::new(AiUsage::default());
    let result = runtime.block_on(async {
        let client = reqwest::Client::builder().timeout(exchange.timeout).redirect(reqwest::redirect::Policy::none()).build().context("failed to build AI HTTP client")?;
        let mut body = if config.provider == AiProvider::Claude {
            let first_party = reqwest::Url::parse(&config.endpoint).is_ok_and(|url| url.host_str() == Some("api.anthropic.com"));
            claude::request_body(&config.model, exchange, first_party)
        } else {
            json!({"model": config.model, "messages": [{"role":"system","content":exchange.system},{"role":"user","content":exchange.user}], "temperature":0.2, "max_tokens":exchange.max_tokens, "stream_options":{"include_usage":true}})
        };
        body["stream"] = json!(true);
        let mut http = client.post(&config.endpoint).json(&body);
        if config.provider == AiProvider::Claude {
            let key = config.api_key.as_deref().context("Claude needs an API key · open Settings > AI to add one")?;
            http = http.header("x-api-key", key).header("anthropic-version", claude::API_VERSION);
            if body.get("fallbacks").is_some() { http = http.header("anthropic-beta", claude::FALLBACK_BETA); }
        } else if let Some(key) = &config.api_key { http = http.bearer_auth(key); }
        let mut response = await_cancellable(http.send(), cancelled).await?.map_err(|error| connection_error(config, &error))?;
        if !response.status().is_success() { return Err(provider_error(config, response.status())); }
        if response.content_length().is_some_and(|length| length > MAX_ENVELOPE_BYTES as u64) { bail!("AI provider response exceeds the {MAX_ENVELOPE_BYTES}-byte limit"); }
        let is_sse = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).is_some_and(|value| value.split(';').next().is_some_and(|kind| kind.trim().eq_ignore_ascii_case("text/event-stream")));
        let mut bytes = Vec::new();
        let mut stream = ProviderStream::new(config.provider);
        let mut total = 0usize;
        loop {
            let chunk = await_cancellable(response.chunk(), cancelled).await?.map_err(|error| connection_error(config, &error))?;
            let Some(chunk) = chunk else { break; };
            total = total.saturating_add(chunk.len());
            if total > MAX_ENVELOPE_BYTES { bail!("AI provider response exceeds the {MAX_ENVELOPE_BYTES}-byte limit"); }
            if is_sse {
                let update = stream.push(&chunk, progress);
                observed.set(stream.usage);
                update?;
                if stream.done { return Ok(AiResponse { value: stream.text, usage: stream.usage }); }
            } else { bytes.extend_from_slice(&chunk); }
        }
        if is_sse { bail!("AI stream ended before its completion marker; nothing was changed"); }
        let value: Value = serde_json::from_slice(&bytes).context("AI provider returned invalid JSON envelope")?;
        observed.set(usage_from(&value, config.provider));
        let text = if config.provider == AiProvider::Claude { claude::parse_response(&value)? } else {
            check_stop_reason(value.pointer("/choices/0/finish_reason").and_then(Value::as_str))?;
            bounded_output(value.pointer("/choices/0/message/content").and_then(Value::as_str).context("AI provider response has no choices[0].message.content")?.to_owned())?
        };
        Ok(AiResponse { value: text, usage: usage_from(&value, config.provider) })
    });
    // System DNS resolution may use a blocking OS worker. Do not wait for
    // that worker during cancellation; dropped HTTP futures own no active
    // request/body socket after this point. DNS cleanup can finish separately.
    runtime.shutdown_background();
    result.map_err(|error| with_usage(error, observed.get()))
}

fn check_stop_reason(reason: Option<&str>) -> Result<()> {
    match reason {
        Some("length" | "max_tokens") => {
            bail!("AI reply was cut off at its length limit; nothing was changed")
        }
        Some("refusal" | "content_filter") => bail!("AI provider declined this request"),
        _ => Ok(()),
    }
}

fn usage_from(value: &Value, provider: AiProvider) -> AiUsage {
    let usage = value
        .get("usage")
        .or_else(|| value.pointer("/message/usage"));
    let input = if provider == AiProvider::Claude {
        "input_tokens"
    } else {
        "prompt_tokens"
    };
    let output = if provider == AiProvider::Claude {
        "output_tokens"
    } else {
        "completion_tokens"
    };
    let input_tokens = usage
        .and_then(|usage| usage.get(input))
        .and_then(Value::as_u64)
        .map(|tokens| {
            if provider == AiProvider::Claude {
                tokens
                    .saturating_add(
                        usage
                            .and_then(|usage| usage.get("cache_creation_input_tokens"))
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    )
                    .saturating_add(
                        usage
                            .and_then(|usage| usage.get("cache_read_input_tokens"))
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    )
            } else {
                tokens
            }
        });
    AiUsage {
        input_tokens,
        output_tokens: usage
            .and_then(|usage| usage.get(output))
            .and_then(Value::as_u64),
    }
}

struct ProviderStream {
    provider: AiProvider,
    pending: Vec<u8>,
    text: String,
    text_chars: usize,
    usage: AiUsage,
    done: bool,
}

impl ProviderStream {
    fn new(provider: AiProvider) -> Self {
        Self {
            provider,
            pending: Vec::new(),
            text: String::new(),
            text_chars: 0,
            usage: AiUsage::default(),
            done: false,
        }
    }
    fn push(
        &mut self,
        bytes: &[u8],
        progress: Option<&std::sync::mpsc::Sender<String>>,
    ) -> Result<()> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > MAX_ENVELOPE_BYTES {
            bail!("AI stream event exceeds response limit");
        }
        loop {
            let lf = self
                .pending
                .windows(2)
                .position(|pair| pair == b"\n\n")
                .map(|at| (at, 2));
            let crlf = self
                .pending
                .windows(4)
                .position(|pair| pair == b"\r\n\r\n")
                .map(|at| (at, 4));
            let Some((end, delimiter)) = (match (lf, crlf) {
                (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
                (a, b) => a.or(b),
            }) else {
                break;
            };
            let event = String::from_utf8(self.pending.drain(..end + delimiter).collect())
                .context("AI stream event is not UTF-8")?;
            let data = event
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("data:")
                        .map(|data| data.strip_prefix(' ').unwrap_or(data))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                if self.provider == AiProvider::Claude {
                    bail!("unexpected Claude stream completion marker");
                }
                self.done = true;
                break;
            }
            let value: Value =
                serde_json::from_str(&data).context("AI stream event is invalid JSON")?;
            if value.get("error").is_some()
                || value.get("type").and_then(Value::as_str) == Some("error")
            {
                bail!(
                    "AI provider reported a stream error; details withheld to protect credentials"
                );
            }
            let usage = usage_from(&value, self.provider);
            if usage.input_tokens.is_some() {
                self.usage.input_tokens = usage.input_tokens;
            }
            if usage.output_tokens.is_some() {
                self.usage.output_tokens = usage.output_tokens;
            }
            let delta = if self.provider == AiProvider::Claude {
                match value.get("type").and_then(Value::as_str) {
                    Some("message_delta") => {
                        check_stop_reason(
                            value.pointer("/delta/stop_reason").and_then(Value::as_str),
                        )?;
                        None
                    }
                    Some("message_stop") => {
                        self.done = true;
                        None
                    }
                    Some("content_block_delta")
                        if value.pointer("/delta/type").and_then(Value::as_str)
                            == Some("text_delta") =>
                    {
                        value.pointer("/delta/text").and_then(Value::as_str)
                    }
                    Some("content_block_start")
                        if value.pointer("/content_block/type").and_then(Value::as_str)
                            == Some("text") =>
                    {
                        value.pointer("/content_block/text").and_then(Value::as_str)
                    }
                    _ => None,
                }
            } else {
                check_stop_reason(
                    value
                        .pointer("/choices/0/finish_reason")
                        .and_then(Value::as_str),
                )?;
                value
                    .pointer("/choices/0/delta/content")
                    .and_then(Value::as_str)
            };
            if let Some(delta) = delta {
                self.text_chars = self.text_chars.saturating_add(delta.chars().count());
                if self.text_chars > MAX_RESPONSE_CHARS {
                    bail!("AI reply is too long; nothing was changed");
                }
                self.text.push_str(delta);
                if let Some(progress) = progress {
                    let _ = progress.send(delta.to_owned());
                }
            }
            if self.done {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::*;

    #[test]
    fn additive_deny_globs_match_paths_and_defaults_cannot_be_removed() {
        let patterns =
            deny_patterns("# local privacy\n*.private\nprivate/**\n**/customer?.json\n").unwrap();
        for path in [
            "demo.private",
            "/work/demo.private",
            "/work/private/nested/demo.rs",
            "customer1.json",
            "/work/customer2.json",
        ] {
            assert!(deny_matches(&patterns, path), "{path}");
        }
        for path in ["public.rs", "/work/private-looking/file", "customer12.json"] {
            assert!(!deny_matches(&patterns, path), "{path}");
        }
        assert!(excluded_path(".env"));
        assert!(excluded_path("/home/user/.ssh/id_ed25519"));
        assert!(deny_patterns("!**/.env").is_err());
        assert!(deny_patterns("[broken").is_err());
    }
    #[test]
    fn user_local_policy_is_bounded_and_resolves_file_aliases_fail_closed() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let policy = dir.path().join("ai-deny.conf");
        let private = dir.path().join("client.private");
        let alias = dir.path().join("innocent.rs");
        fs::write(&private, "ordinary fixture").unwrap();
        symlink(&private, &alias).unwrap();
        fs::write(&policy, "*.private\n").unwrap();
        assert!(configured_deny_problem(&policy, alias.to_str().unwrap()).unwrap());
        assert!(!configured_deny_problem(&policy, "public.rs").unwrap());
        fs::write(&policy, "[unsupported\n").unwrap();
        assert!(configured_deny_problem(&policy, "public.rs").is_err());
        fs::write(&policy, vec![b' '; 65537]).unwrap();
        assert!(configured_deny_problem(&policy, "public.rs").is_err());
        fs::write(&policy, "*.private").unwrap();
        fs::set_permissions(&policy, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(configured_deny_problem(&policy, "public.rs").is_err());
        fs::remove_file(&policy).unwrap();
        symlink(&private, &policy).unwrap();
        assert!(configured_deny_problem(&policy, "public.rs").is_err());
    }

    #[test]
    fn staged_alias_privacy_uses_the_repository_root_not_process_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join(".env");
        let alias = dir.path().join("innocent.rs");
        fs::write(&secret, "ordinary fixture").unwrap();
        std::os::unix::fs::symlink(&secret, &alias).unwrap();
        let error = commit_message_cancellable(
            &local_config("http://127.0.0.1:9/chat".to_owned()),
            "diff --git a/innocent.rs b/innocent.rs\n+++ b/innocent.rs\n+ordinary fixture",
            dir.path(),
            &std::sync::atomic::AtomicBool::new(false),
            100,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("excluded"), "{error}");
    }

    fn stream_fixture(
        body: String,
        pause_before_body: bool,
    ) -> (String, std::thread::JoinHandle<bool>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 32768];
            let _ = socket.read(&mut request).unwrap();
            if pause_before_body {
                // Send headers and keep the body silent: cancellation must
                // abort a blocked chunk read, not merely suppress application.
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n").unwrap();
                loop {
                    match socket.read(&mut request) {
                        Ok(0) => return true,
                        Ok(_) => {}
                        Err(_) => return false,
                    }
                }
            }
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            // Fragment UTF8 and event boundaries intentionally.
            for bytes in body.as_bytes().chunks(3) {
                if socket.write_all(bytes).is_err() {
                    return true;
                }
            }
            false
        });
        (format!("http://{address}/chat"), worker)
    }

    #[test]
    fn streaming_openai_reports_provider_usage_and_preserves_unicode_deltas() {
        let proposal = r#"{"summary":"Looks good 🙂","replacement":null}"#;
        let body = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"delta":{"content":proposal},"finish_reason":null}]}),
            json!({"choices":[],"usage":{"prompt_tokens":17,"completion_tokens":8}})
        );
        let (endpoint, worker) = stream_fixture(body, false);
        let (sender, receiver) = std::sync::mpsc::channel();
        let response = request_cancellable(
            &local_config(endpoint),
            &edit_request("ordinary text".to_owned()),
            &std::sync::atomic::AtomicBool::new(false),
            123,
            Some(&sender),
        )
        .unwrap();
        worker.join().unwrap();
        assert_eq!(response.value.summary, "Looks good 🙂");
        assert_eq!(
            response.usage,
            AiUsage {
                input_tokens: Some(17),
                output_tokens: Some(8)
            }
        );
        assert_eq!(receiver.try_iter().collect::<String>(), proposal);
    }

    #[test]
    fn streaming_claude_merges_usage_and_refuses_cut_off_text() {
        let mut stream = ProviderStream::new(AiProvider::Claude);
        let events = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20,\"output_tokens\":0}}}\r\n\r\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"safe 🙂\"}}\r\n\r\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\r\n\r\n",
            "data: {\"type\":\"message_stop\"}\r\n\r\n"
        );
        for byte in events.as_bytes() {
            stream.push(&[*byte], None).unwrap();
        }
        assert!(stream.done);
        assert_eq!(stream.text, "safe 🙂");
        assert_eq!(
            stream.usage,
            AiUsage {
                input_tokens: Some(20),
                output_tokens: Some(7)
            }
        );
        let mut cut = ProviderStream::new(AiProvider::Claude);
        assert!(cut.push(b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"}}\n\n",None).is_err());
        let mut openai = ProviderStream::new(AiProvider::Custom);
        assert!(
            openai
                .push(
                    b"data: {\"choices\":[{\"finish_reason\":\"length\"}]}\n\n",
                    None
                )
                .is_err()
        );
    }

    #[test]
    fn json_usage_and_inline_output_follow_the_same_bounded_transport() {
        let envelope = json!({"choices":[{"message":{"content":"return 1;"}}],"usage":{"prompt_tokens":12,"completion_tokens":4}}).to_string();
        let (endpoint, worker) = one_shot_server(envelope);
        let reply = complete_inline_cancellable(
            &local_config(endpoint),
            &InlineRequest {
                path: "demo.rs".to_owned(),
                language: "Rust".to_owned(),
                before: "fn main() {".to_owned(),
                after: "}".to_owned(),
            },
            &std::sync::atomic::AtomicBool::new(false),
            100,
            None,
        )
        .unwrap();
        worker.join().unwrap();
        assert_eq!(reply.value, "return 1;");
        assert_eq!(
            reply.usage,
            AiUsage {
                input_tokens: Some(12),
                output_tokens: Some(4)
            }
        );
        assert_eq!(
            usage_from(
                &json!({"usage":{"input_tokens":3,"cache_creation_input_tokens":4,"cache_read_input_tokens":5,"output_tokens":2}}),
                AiProvider::Claude
            ),
            AiUsage {
                input_tokens: Some(12),
                output_tokens: Some(2)
            }
        );
    }

    #[test]
    fn cancellation_aborts_waiting_for_response_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = [0; 32768];
            assert!(socket.read(&mut bytes).unwrap() > 0);
            loop {
                match socket.read(&mut bytes) {
                    Ok(0) => return true,
                    Ok(_) => {}
                    Err(_) => return false,
                }
            }
        });
        let token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled = token.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancelled.store(true, std::sync::atomic::Ordering::Release);
        });
        let started = std::time::Instant::now();
        let error = request_cancellable(
            &local_config(format!("http://{address}/chat")),
            &edit_request("text".to_owned()),
            &token,
            100,
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(1));
        canceller.join().unwrap();
        assert!(worker.join().unwrap());
    }

    #[test]
    fn cancellation_aborts_the_inflight_transport_and_closes_its_socket() {
        let (endpoint, worker) = stream_fixture(String::new(), true);
        let token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled = token.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancelled.store(true, std::sync::atomic::Ordering::Release);
        });
        let started = std::time::Instant::now();
        let result = request_cancellable(
            &local_config(endpoint),
            &edit_request("ordinary text".to_owned()),
            &token,
            100,
            None,
        );
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(1));
        canceller.join().unwrap();
        assert!(
            worker.join().unwrap(),
            "cancelled client kept the socket alive"
        );
    }

    #[test]
    fn streaming_missing_completion_marker_never_accepts_partial_proposals() {
        let body = format!(
            "data: {}\n\ndata: {}\n\n",
            json!({"choices":[{"delta":{"content":r#"{"summary":"partial","replacement":"danger"}"#}}]}),
            json!({"choices":[],"usage":{"prompt_tokens":19,"completion_tokens":7}})
        );
        let (endpoint, worker) = stream_fixture(body, false);
        let result = request_cancellable(
            &local_config(endpoint),
            &edit_request("ordinary".to_owned()),
            &std::sync::atomic::AtomicBool::new(false),
            100,
            None,
        );
        worker.join().unwrap();
        let error = result.unwrap_err();
        assert!(error.to_string().contains("completion marker"));
        assert_eq!(
            error_usage(&error),
            AiUsage {
                input_tokens: Some(19),
                output_tokens: Some(7)
            }
        );
    }

    #[test]
    fn stream_errors_and_limits_withhold_credentials_and_reject_hostile_shell_controls() {
        let mut stream = ProviderStream::new(AiProvider::Custom);
        let error = stream
            .push(
                b"data: {\"error\":{\"message\":\"synthetic-secret\\u001b\"}}\n\n",
                None,
            )
            .unwrap_err()
            .to_string();
        assert!(!error.contains("synthetic-secret"));
        assert!(!error.contains('\u{1b}'));
        let mut stream = ProviderStream::new(AiProvider::Custom);
        let huge =
            json!({"choices":[{"delta":{"content":"x".repeat(MAX_RESPONSE_CHARS+1)}}]}).to_string();
        assert!(
            stream
                .push(format!("data: {huge}\n\n").as_bytes(), None)
                .is_err()
        );
        let body = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"delta":{"content":"echo safe\r# injected"}}]})
        );
        let (endpoint, worker) = stream_fixture(body, false);
        let error = shell_command_cancellable(
            &local_config(endpoint),
            "show files",
            &std::sync::atomic::AtomicBool::new(false),
            100,
            None,
        )
        .unwrap_err()
        .to_string();
        worker.join().unwrap();
        assert!(error.contains("Control bytes"));
        assert_eq!(
            usage_from(&json!({}), AiProvider::Custom),
            AiUsage::default()
        );
    }

    #[test]
    fn proposal_parser_accepts_json_and_never_edits_without_permission() {
        let content = r#"{"summary":"Use a guard clause.","replacement":"return early;"}"#;
        let proposal = parse_proposal(content, true).unwrap();
        assert_eq!(proposal.summary, "Use a guard clause.");
        assert_eq!(proposal.replacement.as_deref(), Some("return early;"));

        let read_only = parse_proposal(content, false).unwrap();
        assert!(read_only.replacement.is_none());
    }

    #[test]
    fn saved_setup_round_trips_and_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mellow/ai.conf");
        let config = AiProviderConfig {
            provider: AiProvider::Claude,
            endpoint: AiProvider::Claude.default_endpoint().to_owned(),
            model: AiProvider::Claude.default_model().to_owned(),
            api_key: Some("sk-test".to_owned()),
            inline_suggestions: true,
        };
        config.save_to(&path).unwrap();
        assert_eq!(AiProviderConfig::load_from(&path).unwrap(), Some(config));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "API key file must be private");
        }
    }

    #[test]
    fn inline_suggestions_drop_fences_and_stay_short() {
        assert_eq!(clean_inline("```sh\necho hi\n```\n"), "echo hi");
        let long = (0..20)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(clean_inline(&long).lines().count(), 8);
    }

    /// Serves one canned chat-completions envelope on localhost.
    #[test]
    fn ai_requests_do_not_follow_redirects_to_another_host() {
        use std::io::Write;
        // The "other host" counts connections; a followed redirect would reach it.
        let other = TcpListener::bind("127.0.0.1:0").unwrap();
        other.set_nonblocking(true).unwrap();
        let other_address = other.local_addr().unwrap();
        let front = TcpListener::bind("127.0.0.1:0").unwrap();
        let front_address = front.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = front.accept().unwrap();
            let mut request = vec![0u8; 16 * 1024];
            let _ = stream.read(&mut request).unwrap();
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: http://{other_address}/steal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let config = local_config(format!("http://{front_address}/v1/chat/completions"));
        let result = request(&config, &edit_request("x".to_owned()));
        server.join().unwrap();
        assert!(result.is_err(), "a redirect must not succeed silently");
        assert!(
            matches!(other.accept(), Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "the redirect target was contacted"
        );
    }

    #[test]
    fn privacy_policy_denies_secret_files_and_credentials_before_network() {
        for path in [
            ".env",
            ".env.production",
            "infra/key.pem",
            "credentials",
            "keys/id_ed25519",
            ".ssh/config",
            ".aws/config",
            "nested/.ssh/config",
            "secrets.json",
            "secrets.yaml",
        ] {
            let mut input = edit_request("innocent text".to_owned());
            input.path = path.to_owned();
            let error = request(&local_config("http://127.0.0.1:1/chat".to_owned()), &input)
                .unwrap_err()
                .to_string();
            assert!(error.contains("excluded"), "{path}: {error}");
        }
        assert!(context_problem(".env.example", "# Fill in values locally").is_none());
        assert!(context_problem("demo.rs", "let count = 5;").is_none());
        for text in [
            r#"{"api_key":"synthetic-secret","password":"dummy"}"#,
            "API_KEY=synthetic-secret",
            "password: dummy-value",
            "-----BEGIN RSA PRIVATE KEY-----",
            "token AKIA1234567890ABCDEF",
        ] {
            assert!(context_problem("demo.rs", text).is_some(), "{text}");
        }
        let error = commit_message(
            &local_config("http://127.0.0.1:1/chat".to_owned()),
            "diff --git a/.env b/.env\n+++ b/.env\n+innocent",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("excluded"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn privacy_policy_denies_an_innocuous_alias_to_an_excluded_file() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join(".env");
        let alias = dir.path().join("innocent.txt");
        fs::write(&secret, "innocent fixture").unwrap();
        std::os::unix::fs::symlink(&secret, &alias).unwrap();
        assert!(context_problem(alias.to_str().unwrap(), "innocent fixture").is_some());
    }

    #[test]
    fn instruction_and_http_envelopes_have_hard_limits() {
        let config = local_config("http://127.0.0.1:1/chat".to_owned());
        let error = shell_command(&config, &"x".repeat(MAX_INSTRUCTION_CHARS + 1))
            .unwrap_err()
            .to_string();
        assert!(error.contains("too large"), "{error}");
        let (endpoint, server) = one_shot_server("x".repeat(MAX_ENVELOPE_BYTES + 1));
        let error = request(&local_config(endpoint), &edit_request("x".to_owned()))
            .unwrap_err()
            .to_string();
        // The connection may close as soon as the announced size is checked.
        let _ = server.join();
        assert!(error.contains("byte limit"), "{error}");
        assert!(bounded_output("x".repeat(MAX_RESPONSE_CHARS + 1)).is_err());
    }

    #[test]
    fn errors_never_echo_provider_credentials_or_terminal_controls() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 16384];
            let _ = stream.read(&mut request).unwrap();
            let body = "synthetic-secret-key\u{1b}[2J";
            write!(stream, "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let mut config = local_config(format!("http://{address}/chat"));
        config.api_key = Some("synthetic-secret-key".to_owned());
        let error = request(&config, &edit_request("x".to_owned()))
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        assert!(error.contains("500"), "{error}");
        assert!(!error.contains("synthetic-secret-key"), "{error}");
        assert!(!error.contains('\u{1b}'), "{error}");
    }

    #[test]
    fn debug_output_never_shows_the_api_key() {
        let config = super::AiProviderConfig {
            provider: super::AiProvider::Claude,
            endpoint: "https://example.invalid/v1/messages".to_owned(),
            model: "m".to_owned(),
            api_key: Some("sk-secret-value".to_owned()),
            inline_suggestions: false,
        };
        let shown = format!("{config:?}");
        assert!(!shown.contains("sk-secret-value"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
        let mut embedded = config;
        embedded.endpoint =
            "https://user:embedded-password@example.invalid/v1?key=embedded-query-secret"
                .to_owned();
        let shown = format!("{embedded:?}");
        assert!(!shown.contains("embedded-password"), "{shown}");
        assert!(!shown.contains("embedded-query-secret"), "{shown}");
        assert!(endpoint_problem(&embedded.endpoint).is_some());
    }

    fn one_shot_server(envelope: String) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0u8; 256 * 1024];
            let _ = stream.read(&mut request).unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", envelope.len()).unwrap();
            if let Err(error) = stream.write_all(envelope.as_bytes()) {
                // Oversized Content-Length is refused before body draining.
                assert!(
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                    ),
                    "{error}"
                );
            }
        });
        (format!("http://{address}/chat"), server)
    }

    fn local_config(endpoint: String) -> AiProviderConfig {
        AiProviderConfig {
            provider: AiProvider::Custom,
            endpoint,
            model: "test-model".to_owned(),
            api_key: None,
            inline_suggestions: false,
        }
    }

    fn edit_request(context: String) -> AiRequest {
        AiRequest {
            instruction: "Rewrite".to_owned(),
            path: "demo.rs".to_owned(),
            language: "Rust".to_owned(),
            context,
            allow_replacement: true,
        }
    }

    #[test]
    fn endpoint_host_names_where_the_key_goes() {
        assert_eq!(
            endpoint_host("https://api.anthropic.com/v1/messages"),
            Some("api.anthropic.com")
        );
        assert_eq!(
            endpoint_host("http://user@localhost:11434/v1"),
            Some("localhost:11434")
        );
        assert_eq!(endpoint_host("not a url"), None);
    }

    #[cfg(unix)]
    #[test]
    fn saving_setup_replaces_the_file_atomically_and_privately() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai.conf");
        fs::write(&path, "old contents").unwrap();
        let mut config = local_config("https://api.example.com/v1".to_owned());
        config.api_key = Some("secret".to_owned());
        config.save_to(&path).unwrap();

        let entries: Vec<_> = fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert_eq!(entries.len(), 1, "no temporary file left behind");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let loaded = AiProviderConfig::load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.api_key.as_deref(), Some("secret"));
    }

    #[test]
    fn remote_plain_http_is_refused_but_localhost_is_allowed() {
        assert!(endpoint_problem("https://api.example.com/v1").is_none());
        for local in [
            "http://localhost:11434/v1/chat/completions",
            "http://127.0.0.1:8080/chat",
            "http://[::1]:8080/chat",
            "http://127.0.0.2:8080/chat",
        ] {
            assert!(endpoint_problem(local).is_none(), "{local}");
        }
        for remote in [
            "http://api.example.com/v1",
            "http://localhost.evil.com/v1",
            "http://127.0.0.1.evil.com/v1",
            "http://127.evil.com:8080/v1",
            "http://user@10.0.0.5/v1",
            "ftp://example.com",
        ] {
            assert!(endpoint_problem(remote).is_some(), "{remote}");
        }
        let error = request(
            &local_config("http://api.example.com/chat".to_owned()),
            &edit_request("x".to_owned()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("https"), "{error}");
    }

    /// Audit: an oversized selection must not be clipped and then replaced.
    #[test]
    fn oversized_selection_is_refused_before_anything_is_sent() {
        let config = local_config("http://127.0.0.1:9/never-contacted".to_owned());
        let error = request(&config, &edit_request("x".repeat(MAX_CONTEXT_CHARS + 1)))
            .unwrap_err()
            .to_string();
        assert!(error.contains("too large"), "{error}");
    }

    #[test]
    fn oversized_or_cut_off_replacements_change_nothing() {
        let huge = "y".repeat(MAX_RESPONSE_CHARS + 1);
        let content = json!({"summary":"done","replacement":huge}).to_string();
        let (endpoint, server) =
            one_shot_server(json!({"choices":[{"message":{"content":content}}]}).to_string());
        let error = request(&local_config(endpoint), &edit_request("x".to_owned()))
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        assert!(error.contains("too long"), "{error}");

        let content = r#"{"summary":"partial","replacement":"fn a"}"#;
        let (endpoint, server) = one_shot_server(
            json!({"choices":[{"message":{"content":content},"finish_reason":"length"}]})
                .to_string(),
        );
        let error = request(&local_config(endpoint), &edit_request("x".to_owned()))
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        assert!(error.contains("cut off"), "{error}");
    }

    #[test]
    fn openai_compatible_request_round_trip_uses_bounded_adapter_contract() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0u8; 16 * 1024];
            let read = stream.read(&mut request).unwrap();
            let text = String::from_utf8_lossy(&request[..read]);
            assert!(text.contains("POST /chat HTTP/1.1"));
            assert!(text.contains("Bearer test-key"));

            let content = r#"{"summary":"Looks good.","replacement":null}"#;
            let envelope = json!({"choices":[{"message":{"content":content}}]}).to_string();

            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                envelope.len(),
                envelope
            )
            .unwrap();
        });

        let config = AiProviderConfig {
            provider: AiProvider::Custom,
            endpoint: format!("http://{address}/chat"),
            model: "test-model".to_owned(),
            api_key: Some("test-key".to_owned()),
            inline_suggestions: false,
        };
        let proposal = request(
            &config,
            &AiRequest {
                instruction: "Explain this".to_owned(),
                path: "demo.rs".to_owned(),
                language: "Rust".to_owned(),
                context: "fn main() {}".to_owned(),
                allow_replacement: false,
            },
        )
        .unwrap();

        server.join().unwrap();
        assert_eq!(proposal.summary, "Looks good.");
        assert!(proposal.replacement.is_none());
    }
}
