//! Server-initiated `sampling/createMessage` — the mechanism that lets
//! codeup borrow the HOST's model (and the host's tokens / the user's
//! existing subscription) with no provider API key. This is the spine of
//! the LLM path in PLAN-MCP.md, requirement 2.
//!
//! Unlike the rest of MCP, this is a request the *server* sends *to the
//! client*. We allocate a fresh numeric id, register a oneshot keyed by
//! it, write the request, and await the matching response — which the
//! read loop in `mod.rs` routes back to us by id. The host shows its own
//! consent UI; we inherit it.

use super::ServerCtx;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Per-request wall-clock ceiling. A host that never answers a sampling
/// request (consent denied silently, model hung) must not wedge the scan
/// forever — we time out, clean up the pending slot, and move on.
const SAMPLING_TIMEOUT: Duration = Duration::from_secs(180);

/// Result of one sampling call: the completion text plus the model id the
/// host reported running it on (used for `detectedBy` attribution).
pub struct SamplingOutcome {
    pub text: String,
    pub model: Option<String>,
}

/// Ask the host to run one completion on its own model. Returns the text
/// of the assistant message. Errors if the client lacks sampling, denied
/// the request, or didn't answer within the timeout.
pub async fn create_message(
    ctx: &ServerCtx,
    system_prompt: &str,
    user_prompt: &str,
    max_tokens: u32,
) -> Result<SamplingOutcome> {
    if !ctx.supports_sampling.load(Ordering::SeqCst) {
        bail!("client does not advertise the `sampling` capability");
    }

    let id = ctx.next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = tokio::sync::oneshot::channel::<Value>();
    {
        let mut pending = ctx.pending.lock().expect("pending mutex poisoned");
        pending.insert(id, tx);
    }

    let request = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "sampling/createMessage",
        "params": {
            "messages": [
                { "role": "user", "content": { "type": "text", "text": user_prompt } }
            ],
            "systemPrompt": system_prompt,
            "maxTokens": max_tokens,
            // Architectural review wants a capable model, but we leave the
            // final choice to the host's own model-preference mapping.
            "modelPreferences": { "intelligencePriority": 0.8 }
        }
    });

    if ctx.out_tx.send(request.to_string()).is_err() {
        ctx.pending.lock().expect("pending mutex poisoned").remove(&id);
        bail!("transport closed before sampling request could be sent");
    }

    let response = match tokio::time::timeout(SAMPLING_TIMEOUT, rx).await {
        Ok(Ok(v)) => v,
        Ok(Err(_)) => {
            // Sender dropped — read loop closed.
            bail!("sampling response channel closed");
        }
        Err(_) => {
            // Timed out: reclaim the pending slot so a late response is
            // ignored rather than delivered to a dead receiver.
            ctx.pending.lock().expect("pending mutex poisoned").remove(&id);
            bail!("sampling request timed out after {}s", SAMPLING_TIMEOUT.as_secs());
        }
    };

    if let Some(err) = response.get("error") {
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        bail!("host rejected sampling request: {message}");
    }

    let result = response
        .get("result")
        .ok_or_else(|| anyhow!("sampling response missing result"))?;

    // The content of a sampling result is a single content block; we only
    // consume text. (Hosts may return image/audio for other use cases —
    // not relevant to a code review.)
    let text = result
        .pointer("/content/text")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow!("sampling result had no text content"))?
        .to_string();

    let model = result
        .get("model")
        .and_then(|m| m.as_str())
        .map(str::to_string);

    Ok(SamplingOutcome { text, model })
}
