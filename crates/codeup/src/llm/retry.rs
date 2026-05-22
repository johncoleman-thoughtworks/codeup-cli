//! Shared HTTP retry policy for LLM provider clients.
//!
//! Anthropic and GitHub Models both surface transient failures we want
//! to retry: 429 rate-limits (token-bucket overage on Anthropic,
//! per-minute request caps on GH Models) and 5xx server errors
//! (especially Anthropic's 529 overloaded_error under load). The
//! retry shape is identical across both providers, so it lives here
//! rather than being duplicated.
//!
//! Policy:
//! - Up to 5 attempts per call.
//! - 429 prefers the server's standard `Retry-After: <seconds>` hint
//!   when present.
//! - Otherwise: exponential backoff (2s, 4s, 8s, 16s, 32s), capped at
//!   60s.
//! - 4xx other than 429 are caller mistakes (bad model, bad key, bad
//!   schema) — retrying just burns time, so they bail immediately.

/// Should this status be retried?
pub fn should_retry(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// Parse the standard HTTP `Retry-After` header (seconds form). Both
/// providers send the integer-seconds variant; the HTTP-date variant
/// of Retry-After is not used.
pub fn retry_after_seconds(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// Exponential backoff seconds for attempt N (1-indexed): 2, 4, 8, 16, 32.
pub fn backoff_seconds(attempt: u32) -> u64 {
    1u64 << attempt
}

/// Convenience: maximum sleep we'll ever do between retries.
pub const MAX_BACKOFF_SECONDS: u64 = 60;

/// Convenience: maximum attempts per call.
pub const MAX_ATTEMPTS: u32 = 5;

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn should_retry_covers_429_and_5xx_only() {
        assert!(should_retry(StatusCode::TOO_MANY_REQUESTS));
        assert!(should_retry(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(should_retry(StatusCode::BAD_GATEWAY));
        assert!(should_retry(StatusCode::SERVICE_UNAVAILABLE));
        assert!(should_retry(StatusCode::GATEWAY_TIMEOUT));
        // Anthropic's overloaded_error
        assert!(should_retry(StatusCode::from_u16(529).unwrap()));
        // Not these:
        assert!(!should_retry(StatusCode::OK));
        assert!(!should_retry(StatusCode::BAD_REQUEST));
        assert!(!should_retry(StatusCode::UNAUTHORIZED));
        assert!(!should_retry(StatusCode::FORBIDDEN));
        assert!(!should_retry(StatusCode::NOT_FOUND));
    }

    #[test]
    fn backoff_seconds_doubles_per_attempt() {
        assert_eq!(backoff_seconds(1), 2);
        assert_eq!(backoff_seconds(2), 4);
        assert_eq!(backoff_seconds(3), 8);
        assert_eq!(backoff_seconds(4), 16);
        assert_eq!(backoff_seconds(5), 32);
    }
}
