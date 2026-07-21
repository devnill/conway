//! HTTP-status → `BackendError` classification, shared by every adapter
//! (architecture §"Module: conway-backends" boundary rule: adapters MUST
//! classify errors into the full `BackendError` taxonomy so the health layer
//! can distinguish endpoint-health signals from request problems).
//!
//! This module is deliberately independent of `reqwest`: it takes a raw
//! status code, a raw response body, and a raw `Retry-After` header value
//! (not `reqwest::StatusCode`/`HeaderMap`), so it compiles and is testable
//! under `--no-default-features`, with no HTTP client of its own. [`http`](crate::http)
//! is the (feature-gated) caller that extracts these primitives from a
//! `reqwest::Response`.
//!
//! Two rows of the classification table have no HTTP status at all —
//! `Transport` for reqwest connect/timeout/body/IO errors and `Cancelled`
//! for a dropped request/fired `CancellationToken` — and are therefore
//! constructed directly by `http::HttpClient`, never through [`classify`].

use std::sync::OnceLock;

use conway_core::error::BackendError;
use regex::Regex;

/// Case-insensitive; matched against the raw response body to detect a
/// context-window overflow reported as a 400/422 rather than a dedicated
/// status code (providers vary).
fn context_overflow_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)context[ _-]?length|maximum context|context window|too many tokens|prompt is too long|n_ctx",
        )
        .expect("context-overflow regex must compile")
    })
}

fn is_context_overflow(body: &str) -> bool {
    context_overflow_regex().is_match(body)
}

/// The provider `error.message` field when `body` is JSON shaped like
/// `{"error":{"message":...}}` (Anthropic and OpenAI both use this shape),
/// else the first 512 bytes of `body`.
fn extract_message(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(message) = value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(|message| message.as_str())
        {
            return message.to_string();
        }
    }
    truncate_bytes(body, 512)
}

/// Truncates `s` to at most `max` bytes, cutting back to the nearest
/// preceding `char` boundary so the result is always valid UTF-8.
fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Parses a `Retry-After` header value: either delay-seconds (`"120"`) or an
/// HTTP-date (RFC 2822). Returns `None` when absent or unparsable, or when
/// an HTTP-date has already passed (surfaced as `Some(0)`).
fn parse_retry_after(header_value: Option<&str>) -> Option<u64> {
    let raw = header_value?.trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(secs);
    }
    let when = chrono::DateTime::parse_from_rfc2822(raw).ok()?;
    let now = chrono::Utc::now();
    let delta = when.with_timezone(&chrono::Utc) - now;
    Some(delta.num_seconds().max(0) as u64)
}

/// Classifies an HTTP response into a [`BackendError`] per the module's
/// shared classification table:
///
/// | Condition | `BackendError` |
/// |---|---|
/// | 401, 403 | `Auth` |
/// | 429 | `RateLimit` (`retry_after` from `Retry-After`, secs or HTTP-date, else `None`) |
/// | 400/422 and body matches the context-length regex | `ContextOverflow` |
/// | 400, 404, 405, 413, 422 (other) | `BadRequest` |
/// | 408 | `Transport` |
/// | 5xx | `ServerError` |
/// | anything else | `BadRequest` (defensive fallback; every status this crate's adapters route through `classify` is one of the rows above) |
///
/// `required_tokens`/`max_context_tokens` on `ContextOverflow` are filled by
/// best-effort numeric extraction from the provider's message: the two
/// largest integers in the matched body are taken as `(required, max)` with
/// `required >= max` (an overflow error implies the request exceeded the
/// window — providers phrase it either "4096 > 2048" or "maximum is 8192
/// ... requested 9500", and the larger number is the request in both).
/// When the body carries fewer than two parseable integers, both fields
/// fall back to the `0` sentinel — the `u32` fields (not `Option<u32>`)
/// cannot express `None`.
pub fn classify(status: u16, body: &str, retry_after_header: Option<&str>) -> BackendError {
    match status {
        401 | 403 => BackendError::Auth {
            detail: extract_message(body),
        },
        429 => BackendError::RateLimit {
            retry_after_secs: parse_retry_after(retry_after_header),
        },
        408 => BackendError::Transport {
            detail: extract_message(body),
        },
        400 | 422 if is_context_overflow(body) => {
            let (required_tokens, max_context_tokens) = extract_overflow_numbers(body);
            BackendError::ContextOverflow {
                required_tokens,
                max_context_tokens,
            }
        }
        400 | 404 | 405 | 413 | 422 => BackendError::BadRequest {
            detail: extract_message(body),
        },
        500..=599 => BackendError::ServerError {
            status,
            detail: extract_message(body),
        },
        other => BackendError::BadRequest {
            detail: format!("unexpected status {other}: {}", extract_message(body)),
        },
    }
}

/// Classifies a `2xx` response whose body failed JSON deserialization —
/// a distinct row of the table from [`classify`], since here the status
/// itself signals success and the failure is in decoding the body.
pub fn classify_malformed_body(body: &str) -> BackendError {
    BackendError::BadRequest {
        detail: format!("malformed response body: {}", truncate_bytes(body, 512)),
    }
}

/// Best-effort extraction of `(required_tokens, max_context_tokens)` from a
/// context-overflow message body. Takes every base-10 integer in the body
/// that fits in `u32`, and returns the two largest as `(larger, smaller)` —
/// an overflow error always has `required > max`. Fewer than two integers:
/// `(0, 0)` sentinel.
fn extract_overflow_numbers(body: &str) -> (u32, u32) {
    let mut numbers: Vec<u32> = Vec::new();
    let mut current: Option<u64> = None;
    for c in body.chars() {
        if let Some(d) = c.to_digit(10) {
            current = Some(
                current
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(d as u64),
            );
        } else if let Some(n) = current.take() {
            if let Ok(n) = u32::try_from(n) {
                numbers.push(n);
            }
        }
    }
    if let Some(n) = current {
        if let Ok(n) = u32::try_from(n) {
            numbers.push(n);
        }
    }
    numbers.sort_unstable_by(|a, b| b.cmp(a));
    match numbers.as_slice() {
        [first, second, ..] => (*first, *second),
        _ => (0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_401_and_403_as_auth() {
        for status in [401u16, 403] {
            let err = classify(status, r#"{"error":{"message":"bad key"}}"#, None);
            assert!(
                matches!(err, BackendError::Auth { .. }),
                "{status}: {err:?}"
            );
        }
    }

    #[test]
    fn classifies_429_rate_limit_with_and_without_retry_after() {
        assert_eq!(
            classify(429, "{}", Some("7")),
            BackendError::RateLimit {
                retry_after_secs: Some(7)
            }
        );
        assert_eq!(
            classify(429, "{}", None),
            BackendError::RateLimit {
                retry_after_secs: None
            }
        );
    }

    #[test]
    fn classifies_408_as_transport() {
        let err = classify(408, "request timed out", None);
        assert!(matches!(err, BackendError::Transport { .. }), "{err:?}");
    }

    #[test]
    fn classifies_context_overflow_body_over_plain_bad_request() {
        for body in [
            r#"{"error":{"message":"maximum context length exceeded"}}"#,
            r#"{"error":{"message":"prompt is too long for this model"}}"#,
            r#"{"error":{"message":"n_ctx exceeded: 4096 > 2048"}}"#,
        ] {
            let err = classify(400, body, None);
            assert!(
                matches!(err, BackendError::ContextOverflow { .. }),
                "{body}: {err:?}"
            );
        }
        let err = classify(
            422,
            r#"{"error":{"message":"context window exceeded"}}"#,
            None,
        );
        assert!(matches!(err, BackendError::ContextOverflow { .. }));
    }

    #[test]
    fn classifies_other_4xx_as_bad_request() {
        for status in [400u16, 404, 405, 413, 422] {
            let err = classify(status, r#"{"error":{"message":"nope"}}"#, None);
            assert!(
                matches!(err, BackendError::BadRequest { .. }),
                "{status}: {err:?}"
            );
        }
    }

    #[test]
    fn classifies_5xx_as_server_error_with_status_preserved() {
        for status in [500u16, 502, 503, 504, 529] {
            let err = classify(status, r#"{"error":{"message":"boom"}}"#, None);
            match err {
                BackendError::ServerError { status: got, .. } => assert_eq!(got, status),
                other => panic!("{status}: expected ServerError, got {other:?}"),
            }
        }
    }

    #[test]
    fn malformed_2xx_body_is_bad_request_with_documented_prefix() {
        match classify_malformed_body("not json") {
            BackendError::BadRequest { detail } => {
                assert!(detail.starts_with("malformed response body: "), "{detail}");
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn transport_and_cancelled_rows_have_no_status_and_are_not_produced_by_classify() {
        // No HTTP status exists for a reqwest connect/timeout/IO error or a
        // dropped/cancelled request; `http::HttpClient` constructs these
        // variants directly rather than through `classify`. Asserted here at
        // the variant/health-signal level; the retry-loop wiring is covered
        // by the wiremock tests in `src/http.rs`.
        let transport = BackendError::Transport {
            detail: "connection refused".into(),
        };
        assert!(transport.is_health_signal());
        assert!(transport.is_failover_worthy());

        let cancelled = BackendError::Cancelled;
        assert!(!cancelled.is_health_signal());
        assert!(!cancelled.is_failover_worthy());
    }

    #[test]
    fn extracts_provider_error_message_from_json_error_shape() {
        match classify(400, r#"{"error":{"message":"bad field: foo"}}"#, None) {
            BackendError::BadRequest { detail } => assert_eq!(detail, "bad field: foo"),
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn falls_back_to_truncated_body_when_not_json_error_shape() {
        match classify(400, "plain text failure, not json", None) {
            BackendError::BadRequest { detail } => {
                assert_eq!(detail, "plain text failure, not json");
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn parses_retry_after_http_date() {
        let future = (chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc2822();
        let err = classify(429, "{}", Some(&future));
        match err {
            BackendError::RateLimit {
                retry_after_secs: Some(secs),
            } => assert!((110..=120).contains(&secs), "secs={secs}"),
            other => panic!("expected RateLimit, got {other:?}"),
        }
    }

    #[test]
    fn overflow_numbers_extracted_from_common_shapes() {
        assert_eq!(
            extract_overflow_numbers("n_ctx exceeded: 4096 > 2048"),
            (4096, 2048)
        );
        assert_eq!(
            extract_overflow_numbers(
                "maximum context length is 8192 tokens, however you requested 9500 tokens"
            ),
            (9500, 8192)
        );
        assert_eq!(
            extract_overflow_numbers("prompt is too long: 210000 tokens > 200000 maximum"),
            (210000, 200000)
        );
        // Fewer than two integers: sentinel.
        assert_eq!(extract_overflow_numbers("context length exceeded"), (0, 0));
        assert_eq!(
            extract_overflow_numbers("too long: 99999999999999999999"),
            (0, 0)
        );
    }
}
