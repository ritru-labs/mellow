//! Claude (Anthropic Messages API) transport for the optional AI assistant.
//!
//! Rust has no official Anthropic SDK, so this speaks the documented HTTP
//! API directly: `POST /v1/messages` with `x-api-key` and
//! `anthropic-version` headers, reading the `text` content blocks back.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::ai::{AiProviderConfig, connection_error};

pub const MESSAGES_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
const API_VERSION: &str = "2023-06-01";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// One request/response round trip.
pub struct Exchange<'a> {
    pub system: &'a str,
    pub user: &'a str,
    pub max_tokens: u32,
    /// `output_config.effort` for models that accept it.
    pub effort: &'a str,
    pub timeout: Duration,
}

/// Models that accept `output_config.effort`.
fn supports_effort(model: &str) -> bool {
    matches!(
        model,
        "claude-fable-5-1"
            | "claude-fable-5"
            | "claude-opus-5-5"
            | "claude-opus-5"
            | "claude-opus-4-8"
            | "claude-opus-4-7"
            | "claude-opus-4-6"
            | "claude-sonnet-5-5"
            | "claude-sonnet-5"
            | "claude-sonnet-4-6"
    )
}

/// Models that accept server-side refusal fallbacks (`fallbacks: "default"`)
/// on the Claude API.
fn supports_fallbacks(model: &str) -> bool {
    matches!(
        model,
        "claude-fable-5-1" | "claude-opus-5-5" | "claude-opus-5" | "claude-sonnet-5-5"
    )
}

pub fn request_body(model: &str, exchange: &Exchange<'_>, first_party: bool) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": exchange.max_tokens,
        "system": exchange.system,
        "messages": [{"role": "user", "content": exchange.user}],
    });
    if supports_effort(model) {
        body["output_config"] = json!({"effort": exchange.effort});
    }
    if first_party && supports_fallbacks(model) {
        body["fallbacks"] = json!("default");
    }
    body
}

/// Sends one message and returns the concatenated text blocks.
pub fn send(config: &AiProviderConfig, exchange: &Exchange<'_>) -> Result<String> {
    let Some(api_key) = config.api_key.as_deref() else {
        bail!("Claude needs an API key · open Settings > AI to add one");
    };
    let first_party = config.endpoint.starts_with(MESSAGES_ENDPOINT);
    let body = request_body(&config.model, exchange, first_party);

    // Never follow redirects: the key header would go to wherever they point.
    let client = reqwest::blocking::Client::builder()
        .timeout(exchange.timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("failed to build AI HTTP client")?;
    let mut http = client
        .post(&config.endpoint)
        .header("x-api-key", api_key)
        .header("anthropic-version", API_VERSION)
        .json(&body);
    if body.get("fallbacks").is_some() {
        http = http.header("anthropic-beta", FALLBACK_BETA);
    }
    let response = http
        .send()
        .map_err(|error| connection_error(config, &error))?;
    let status = response.status();
    let text = response
        .text()
        .context("failed to read Claude's response")?;
    let envelope: Value = serde_json::from_str(&text).unwrap_or(Value::Null);

    if !status.is_success() {
        let message = envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| text.chars().take(300).collect());
        match status.as_u16() {
            401 => bail!("Claude rejected the API key · check it in Settings > AI"),
            429 => bail!("Claude is rate limiting requests · try again in a moment"),
            529 => bail!("Claude is overloaded right now · try again shortly"),
            _ => bail!("Claude returned {status}: {message}"),
        }
    }

    parse_response(&envelope)
}

fn parse_response(envelope: &Value) -> Result<String> {
    // Always check the stop reason before reading content.
    if envelope.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        let category = envelope
            .pointer("/stop_details/category")
            .and_then(Value::as_str);
        match category {
            Some(category) => bail!("Claude declined this request ({category})"),
            None => bail!("Claude declined this request"),
        }
    }
    if envelope.get("stop_reason").and_then(Value::as_str) == Some("max_tokens") {
        bail!("Claude's reply was cut off at its length limit; nothing was changed");
    }
    let blocks = envelope
        .get("content")
        .and_then(Value::as_array)
        .context("Claude's response has no content")?;
    Ok(blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(""))
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::*;
    use crate::ai::AiProvider;

    fn exchange() -> Exchange<'static> {
        Exchange {
            system: "system text",
            user: "user text",
            max_tokens: 16_000,
            effort: "medium",
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn request_body_gates_effort_and_fallbacks_by_model() {
        let body = request_body("claude-opus-5-5", &exchange(), true);
        assert_eq!(body["output_config"]["effort"], "medium");
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["system"], "system text");
        assert_eq!(body["messages"][0]["role"], "user");

        let haiku = request_body("claude-haiku-4-5", &exchange(), true);
        assert!(haiku.get("output_config").is_none());
        assert!(haiku.get("fallbacks").is_none());

        let proxied = request_body("claude-opus-5-5", &exchange(), false);
        assert!(proxied.get("fallbacks").is_none());
    }

    #[test]
    fn parse_response_reads_text_blocks_and_reports_refusals() {
        let ok = json!({
            "stop_reason": "end_turn",
            "content": [
                {"type": "thinking", "thinking": ""},
                {"type": "text", "text": "Hello "},
                {"type": "text", "text": "there"}
            ]
        });
        assert_eq!(parse_response(&ok).unwrap(), "Hello there");

        let cut_off = json!({
            "stop_reason": "max_tokens",
            "content": [{"type": "text", "text": "{\"summary\": \"half"}]
        });
        let error = parse_response(&cut_off).unwrap_err().to_string();
        assert!(error.contains("cut off"), "{error}");

        let refused = json!({
            "stop_reason": "refusal",
            "stop_details": {"type": "refusal", "category": "cyber"},
            "content": []
        });
        let error = parse_response(&refused).unwrap_err().to_string();
        assert!(error.contains("declined"), "{error}");
        assert!(error.contains("cyber"), "{error}");
    }

    #[test]
    fn send_uses_messages_headers_and_returns_text() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0u8; 16 * 1024];
            let read = stream.read(&mut request).unwrap();
            let text = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(text.contains("post /v1/messages http/1.1"), "{text}");
            assert!(text.contains("x-api-key: test-key"), "{text}");
            assert!(text.contains("anthropic-version: 2023-06-01"), "{text}");

            let body = json!({
                "stop_reason": "end_turn",
                "content": [{"type": "text", "text": "{\"summary\":\"ok\",\"replacement\":null}"}]
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let config = AiProviderConfig {
            provider: AiProvider::Claude,
            endpoint: format!("http://{address}/v1/messages"),
            model: "claude-opus-5-5".to_owned(),
            api_key: Some("test-key".to_owned()),
            inline_suggestions: false,
        };
        let text = send(&config, &exchange()).unwrap();
        server.join().unwrap();
        assert!(text.contains("\"summary\":\"ok\""));
    }
}
