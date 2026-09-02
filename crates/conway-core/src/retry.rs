//! The bounded transport-retry policy's shared numbers.
//!
//! Two independent retry loops need to agree on the SAME backoff policy
//! without literally sharing code across a crate boundary neither wants a
//! runtime dependency on: `conway-plugin-backends`'s `HttpClient::
//! send_with_retry` (whole-request retry, before a stream ever opens) and
//! `conway-runtime`'s `AttemptEngine` same-candidate stream retry (after a
//! stream opened, when a mid-stream `Transport`/`ServerError` discards the
//! partial delta and restarts the SAME candidate — board item
//! `01M1FSJ4E2S5M9KBSBJAAPJQ48`). Both crates already depend on
//! `conway-core`, so this is the one place the numbers live, rather than
//! two copies that could quietly drift apart.
//!
//! This module deliberately stops at the constants and the pure jitter-
//! window formula: the actual random draw stays with each caller (this
//! crate holds no `rand` dependency), and rate-limit `Retry-After` handling
//! is `send_with_retry`'s own concern — a stream retry never sees a
//! `RateLimit` (that classification is not eligible for the same-candidate
//! stream retry at all; see `conway-runtime::attempt`'s module doc).

use std::time::Duration;

/// Maximum number of retries after the initial attempt — three attempts
/// total, per the module boundary rule both callers implement ("at most two
/// retries, single endpoint").
pub const MAX_RETRIES: u32 = 2;

/// Base of the full-jitter exponential backoff: `250ms`, then `500ms`.
pub const BASE_BACKOFF: Duration = Duration::from_millis(250);

/// The full-jitter window's inclusive upper bound for a given zero-based
/// retry index: `base * 2^retry_index` (`250ms` for the first retry,
/// `500ms` for the second). Callers draw their own `0..=max_jitter(n)`
/// random duration — this module has no `rand` dependency, so the draw
/// itself is never shared, only the window it draws from.
pub fn max_jitter(retry_index: u32) -> Duration {
    Duration::from_millis((BASE_BACKOFF.as_millis() as u64) * (1u64 << retry_index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_jitter_doubles_per_retry_index() {
        assert_eq!(max_jitter(0), Duration::from_millis(250));
        assert_eq!(max_jitter(1), Duration::from_millis(500));
    }
}
