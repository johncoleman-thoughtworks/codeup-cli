//! Minimal JSON-RPC 2.0 helpers for the MCP stdio transport.
//!
//! MCP over stdio is newline-delimited JSON-RPC 2.0: one message per line,
//! no embedded newlines. We hand-roll the framing rather than pull in a
//! full MCP SDK — the surface we need (initialize, tools, prompts, plus a
//! single server-initiated `sampling/createMessage`) is small, and a
//! hand-rolled server keeps the dependency footprint and binary size in
//! line with the rest of codeup (no chrono, no heavy frameworks).

use serde_json::{json, Value};

pub const JSONRPC: &str = "2.0";

// Standard JSON-RPC error codes we use.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// A successful response. `id` is echoed verbatim so the client can
/// correlate — it may be a number or a string per the spec.
pub fn success(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": JSONRPC, "id": id, "result": result }).to_string()
}

/// A JSON-RPC protocol-level error (bad method, malformed params). Tool
/// *execution* failures are NOT these — see `error_content`.
pub fn error(id: &Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": JSONRPC, "id": id, "error": { "code": code, "message": message } })
        .to_string()
}

/// A `tools/call` result carrying text content. MCP tool results are a
/// `content[]` array; we return a single text block (JSON-as-text for
/// structured tools, prose for summaries).
pub fn text_content(text: String) -> Value {
    json!({ "content": [ { "type": "text", "text": text } ], "isError": false })
}

/// A `tools/call` result flagged as an error. Per MCP, tool execution
/// errors are reported *in the result* with `isError: true` (so the model
/// can see and react to them), not as JSON-RPC protocol errors.
pub fn error_content(text: String) -> Value {
    json!({ "content": [ { "type": "text", "text": text } ], "isError": true })
}
