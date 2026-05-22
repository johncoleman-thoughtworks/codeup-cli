//! GitHub Models client.
//!
//! GitHub Models (<https://github.com/marketplace/models>) hosts a fleet of
//! frontier models behind an OpenAI-compatible chat-completions endpoint
//! at `models.github.ai`. Free for individuals with a `GITHUB_TOKEN` (the
//! same token that Actions provides automatically), which makes it the
//! ideal fallback when no Anthropic key is configured — the dogfood
//! workflow gets a real LLM pass with zero secret setup.
//!
//! The wire format is OpenAI's, not Anthropic's. Three deltas to keep
//! in mind:
//! - Tools live under `tools: [{type: "function", function: {...}}]`
//!   with the schema in `parameters`, not `input_schema`.
//! - The model is forced toward the tool with `tool_choice: {type:
//!   "function", function: {name}}` — without this many models prefer
//!   plain text and the analyzer's tool-call extraction comes back empty.
//! - Tool arguments are returned as a **JSON-encoded string** in
//!   `tool_calls[].function.arguments`, which we parse before handing
//!   back to the shared `LLMAnalyzeResponse` type.

use crate::llm::retry::{backoff_seconds, retry_after_seconds, should_retry, MAX_ATTEMPTS, MAX_BACKOFF_SECONDS};
use crate::llm::{LLMAnalyzeRequest, LLMAnalyzeResponse, ReportedToolCall};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const ENDPOINT: &str = "https://models.github.ai/inference/chat/completions";

pub struct GithubModelsClient {
    pub model: String,
    api_key: String,
    http: reqwest::Client,
}

impl GithubModelsClient {
    pub fn new(api_key: String, model: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("codeup-cli")
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .context("build reqwest client")?;
        Ok(Self { model, api_key, http })
    }

    pub async fn analyze(&self, req: LLMAnalyzeRequest<'_>) -> Result<LLMAnalyzeResponse> {
        let body = ChatRequest {
            model: &self.model,
            max_tokens: req.max_output_tokens,
            messages: vec![
                Message { role: "system", content: req.system_prompt },
                Message { role: "user", content: req.user_prompt },
            ],
            tools: vec![ToolWrapper {
                kind: "function",
                function: FunctionDef {
                    name: &req.tool.name,
                    description: &req.tool.description,
                    parameters: req.tool.input_schema.clone(),
                },
            }],
            // Force the model toward the tool — without this, weaker
            // GH Models commonly return plain text and we get zero
            // findings even when there's clear signal.
            tool_choice: ToolChoice {
                kind: "function",
                function: ToolChoiceFn { name: &req.tool.name },
            },
        };

        // Same retry shape as the Anthropic client — GH Models also
        // imposes per-minute request limits and occasionally surfaces
        // 5xx from upstream providers. See anthropic.rs for the
        // rationale; the policy is identical so the two providers
        // behave consistently when wrapped behind the LLMClient enum.
        let mut attempt = 0u32;
        let bytes = loop {
            attempt += 1;
            let resp = self
                .http
                .post(ENDPOINT)
                .header("authorization", format!("Bearer {}", self.api_key))
                .header("content-type", "application/json")
                .header("accept", "application/json")
                .json(&body)
                .send()
                .await
                .context("posting to GitHub Models chat-completions API")?;

            let status = resp.status();
            if should_retry(status) && attempt < MAX_ATTEMPTS {
                let wait = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    retry_after_seconds(&resp).unwrap_or_else(|| backoff_seconds(attempt))
                } else {
                    backoff_seconds(attempt)
                };
                let wait = wait.min(MAX_BACKOFF_SECONDS);
                let kind = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    "429 rate-limit"
                } else {
                    "5xx server error"
                };
                tracing::warn!(
                    "GitHub Models {kind} ({status}); sleeping {wait}s (attempt {attempt}/{MAX_ATTEMPTS})"
                );
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                continue;
            }

            let bytes = resp
                .bytes()
                .await
                .context("reading GitHub Models response body")?;
            if !status.is_success() {
                let preview = String::from_utf8_lossy(&bytes);
                let trimmed = if preview.len() > 500 { &preview[..500] } else { &preview };
                bail!("GitHub Models API returned {status}: {trimmed}");
            }
            break bytes;
        };
        let parsed: ChatResponse = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "decoding GitHub Models response (first 500 bytes: {})",
                String::from_utf8_lossy(&bytes)
                    .chars()
                    .take(500)
                    .collect::<String>()
            )
        })?;

        let mut tool_calls = Vec::new();
        for choice in parsed.choices {
            for call in choice.message.tool_calls.unwrap_or_default() {
                // `arguments` is a JSON-encoded string per the OpenAI
                // spec. Some providers occasionally send a raw object;
                // accept both shapes.
                let input: serde_json::Value = match call.function.arguments {
                    ArgumentsField::String(s) => serde_json::from_str(&s)
                        .with_context(|| format!("decoding tool call arguments: {s}"))?,
                    ArgumentsField::Value(v) => v,
                };
                tool_calls.push(ReportedToolCall {
                    name: call.function.name,
                    input,
                });
            }
        }
        Ok(LLMAnalyzeResponse { tool_calls })
    }
}

pub fn token_from_env() -> Option<String> {
    std::env::var("GITHUB_TOKEN").ok().filter(|s| !s.is_empty())
}

// ---- wire types ----

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<Message<'a>>,
    tools: Vec<ToolWrapper<'a>>,
    tool_choice: ToolChoice<'a>,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ToolWrapper<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    function: FunctionDef<'a>,
}

#[derive(Serialize)]
struct FunctionDef<'a> {
    name: &'a str,
    description: &'a str,
    parameters: serde_json::Value,
}

#[derive(Serialize)]
struct ToolChoice<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    function: ToolChoiceFn<'a>,
}

#[derive(Serialize)]
struct ToolChoiceFn<'a> {
    name: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Deserialize)]
struct ToolCall {
    function: ToolCallFunction,
}

#[derive(Deserialize)]
struct ToolCallFunction {
    name: String,
    arguments: ArgumentsField,
}

/// OpenAI's spec says arguments is always a JSON-encoded string, but in
/// practice some compatible providers (and some test fixtures) send a
/// raw object. Accept either to stay defensive.
#[derive(Deserialize)]
#[serde(untagged)]
enum ArgumentsField {
    String(String),
    Value(serde_json::Value),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_response_with_tool_call_string_arguments() {
        // The canonical OpenAI shape: arguments as a JSON-encoded string.
        let json = r#"{
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "report_finding",
                            "arguments": "{\"category\":\"long-method\",\"severity\":\"high\",\"line\":5,\"explanation\":\"x\",\"confidence\":0.9}"
                        }
                    }]
                }
            }]
        }"#;
        let parsed: ChatResponse = serde_json::from_str(json).unwrap();
        let call = &parsed.choices[0].message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.function.name, "report_finding");
        match &call.function.arguments {
            ArgumentsField::String(s) => {
                let v: serde_json::Value = serde_json::from_str(s).unwrap();
                assert_eq!(v.get("category").and_then(|x| x.as_str()), Some("long-method"));
            }
            ArgumentsField::Value(_) => panic!("expected string variant"),
        }
    }

    #[test]
    fn parses_response_with_tool_call_object_arguments() {
        // Non-conforming-but-real shape some providers emit.
        let json = r#"{
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "function": {
                            "name": "report_finding",
                            "arguments": {"category": "dead-code", "severity": "low"}
                        }
                    }]
                }
            }]
        }"#;
        let parsed: ChatResponse = serde_json::from_str(json).unwrap();
        let call = &parsed.choices[0].message.tool_calls.as_ref().unwrap()[0];
        match &call.function.arguments {
            ArgumentsField::Value(v) => {
                assert_eq!(v.get("category").and_then(|x| x.as_str()), Some("dead-code"));
            }
            ArgumentsField::String(_) => panic!("expected value variant"),
        }
    }

    #[test]
    fn parses_response_with_no_tool_call() {
        // Model declined to call the tool — analyzer should see zero findings.
        let json = r#"{"choices": [{"message": {"content": "no findings"}}]}"#;
        let parsed: ChatResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.choices[0].message.tool_calls.is_none());
    }
}
