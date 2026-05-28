//! Provider resolution: auto | anthropic | github-models.

use super::{anthropic, github_models, LLMClient};
use anyhow::{anyhow, Result};

// Default model picks. Anthropic stays on the latest Sonnet — quality
// matters for the analyzer's tool-use reasoning. GitHub Models defaults
// to `openai/gpt-4o-mini` because (a) it's free-tier-friendly so the
// dogfood fallback costs nothing, (b) its tool-use is reliable, and
// (c) it's definitely in the GH Models catalogue. Users wanting a
// stronger model pass --model openai/gpt-4o or similar.
const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_GH_MODELS_MODEL: &str = "openai/gpt-4o-mini";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSetting {
    Auto,
    Anthropic,
    GithubModels,
}

impl ProviderSetting {
    pub fn parse(s: Option<&str>) -> Result<Self> {
        match s.map(|x| x.to_ascii_lowercase()).as_deref() {
            None | Some("auto") | Some("") => Ok(ProviderSetting::Auto),
            Some("anthropic") => Ok(ProviderSetting::Anthropic),
            Some("github-models") | Some("github") | Some("ghm") => Ok(ProviderSetting::GithubModels),
            Some(other) => Err(anyhow!("unknown provider {other:?}. Use anthropic, github-models, or auto.")),
        }
    }
}

pub struct ResolvedProvider {
    pub client: LLMClient,
    pub reason: String,
}

/// Typed credential slots. Each variant can only be constructed with the
/// matching key — the provider resolver never silently substitutes one
/// for the other, so an ANTHROPIC_API_KEY can never end up in the
/// Authorization header of a GitHub Models call.
#[derive(Debug, Clone)]
pub struct AnthropicKey(pub String);

#[derive(Debug, Clone)]
pub struct GitHubToken(pub String);

/// Resolve a provider given the CLI flags + environment. Each provider's
/// credential is resolved from its own typed slot — there is deliberately
/// no shared fallback that would let a key for one destination be sent
/// to another. (Mitigates credential cross-vendor leakage.)
pub fn resolve(
    setting: ProviderSetting,
    anthropic_key: Option<&str>,
    github_token: Option<&str>,
    model_override: Option<&str>,
) -> Result<ResolvedProvider> {
    let anthropic_key: Option<AnthropicKey> = anthropic_key
        .map(|s| AnthropicKey(s.to_string()))
        .or_else(|| anthropic::key_from_env().map(AnthropicKey));
    let github_token: Option<GitHubToken> = github_token
        .map(|s| GitHubToken(s.to_string()))
        .or_else(|| github_models::token_from_env().map(GitHubToken));

    match setting {
        ProviderSetting::Anthropic => {
            let key = anthropic_key.ok_or_else(|| {
                anyhow!(
                    "--provider anthropic requires an Anthropic key. Pass --anthropic-api-key or set ANTHROPIC_API_KEY. (GITHUB_TOKEN is not accepted here.)"
                )
            })?;
            let model = model_override.unwrap_or(DEFAULT_ANTHROPIC_MODEL).to_string();
            Ok(ResolvedProvider {
                client: LLMClient::Anthropic(anthropic::AnthropicClient::new(key.0, model)?),
                reason: "--provider anthropic".into(),
            })
        }
        ProviderSetting::GithubModels => {
            let token = github_token.ok_or_else(|| {
                anyhow!(
                    "--provider github-models requires a GitHub token. Pass --github-token or set GITHUB_TOKEN. (ANTHROPIC_API_KEY is not accepted here.)"
                )
            })?;
            let model = model_override.unwrap_or(DEFAULT_GH_MODELS_MODEL).to_string();
            Ok(ResolvedProvider {
                client: LLMClient::GithubModels(github_models::GithubModelsClient::new(token.0, model)?),
                reason: "--provider github-models".into(),
            })
        }
        ProviderSetting::Auto => {
            if let Some(key) = anthropic_key {
                let model = model_override.unwrap_or(DEFAULT_ANTHROPIC_MODEL).to_string();
                return Ok(ResolvedProvider {
                    client: LLMClient::Anthropic(anthropic::AnthropicClient::new(key.0, model)?),
                    reason: "auto: ANTHROPIC_API_KEY present".into(),
                });
            }
            if let Some(token) = github_token {
                let model = model_override.unwrap_or(DEFAULT_GH_MODELS_MODEL).to_string();
                return Ok(ResolvedProvider {
                    client: LLMClient::GithubModels(github_models::GithubModelsClient::new(token.0, model)?),
                    reason: "auto: no Anthropic key, falling back to GITHUB_TOKEN".into(),
                });
            }
            Err(anyhow!(
                "No LLM credentials found. Set ANTHROPIC_API_KEY (or --anthropic-api-key), set GITHUB_TOKEN (or --github-token), or use --deterministic-only to skip the LLM pass."
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_setting_variants() {
        assert_eq!(ProviderSetting::parse(None).unwrap(), ProviderSetting::Auto);
        assert_eq!(ProviderSetting::parse(Some("auto")).unwrap(), ProviderSetting::Auto);
        assert_eq!(ProviderSetting::parse(Some("anthropic")).unwrap(), ProviderSetting::Anthropic);
        assert_eq!(ProviderSetting::parse(Some("github-models")).unwrap(), ProviderSetting::GithubModels);
        assert_eq!(ProviderSetting::parse(Some("ghm")).unwrap(), ProviderSetting::GithubModels);
        assert!(ProviderSetting::parse(Some("bogus")).is_err());
    }
}
