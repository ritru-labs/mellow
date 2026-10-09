use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::claude;

const MAX_CONTEXT_CHARS: usize = 32_000;
const MAX_RESPONSE_CHARS: usize = 64_000;

/// Keys and code only travel over HTTPS; plain HTTP is allowed for a model
/// running on this computer (Ollama and similar).
pub fn endpoint_problem(endpoint: &str) -> Option<&'static str> {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiProviderConfig {
    pub provider: AiProvider,
    pub endpoint: String,
    pub model: String,
    pub api_key: Option<String>,
    /// Ask for grey suggestions after a typing pause. Off unless chosen,
    /// because it sends nearby code without an explicit request.
    pub inline_suggestions: bool,
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

pub fn request(config: &AiProviderConfig, request: &AiRequest) -> Result<AiProposal> {
    if request.instruction.trim().is_empty() {
        bail!("AI instruction cannot be empty");
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

    let content = match config.provider {
        AiProvider::Claude => claude::send(
            config,
            &claude::Exchange {
                system: ASK_SYSTEM,
                user: &user_payload,
                max_tokens: 16_000,
                effort: "medium",
                timeout: ASK_TIMEOUT,
            },
        )?,
        _ => chat_completion(config, ASK_SYSTEM, &user_payload, ASK_TIMEOUT)?,
    };
    parse_proposal(&content, request.allow_replacement)
}

/// A short continuation for the cursor position, or an empty string.
pub fn complete_inline(config: &AiProviderConfig, request: &InlineRequest) -> Result<String> {
    if let Some(problem) = endpoint_problem(&config.endpoint) {
        bail!("AI address refused: {problem}");
    }
    let before: String = {
        let chars: Vec<char> = request.before.chars().collect();
        chars[chars.len().saturating_sub(6_000)..].iter().collect()
    };
    let after: String = request.after.chars().take(2_000).collect();
    let user = format!(
        "File: {}\nLanguage: {}\n<before_cursor>\n{}</before_cursor>\n<after_cursor>\n{}</after_cursor>",
        request.path, request.language, before, after
    );
    let text = match config.provider {
        AiProvider::Claude => claude::send(
            config,
            &claude::Exchange {
                system: INLINE_SYSTEM,
                user: &user,
                // A deliberately short answer: a few lines at most.
                max_tokens: 1_024,
                effort: "low",
                timeout: INLINE_TIMEOUT,
            },
        )?,
        _ => chat_completion(config, INLINE_SYSTEM, &user, INLINE_TIMEOUT)?,
    };
    Ok(clean_inline(&text))
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
pub fn commit_message(config: &AiProviderConfig, diff: &str) -> Result<String> {
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
    let text = match config.provider {
        AiProvider::Claude => claude::send(
            config,
            &claude::Exchange {
                system: COMMIT_SYSTEM,
                user: &user,
                max_tokens: 1_024,
                effort: "low",
                timeout: ASK_TIMEOUT,
            },
        )?,
        _ => chat_completion(config, COMMIT_SYSTEM, &user, ASK_TIMEOUT)?,
    };
    Ok(clean_commit_message(&text))
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

fn chat_completion(
    config: &AiProviderConfig,
    system: &str,
    user: &str,
    timeout: Duration,
) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .context("failed to build AI HTTP client")?;
    let mut http = client.post(&config.endpoint).json(&json!({
        "model": config.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "temperature": 0.2
    }));

    if let Some(api_key) = &config.api_key {
        http = http.bearer_auth(api_key);
    }
    let response = http
        .send()
        .map_err(|error| connection_error(config, &error))?;
    let status = response.status();
    let body = response
        .text()
        .context("failed to read AI provider response")?;
    if !status.is_success() {
        let preview: String = body.chars().take(500).collect();
        bail!(
            "{} returned {status}: {preview}",
            config.provider.short_label()
        );
    }

    let envelope: Value =
        serde_json::from_str(&body).context("AI provider returned invalid JSON envelope")?;
    if envelope
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        == Some("length")
    {
        bail!(
            "{}'s reply was cut off at its length limit; nothing was changed",
            config.provider.short_label()
        );
    }
    envelope
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("AI provider response has no choices[0].message.content")
}

pub(crate) fn connection_error(config: &AiProviderConfig, error: &reqwest::Error) -> anyhow::Error {
    if error.is_timeout() {
        anyhow::anyhow!("{} took too long to answer", config.provider.short_label())
    } else if error.is_connect() {
        anyhow::anyhow!(
            "could not reach {} at {} (offline?)",
            config.provider.short_label(),
            config.endpoint
        )
    } else {
        anyhow::anyhow!(
            "request to {} failed: {error}",
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

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::*;

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
    fn one_shot_server(envelope: String) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0u8; 256 * 1024];
            let _ = stream.read(&mut request).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                envelope.len(),
                envelope
            )
            .unwrap();
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
