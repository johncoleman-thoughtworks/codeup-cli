//! GitHub Models client — stub for Phase 2.x.
//!
//! GitHub Models exposes Anthropic Claude via an OpenAI-compatible chat
//! endpoint at `models.github.ai/inference/chat/completions`. Tool calls
//! use OpenAI's `tools: [{type: "function", function: {...}}]` shape
//! rather than Anthropic's native format, so it warrants its own
//! request/response wiring and a focused round-trip verification pass
//! against the proxy. That's the Phase 2.x patch.
//!
//! For now: returning a clear, actionable error so users hitting this
//! path know exactly where they are.

use crate::llm::{LLMAnalyzeRequest, LLMAnalyzeResponse};
use anyhow::{bail, Result};

pub struct GithubModelsClient {
    pub model: String,
    #[allow(dead_code)]
    api_key: String,
}

impl GithubModelsClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self { model, api_key }
    }

    pub async fn analyze(&self, _req: LLMAnalyzeRequest<'_>) -> Result<LLMAnalyzeResponse> {
        bail!(
            "GitHub Models provider is not yet wired up (planned for v0.2.x). Use --provider anthropic for now, or set ANTHROPIC_API_KEY and re-run without --provider."
        )
    }
}

pub fn token_from_env() -> Option<String> {
    std::env::var("GITHUB_TOKEN").ok().filter(|s| !s.is_empty())
}
