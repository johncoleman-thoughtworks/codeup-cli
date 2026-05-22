//! Anthropic Messages API client. Handwritten — no official Rust SDK
//! exists, but the API is straightforward: POST to /v1/messages with a
//! JSON body, expect a content[] array with `tool_use` / `text` blocks.

use crate::llm::retry::{backoff_seconds, retry_after_seconds, should_retry, MAX_ATTEMPTS, MAX_BACKOFF_SECONDS};
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

        // Retry loop covering both:
        //   - 429 Too Many Requests (org-level token-bucket limits e.g.
        //     30k input tokens/min on smaller plans — trips easily on a
        //     sustained workspace scan).
        //   - 5xx server errors, especially 529 overloaded_error, which
        //     Anthropic returns under load. These are transient and
        //     usually succeed on retry within a few seconds.
        //
        // Both share the same attempt budget (5) so a single call can't
        // sit in here forever. 429 prefers the server's `Retry-After`
        // hint; 5xx uses straight exponential backoff (no Retry-After
        // is sent on those).
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
            if should_retry(status) && attempt < MAX_ATTEMPTS {
                let wait = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    retry_after_seconds(&resp).unwrap_or_else(|| backoff_seconds(attempt))
                } else {
                    // 5xx: no Retry-After expected, exponential backoff.
                    backoff_seconds(attempt)
                };
                let wait = wait.min(MAX_BACKOFF_SECONDS);
                let kind = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    "429 rate-limit"
                } else {
                    "5xx server error"
                };
                tracing::warn!(
                    "Anthropic {kind} ({status}); sleeping {wait}s (attempt {attempt}/{MAX_ATTEMPTS})"
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
