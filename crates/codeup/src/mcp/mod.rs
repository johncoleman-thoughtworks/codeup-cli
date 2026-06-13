//! `codeup mcp` — a local MCP server (stdio) that exposes codeup's
//! analysis to MCP hosts (GitHub Copilot / VS Code, Claude Desktop,
//! Cursor, Claude Code) with no LLM provider key. The deterministic
//! engine runs locally and keyless; the catalogue review borrows the
//! host's own model via MCP **sampling** (see `sampling.rs`). Design:
//! PLAN-MCP.md in the repo root.

mod protocol;
mod sampling;
mod tools;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

/// Default protocol version we report when the client doesn't pin one.
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

/// Shared server state, cloned (via Arc) into each per-request task.
pub struct ServerCtx {
    /// Workspace root the server operates on (its launch cwd).
    pub root: PathBuf,
    /// Outbound message queue → serialized to stdout by the writer task.
    /// Carries both responses to the client and our server-initiated
    /// sampling requests, so writes never interleave on the wire.
    pub out_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// Pending server-initiated requests (sampling), keyed by the numeric
    /// id we allocated. The read loop routes matching responses back here.
    pub pending: Mutex<HashMap<i64, oneshot::Sender<Value>>>,
    /// Monotonic id source for server-initiated requests.
    pub next_id: AtomicI64,
    /// Whether the client advertised the `sampling` capability at
    /// `initialize`. Gates the host-tokens review path.
    pub supports_sampling: AtomicBool,
    /// Serializes tool execution. Requests are dispatched on their own
    /// tasks (so a tool blocked on sampling doesn't stall the read loop),
    /// but tools touch the shared `.codeup/` store, so we run them one at
    /// a time to avoid read-before-write races between, say, a
    /// `codeup_save_findings` and a following `codeup_list_findings`. This
    /// cannot deadlock with sampling: the sampling *response* is routed by
    /// the read loop, which never takes this lock.
    pub tool_lock: tokio::sync::Mutex<()>,
}

/// Run the MCP server over stdio until stdin closes.
pub async fn serve(root: PathBuf) -> Result<()> {
    let root = root
        .canonicalize()
        .unwrap_or(root); // canonicalize for stable workspace-relative paths; fall back if it fails.

    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // Single writer task — owns stdout, drains the outbound queue.
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = out_rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if stdout.write_all(b"\n").await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    });

    let ctx = Arc::new(ServerCtx {
        root,
        out_tx,
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicI64::new(1),
        supports_sampling: AtomicBool::new(false),
        tool_lock: tokio::sync::Mutex::new(()),
    });

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    while let Some(line) = lines.next_line().await.context("reading stdin")? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("ignoring unparseable JSON-RPC line: {e}");
                continue;
            }
        };

        // A response to one of OUR server-initiated requests has an `id`
        // and no `method`. Route it to the waiting sampling call.
        if msg.get("method").is_none() && msg.get("id").is_some() {
            if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
                let waiter = ctx.pending.lock().expect("pending mutex poisoned").remove(&id);
                if let Some(tx) = waiter {
                    let _ = tx.send(msg);
                }
            }
            continue;
        }

        // A request or notification from the client. Spawn so a tool that
        // blocks on sampling doesn't stall the read loop (the read loop
        // must keep running to deliver that very sampling response).
        let ctx = ctx.clone();
        tokio::spawn(async move {
            handle_incoming(ctx, msg).await;
        });
    }

    drop(ctx);
    let _ = writer.await;
    Ok(())
}

async fn handle_incoming(ctx: Arc<ServerCtx>, msg: Value) {
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = msg.get("id").cloned();

    match method {
        "initialize" => {
            // Capture the client's sampling capability BEFORE replying —
            // the client waits for our response before sending tools/call,
            // so this is observed by the time any review runs.
            let supports = msg.pointer("/params/capabilities/sampling").is_some();
            ctx.supports_sampling.store(supports, Ordering::SeqCst);
            let proto = msg
                .pointer("/params/protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(DEFAULT_PROTOCOL_VERSION)
                .to_string();
            let result = json!({
                "protocolVersion": proto,
                "capabilities": { "tools": {}, "prompts": {} },
                "serverInfo": { "name": "codeup", "version": env!("CARGO_PKG_VERSION") }
            });
            reply(&ctx, id, result);
        }
        // Notifications — no response.
        "notifications/initialized" | "notifications/cancelled" | "initialized" => {}
        "ping" => reply(&ctx, id, json!({})),
        "tools/list" => reply(&ctx, id, json!({ "tools": tools::list() })),
        "tools/call" => {
            let name = msg
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args = msg.pointer("/params/arguments").cloned().unwrap_or(json!({}));
            let _guard = ctx.tool_lock.lock().await;
            let result = match tools::call(&ctx, &name, args).await {
                Ok(content) => content,
                Err(e) => protocol::error_content(format!("{e:#}")),
            };
            drop(_guard);
            reply(&ctx, id, result);
        }
        "prompts/list" => reply(&ctx, id, json!({ "prompts": tools::prompt_list() })),
        "prompts/get" => {
            let args = msg.pointer("/params/arguments").cloned().unwrap_or(json!({}));
            let result = tools::prompt_get(&ctx, args);
            match result {
                Ok(r) => reply(&ctx, id, r),
                Err(e) => reply_error(&ctx, id, protocol::METHOD_NOT_FOUND, &format!("{e:#}")),
            }
        }
        other => {
            if id.is_some() {
                reply_error(&ctx, id, protocol::METHOD_NOT_FOUND, &format!("method not found: {other}"));
            }
        }
    }
}

/// Send a success response if this was a request (has an id). Notifications
/// (no id) get nothing.
fn reply(ctx: &ServerCtx, id: Option<Value>, result: Value) {
    if let Some(id) = id {
        let _ = ctx.out_tx.send(protocol::success(&id, result));
    }
}

fn reply_error(ctx: &ServerCtx, id: Option<Value>, code: i64, message: &str) {
    if let Some(id) = id {
        let _ = ctx.out_tx.send(protocol::error(&id, code, message));
    }
}
