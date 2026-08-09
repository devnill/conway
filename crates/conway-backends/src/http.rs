//! The bounded transport-retry HTTP client shared by every adapter
//! (architecture §"Module: conway-backends" boundary rule: at most two
//! retries, single endpoint, never cross-backend).
//!
//! This is the sole consumer of the crate's `reqwest` dependency.
//!
//! `HttpClient` itself is not yet constructed anywhere outside this
//! module's own tests: the adapters that build one (`AnthropicBackend`,
//! `OpenAiCompatBackend`) are later work items (WI-019, WI-021). The
//! `#[allow(dead_code)]` below is scoped to this file for exactly that
//! reason and should be revisited once those adapters land.

#![allow(dead_code)]

use std::time::Duration;

use conway_core::error::BackendError;
use rand::RngExt;
use reqwest::{RequestBuilder, Response};
use tokio_util::sync::CancellationToken;

use crate::error::classify;

/// Maximum number of retries after the initial attempt — three attempts
/// total, per the module boundary rule ("at most two retries").
const MAX_RETRIES: u32 = 2;

/// Base of the full-jitter exponential backoff: `250ms`, then `500ms`.
const BASE_BACKOFF: Duration = Duration::from_millis(250);

/// Cap on the sleep applied when a `RateLimit`'s `retry_after` is honored.
const MAX_RATE_LIMIT_SLEEP: Duration = Duration::from_secs(30);

/// A thin wrapper over `reqwest::Client` implementing the bounded
/// transport-retry policy shared by every adapter. Deliberately `pub(crate)`
/// — this is an internal implementation detail of the adapters, not part of
/// this crate's public API (architecture: adapters, not the HTTP layer, are
/// `conway-backends`'s `Provides`).
pub(crate) struct HttpClient {
    inner: reqwest::Client,
    /// Consulted by adapter constructors (WI-019, WI-021) when building
    /// per-request timeouts; not read directly by `send_with_retry`, which
    /// operates on requests the caller has already configured via `make`.
    timeout: Duration,
}

impl HttpClient {
    pub(crate) fn new(inner: reqwest::Client, timeout: Duration) -> Self {
        Self { inner, timeout }
    }

    /// Builds a client with rustls (no native-tls) from a base request
    /// timeout, per the module's TLS policy.
    pub(crate) fn with_timeout(timeout: Duration) -> Result<Self, reqwest::Error> {
        let inner = reqwest::Client::builder().timeout(timeout).build()?;
        Ok(Self::new(inner, timeout))
    }

    pub(crate) fn inner(&self) -> &reqwest::Client {
        &self.inner
    }

    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Sends a request built fresh on each attempt by `make`, retrying
    /// `Transport`/`ServerError`/`RateLimit` failures at most twice (three
    /// attempts total) with full-jitter exponential backoff — `sleep(rand
    /// in 0..=base * 2^attempt)`, base `250ms` — honoring `Retry-After` for
    /// rate limits (`min(retry_after, 30s)`, in place of the jitter
    /// backoff), and aborting immediately with `BackendError::Cancelled` on
    /// cancellation during either an in-flight request or a backoff sleep.
    /// After the retry budget is exhausted, returns the last classified
    /// error unchanged.
    ///
    /// Streaming requests use this helper only for the *initial* response;
    /// mid-stream failures are never retried here (a partially consumed
    /// stream is not idempotent) — that is the caller's responsibility.
    pub(crate) async fn send_with_retry<F>(
        &self,
        make: F,
        cancel: &CancellationToken,
    ) -> Result<Response, BackendError>
    where
        F: Fn() -> RequestBuilder,
    {
        let mut attempt: u32 = 0;
        loop {
            if cancel.is_cancelled() {
                return Err(BackendError::Cancelled);
            }

            let request = make();
            let outcome = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(BackendError::Cancelled),
                result = request.send() => result,
            };

            let classified = match outcome {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        return Ok(response);
                    }
                    let retry_after = response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string);
                    let body = response.text().await.unwrap_or_default();
                    classify(status.as_u16(), &body, retry_after.as_deref())
                }
                Err(source) => BackendError::Transport {
                    detail: source.to_string(),
                },
            };

            let retryable = matches!(
                classified,
                BackendError::Transport { .. }
                    | BackendError::ServerError { .. }
                    | BackendError::RateLimit { .. }
            );

            if !retryable || attempt >= MAX_RETRIES {
                return Err(classified);
            }

            let sleep_for = match &classified {
                BackendError::RateLimit {
                    retry_after_secs: Some(secs),
                } => Duration::from_secs(*secs).min(MAX_RATE_LIMIT_SLEEP),
                _ => {
                    let max_jitter_ms = (BASE_BACKOFF.as_millis() as u64) * (1u64 << attempt);
                    let millis = rand::rng().random_range(0..=max_jitter_ms);
                    Duration::from_millis(millis)
                }
            };

            attempt += 1;

            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(BackendError::Cancelled),
                () = tokio::time::sleep(sleep_for) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn client() -> HttpClient {
        HttpClient::new(reqwest::Client::new(), Duration::from_secs(30))
    }

    #[tokio::test(start_paused = true)]
    async fn retries_twice_on_503_then_succeeds_on_200_after_exactly_three_requests() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/thing"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/thing"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let http = client();
        let inner = http.inner.clone();
        let url = format!("{}/v1/thing", server.uri());
        let cancel = CancellationToken::new();

        let result = http.send_with_retry(move || inner.get(&url), &cancel).await;

        assert!(result.is_ok(), "{result:?}");
        server.verify().await;
    }

    #[tokio::test(start_paused = true)]
    async fn does_not_retry_a_400_bad_request_and_stops_after_exactly_one_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/thing"))
            .respond_with(
                ResponseTemplate::new(400).set_body_string(r#"{"error":{"message":"bad"}}"#),
            )
            .expect(1)
            .mount(&server)
            .await;

        let http = client();
        let inner = http.inner.clone();
        let url = format!("{}/v1/thing", server.uri());
        let cancel = CancellationToken::new();

        let err = http
            .send_with_retry(move || inner.get(&url), &cancel)
            .await
            .expect_err("400 must not be retried");

        assert!(matches!(err, BackendError::BadRequest { .. }), "{err:?}");
        server.verify().await;
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limit_honors_retry_after_and_exhausts_the_budget_after_three_requests() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/thing"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "7"))
            .expect(3)
            .mount(&server)
            .await;

        let http = client();
        let inner = http.inner.clone();
        let url = format!("{}/v1/thing", server.uri());
        let cancel = CancellationToken::new();

        let err = http
            .send_with_retry(move || inner.get(&url), &cancel)
            .await
            .expect_err("rate limit must exhaust the retry budget");

        assert_eq!(
            err,
            BackendError::RateLimit {
                retry_after_secs: Some(7)
            }
        );
        server.verify().await;
    }

    #[tokio::test(start_paused = true)]
    async fn already_cancelled_token_short_circuits_before_any_request() {
        let server = MockServer::start().await;
        // No mock mounted: we assert zero requests are made at all, not any
        // particular response.

        let http = client();
        let inner = http.inner.clone();
        let url = format!("{}/v1/thing", server.uri());
        let cancel = CancellationToken::new();
        cancel.cancel();

        let err = http
            .send_with_retry(move || inner.get(&url), &cancel)
            .await
            .expect_err("a pre-cancelled token must short-circuit");

        assert_eq!(err, BackendError::Cancelled);
        let received = server.received_requests().await.unwrap_or_default();
        assert!(
            received.is_empty(),
            "expected zero requests, got {received:?}"
        );
    }
}
