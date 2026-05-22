//! Anthropic Messages API client. Handwritten — no official Rust SDK
//! exists, but the API is straightforward: POST to /v1/messages with a
//! JSON body, expect a content[] array with `tool_use` / `text` blocks.

use crate::llm::{LLMAnalyzeRequest, LLMAnalyzeResponse, ReportedToolCall};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

pub struct AnthropicClient {
    pub model: String,
    api_key: String,
    http: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(api_key: String, model: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("codeup-cli")
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .context("build reqwest client")?;
        Ok(Self { model, api_key, http })
    }

    pub async fn analyze(&self, req: LLMAnalyzeRequest<'_>) -> Result<LLMAnalyzeResponse> {
        let body = MessagesRequest {
            model: &self.model,
            max_tokens: req.max_output_tokens,
            system: req.system_prompt,
            tools: vec![AnthropicTool {
                name: &req.tool.name,
                description: &req.tool.description,
                input_schema: req.tool.input_schema.clone(),
            }],
            messages: vec![Message { role: "user", content: req.user_prompt }],
        };

        // Retry-on-429 with retry-after-aware backoff. Anthropic's
        // org-level token-bucket limits (e.g. 30k input tokens/min on
        // smaller plans) trip easily during a sustained workspace scan
        // — without this the runner just drops every finding past the
        // first ~10 files. 5 attempts × max 60s sleep keeps the worst
        // case bounded.
        let mut attempt = 0u32;
        let bytes = loop {
            attempt += 1;
            let resp = self
                .http
                .post(ENDPOINT)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .context("posting to Anthropic Messages API")?;

            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < 5 {
                let wait = retry_after_seconds(&resp).unwrap_or_else(|| {
                    // No retry-after header — exponential fallback,
                    // capped: 2s, 4s, 8s, 16s.
                    1u64 << attempt
                });
                let wait = wait.min(60);
                tracing::warn!(
                    "Anthropic 429 rate-limit; sleeping {wait}s (attempt {attempt}/5)"
                );
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                continue;
            }

            let bytes = resp.bytes().await.context("reading Anthropic response body")?;
            if !status.is_success() {
                let preview = String::from_utf8_lossy(&bytes);
                let trimmed = if preview.len() > 500 { &preview[..500] } else { &preview };
                bail!("Anthropic API returned {status}: {trimmed}");
            }
            break bytes;
        };
        let parsed: MessagesResponse = serde_json::from_slice(&bytes)
            .with_context(|| format!("decoding Anthropic response (first 500 bytes: {})", String::from_utf8_lossy(&bytes).chars().take(500).collect::<String>()))?;

        let tool_calls = parsed
            .content
            .into_iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { name, input, .. } => Some(ReportedToolCall { name, input }),
                ContentBlock::Text { .. } => None,
            })
            .collect();
        Ok(LLMAnalyzeResponse { tool_calls })
    }
}

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    tools: Vec<AnthropicTool<'a>>,
    messages: Vec<Message<'a>>,
}

#[derive(Serialize)]
struct AnthropicTool<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: serde_json::Value,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text {
        #[allow(dead_code)]
        text: String,
    },
    ToolUse {
        #[allow(dead_code)]
        id: Option<String>,
        name: String,
        input: serde_json::Value,
    },
}

// Helper used elsewhere to keep the "no Anthropic key set" path tidy.
pub fn key_from_env() -> Option<String> {
    std::env::var("ANTHROPIC_API_KEY").ok().filter(|s| !s.is_empty())
}

/// Parse the standard HTTP `Retry-After` header (seconds form). Anthropic
/// also exposes `anthropic-ratelimit-input-tokens-reset` with an absolute
/// timestamp, but the simple seconds form is enough for our purposes.
fn retry_after_seconds(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_response_with_tool_use() {
        let json = r#"{
            "content": [
                {"type": "text", "text": "I will report."},
                {"type": "tool_use", "id": "abc", "name": "report_finding", "input": {"category": "long-method", "severity": "high", "line": 5, "explanation": "x", "confidence": 0.9}}
            ]
        }"#;
        let parsed: MessagesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.content.len(), 2);
        match &parsed.content[1] {
            ContentBlock::ToolUse { name, input, .. } => {
                assert_eq!(name, "report_finding");
                assert_eq!(input.get("category").and_then(|v| v.as_str()), Some("long-method"));
            }
            _ => panic!("expected tool_use at index 1"),
        }
    }

    #[test]
    fn parses_response_with_only_text() {
        let json = r#"{"content": [{"type": "text", "text": "no findings"}]}"#;
        let parsed: MessagesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.content.len(), 1);
        assert!(matches!(parsed.content[0], ContentBlock::Text { .. }));
    }
}
