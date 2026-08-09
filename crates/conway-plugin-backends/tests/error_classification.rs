//! Integration tests for `conway_plugin_backends::error::classify`: one assertion
//! (or small loop of assertions) per row of the classification table in the
//! WI-016 implementation notes. The two rows with no HTTP status
//! (`Transport` for connect/timeout/IO errors, `Cancelled` for a
//! dropped/cancelled request) are asserted at the variant/health-signal
//! level here — `http::HttpClient` constructs them directly, never through
//! `classify`, and the retry-loop wiring for the status-driven rows is
//! covered by the wiremock tests inside `src/http.rs`.

use conway_plugin_backends::error::{classify, classify_malformed_body};
use conway_core::error::BackendError;

#[test]
fn row_401_403_maps_to_auth() {
    for status in [401u16, 403] {
        let err = classify(status, r#"{"error":{"message":"bad key"}}"#, None);
        assert!(
            matches!(err, BackendError::Auth { .. }),
            "{status}: {err:?}"
        );
    }
}

#[test]
fn row_429_maps_to_rate_limit_with_retry_after_from_header_else_none() {
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
fn row_400_422_context_length_body_maps_to_context_overflow() {
    for (status, body) in [
        (
            400u16,
            r#"{"error":{"message":"maximum context length exceeded"}}"#,
        ),
        (422u16, r#"{"error":{"message":"context window exceeded"}}"#),
        (400u16, r#"{"error":{"message":"prompt is too long"}}"#),
        (
            400u16,
            r#"{"error":{"message":"too many tokens for this model"}}"#,
        ),
        (400u16, r#"{"error":{"message":"n_ctx exceeded"}}"#),
    ] {
        let err = classify(status, body, None);
        assert!(
            matches!(err, BackendError::ContextOverflow { .. }),
            "{status} {body}: {err:?}"
        );
    }
}

#[test]
fn row_400_404_405_413_422_other_maps_to_bad_request() {
    for status in [400u16, 404, 405, 413, 422] {
        let err = classify(status, r#"{"error":{"message":"nope"}}"#, None);
        assert!(
            matches!(err, BackendError::BadRequest { .. }),
            "{status}: {err:?}"
        );
    }
}

#[test]
fn row_408_maps_to_transport() {
    let err = classify(408, "request timed out", None);
    assert!(matches!(err, BackendError::Transport { .. }), "{err:?}");
}

#[test]
fn row_5xx_maps_to_server_error_preserving_status() {
    for status in [500u16, 502, 503, 504, 529] {
        let err = classify(status, r#"{"error":{"message":"boom"}}"#, None);
        match err {
            BackendError::ServerError { status: got, .. } => assert_eq!(got, status),
            other => panic!("{status}: expected ServerError, got {other:?}"),
        }
    }
}

#[test]
fn row_2xx_malformed_body_maps_to_bad_request_with_prefix() {
    match classify_malformed_body("not json at all") {
        BackendError::BadRequest { detail } => {
            assert!(
                detail.starts_with("malformed response body: "),
                "{detail:?}"
            );
        }
        other => panic!("expected BadRequest, got {other:?}"),
    }
}

#[test]
fn row_transport_no_http_status_is_a_failover_worthy_health_signal() {
    let transport = BackendError::Transport {
        detail: "connection refused".into(),
    };
    assert!(transport.is_health_signal());
    assert!(transport.is_failover_worthy());
}

#[test]
fn row_cancelled_no_http_status_is_not_a_health_signal_and_not_failover_worthy() {
    let cancelled = BackendError::Cancelled;
    assert!(!cancelled.is_health_signal());
    assert!(!cancelled.is_failover_worthy());
}

#[test]
fn provider_error_message_shape_is_extracted_verbatim() {
    match classify(400, r#"{"error":{"message":"bad field: foo"}}"#, None) {
        BackendError::BadRequest { detail } => assert_eq!(detail, "bad field: foo"),
        other => panic!("expected BadRequest, got {other:?}"),
    }
}

#[test]
fn non_json_body_falls_back_to_truncated_raw_body() {
    match classify(400, "plain text failure, not json", None) {
        BackendError::BadRequest { detail } => {
            assert_eq!(detail, "plain text failure, not json");
        }
        other => panic!("expected BadRequest, got {other:?}"),
    }
}

#[test]
fn overlong_body_is_truncated_to_512_bytes_on_the_fallback_path() {
    let body = "x".repeat(1000);
    match classify(400, &body, None) {
        BackendError::BadRequest { detail } => assert_eq!(detail.len(), 512),
        other => panic!("expected BadRequest, got {other:?}"),
    }
}
