//! LLM client abstraction over Anthropic + GitHub Models.
//!
//! Enum dispatch rather than `dyn Trait` — there are only ever two
//! providers, and the enum dodges the async-trait-object boilerplate.

pub mod anthropic;
pub mod github_models;
pub mod provider;
mod retry;

use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderName {
    Anthropic,
    GithubModels,
}

impl ProviderName {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderName::Anthropic => "anthropic",
            ProviderName::GithubModels => "github-models",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

pub struct LLMAnalyzeRequest<'a> {
    pub system_prompt: &'a str,
    pub user_prompt: &'a str,
    pub tool: &'a ToolDefinition,
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct ReportedToolCall {
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct LLMAnalyzeResponse {
    pub tool_calls: Vec<ReportedToolCall>,
}

pub enum LLMClient {
    Anthropic(anthropic::AnthropicClient),
    GithubModels(github_models::GithubModelsClient),
}

impl LLMClient {
    pub fn provider(&self) -> ProviderName {
        match self {
            LLMClient::Anthropic(_) => ProviderName::Anthropic,
            LLMClient::GithubModels(_) => ProviderName::GithubModels,
        }
    }

    pub fn model(&self) -> String {
        match self {
            LLMClient::Anthropic(c) => c.model.clone(),
            LLMClient::GithubModels(c) => c.model.clone(),
        }
    }

    pub async fn analyze(&self, req: LLMAnalyzeRequest<'_>) -> Result<LLMAnalyzeResponse> {
        match self {
            LLMClient::Anthropic(c) => c.analyze(req).await,
            LLMClient::GithubModels(c) => c.analyze(req).await,
        }
    }
}
