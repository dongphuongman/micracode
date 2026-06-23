//! Direct Anthropic text generation (PRD FR `ClaudeTextGeneration` / decision D1).
//!
//! For non-conversational generations — commit messages, summaries — spinning up
//! a full Claude Code CLI turn (the [`core_provider`] subprocess path) is heavy.
//! Instead we make a single, native [`reqwest`] call to the Anthropic Messages
//! API. This stays behind a [`TextGenerator`] trait so the backend is swappable
//! (D1: "keep this behind the same provider trait"), mirroring how the
//! subprocess provider sits behind `core_provider::ProviderDriver`.
//!
//! Rust has no official Anthropic SDK, so raw HTTP is the sanctioned surface
//! here (the same reqwest pattern `model_catalog` already uses for Ollama).

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::Config;

/// Default model for text generation. Opus is the most capable model and the
/// right default for commit messages / summaries; override with `ANTHROPIC_MODEL`.
pub const DEFAULT_MODEL: &str = "claude-opus-4-8";

/// The Anthropic Messages API version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, thiserror::Error)]
pub enum TextGenError {
    /// No `ANTHROPIC_API_KEY` configured — the feature is unavailable.
    #[error("anthropic API key is not configured")]
    MissingApiKey,
    /// Transport-level failure (connect, timeout, body read).
    #[error("anthropic request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// The API responded with a non-2xx status.
    #[error("anthropic API error (HTTP {status}): {body}")]
    Api { status: u16, body: String },
    /// A 200 response whose `stop_reason` was `refusal` (safety decline).
    #[error("anthropic declined the request")]
    Refused,
    /// A 200 response that carried no usable text content.
    #[error("anthropic returned no text content")]
    Empty,
}

/// A single non-conversational generation request.
pub struct TextRequest {
    /// Optional system prompt (the role/instructions).
    pub system: Option<String>,
    /// The user prompt.
    pub prompt: String,
    /// Hard ceiling on output tokens.
    pub max_tokens: u32,
}

/// A backend that turns a prompt into text. The seam D1 calls for: a native
/// Anthropic client today, a different backend (or the CLI) tomorrow, without
/// touching callers.
#[allow(async_fn_in_trait)] // concrete callers only; no `dyn TextGenerator`.
pub trait TextGenerator {
    async fn generate(&self, req: TextRequest) -> Result<String, TextGenError>;
}

/// Calls the Anthropic Messages API directly over HTTP.
pub struct ClaudeTextGenerator {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl ClaudeTextGenerator {
    /// Build a generator with an explicit endpoint (used by tests to target a
    /// mock server). `base_url` is the API host, e.g. `https://api.anthropic.com`.
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        ClaudeTextGenerator {
            client,
            api_key: api_key.into(),
            model: model.into(),
            base_url: base_url.into(),
        }
    }

    /// Build from runtime [`Config`] (`ANTHROPIC_API_KEY` / `ANTHROPIC_MODEL` /
    /// `ANTHROPIC_BASE_URL`).
    pub fn from_config(config: &Config) -> Self {
        Self::new(
            config.anthropic_api_key.clone(),
            config.anthropic_model.clone(),
            config.anthropic_base_url.clone(),
        )
    }

    /// Generate a git commit message for `diff`. Returns only the message body —
    /// a short imperative subject, optionally followed by a brief body.
    pub async fn commit_message(&self, diff: &str) -> Result<String, TextGenError> {
        // Bound the prompt so a huge working tree can't blow the request up.
        const MAX_DIFF_CHARS: usize = 24_000;
        let diff = truncate(diff, MAX_DIFF_CHARS);
        let system = "You write concise git commit messages. Reply with ONLY the \
            commit message: a short imperative subject line of at most 72 \
            characters, optionally followed by a blank line and a brief body. No \
            backticks, no quotes, no preamble, no explanation.";
        self.generate(TextRequest {
            system: Some(system.to_string()),
            prompt: format!("Write a commit message for this diff:\n\n{diff}"),
            max_tokens: 512,
        })
        .await
    }
}

impl TextGenerator for ClaudeTextGenerator {
    async fn generate(&self, req: TextRequest) -> Result<String, TextGenError> {
        if self.api_key.is_empty() {
            return Err(TextGenError::MissingApiKey);
        }

        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens,
            "messages": [{ "role": "user", "content": req.prompt }],
        });
        if let Some(system) = req.system {
            body["system"] = Value::String(system);
        }

        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let resp = self
            .client
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(TextGenError::Api { status: status.as_u16(), body });
        }

        let parsed: MessagesResponse = resp.json().await?;
        if parsed.stop_reason.as_deref() == Some("refusal") {
            return Err(TextGenError::Refused);
        }

        // Concatenate the `text` blocks; the response can interleave other block
        // types, so filter to text and join.
        let text: String = parsed
            .content
            .into_iter()
            .filter(|b| b.kind == "text")
            .filter_map(|b| b.text)
            .collect::<Vec<_>>()
            .join("");
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(TextGenError::Empty);
        }
        Ok(text)
    }
}

/// Truncate `s` to at most `max` chars on a char boundary, marking the cut.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let end = s.char_indices().take(max).last().map(|(i, _)| i).unwrap_or(0);
    format!("{}\n\n[diff truncated]", &s[..end])
}

#[derive(Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// Spawn a one-route mock of the Anthropic Messages API. The handler records
    /// the request it saw and replies with `reply`. Returns the base URL plus a
    /// shared slot holding the captured request headers + body.
    async fn mock_anthropic(
        reply: Value,
        status: axum::http::StatusCode,
    ) -> (String, Arc<Mutex<Option<(Vec<(String, String)>, Value)>>>) {
        let captured = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&captured);

        let app = Router::new().route(
            "/v1/messages",
            post(move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                let slot = Arc::clone(&slot);
                let reply = reply.clone();
                async move {
                    let hs = headers
                        .iter()
                        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                        .collect();
                    *slot.lock().await = Some((hs, body));
                    (status, Json(reply))
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), captured)
    }

    #[tokio::test]
    async fn generate_sends_auth_headers_and_parses_text() {
        let reply = json!({
            "content": [{ "type": "text", "text": "Add commit-message endpoint" }],
            "stop_reason": "end_turn",
        });
        let (base_url, captured) = mock_anthropic(reply, axum::http::StatusCode::OK).await;

        let gen = ClaudeTextGenerator::new("sk-test", "claude-opus-4-8", base_url);
        let out = gen.commit_message("diff --git a/x b/x\n+hello").await.unwrap();
        assert_eq!(out, "Add commit-message endpoint");

        let (headers, body) = captured.lock().await.clone().unwrap();
        // Auth + version headers are present and correct.
        assert!(headers.iter().any(|(k, v)| k == "x-api-key" && v == "sk-test"));
        assert!(headers
            .iter()
            .any(|(k, v)| k == "anthropic-version" && v == ANTHROPIC_VERSION));
        // The request carries the model, a system prompt, and a user message.
        assert_eq!(body["model"], "claude-opus-4-8");
        assert!(body["system"].as_str().unwrap().contains("commit message"));
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[tokio::test]
    async fn a_refusal_stop_reason_is_an_error() {
        let reply = json!({ "content": [], "stop_reason": "refusal" });
        let (base_url, _) = mock_anthropic(reply, axum::http::StatusCode::OK).await;
        let gen = ClaudeTextGenerator::new("sk-test", "claude-opus-4-8", base_url);
        assert!(matches!(
            gen.commit_message("x").await,
            Err(TextGenError::Refused)
        ));
    }

    #[tokio::test]
    async fn a_non_2xx_response_is_surfaced_as_an_api_error() {
        let reply = json!({ "error": { "type": "authentication_error" } });
        let (base_url, _) =
            mock_anthropic(reply, axum::http::StatusCode::UNAUTHORIZED).await;
        let gen = ClaudeTextGenerator::new("sk-test", "claude-opus-4-8", base_url);
        match gen.commit_message("x").await {
            Err(TextGenError::Api { status, .. }) => assert_eq!(status, 401),
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_empty_api_key_short_circuits_without_a_request() {
        // base_url points nowhere reachable; we must fail before hitting it.
        let gen = ClaudeTextGenerator::new("", "claude-opus-4-8", "http://127.0.0.1:1");
        assert!(matches!(
            gen.commit_message("x").await,
            Err(TextGenError::MissingApiKey)
        ));
    }
}
