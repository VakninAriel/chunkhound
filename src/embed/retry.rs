#![expect(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use rand::Rng;

use crate::error::PipelineError;

// ── RetryPolicy ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub jitter: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            jitter: true,
        }
    }
}

// ── classify_http_response ──────────────────────────────────────────────────

/// Map an HTTP response to a typed `PipelineError`.
///
/// Takes ownership of the response so the body can be read for 4xx
/// classification (context-length checks).
pub fn classify_http_response(
    provider: &str,
    response: reqwest::blocking::Response,
) -> Result<(), PipelineError> {
    let status = response.status().as_u16();

    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(PipelineError::Auth {
            provider: provider.into(),
            message: format!("HTTP {status}"),
        }),
        429 => {
            let retry_after_secs = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            Err(PipelineError::RateLimited {
                provider: provider.into(),
                retry_after_secs,
            })
        }
        400..=499 => {
            let body = response.text().unwrap_or_default();
            if body.contains("context length") || body.contains("maximum context length") {
                Err(PipelineError::ContextLengthExceeded(body))
            } else {
                Err(PipelineError::BadRequest {
                    provider: provider.into(),
                    message: body,
                })
            }
        }
        500..=599 => Err(PipelineError::ProviderError {
            provider: provider.into(),
            message: format!("HTTP {status}"),
        }),
        _ => Err(PipelineError::ProviderError {
            provider: provider.into(),
            message: format!("unexpected HTTP {status}"),
        }),
    }
}

// ── retry helpers ───────────────────────────────────────────────────────────

/// Returns `true` when the error variant permits retrying.
pub fn is_retryable(e: &PipelineError) -> bool {
    matches!(
        e,
        PipelineError::ProviderError { .. }
            | PipelineError::RateLimited { .. }
            | PipelineError::ContextLengthExceeded(_)
    )
}

/// Sleep for `duration` with optional 0–25 % jitter.
pub fn sleep_with_jitter(duration: Duration, jitter: bool) {
    let actual = if jitter {
        let jitter_pct: f64 = rand::thread_rng().gen_range(0.0..0.25);
        duration.mul_f64(1.0 + jitter_pct)
    } else {
        duration
    };
    thread::sleep(actual);
}

// ── embed_with_retry ────────────────────────────────────────────────────────

/// Call `f` repeatedly with exponential backoff until it succeeds, fails
/// fatally, or the retry budget is exhausted.
///
/// - 429 (rate-limit): use the `Retry-After` value; do **not** compound.
/// - `ContextLengthExceeded`: propagated immediately.
/// - `Auth` / `BadRequest` / `Cancelled`: returned immediately.
/// - Cancellation is checked before every attempt via `cancelled`.
pub fn embed_with_retry<F>(
    provider: &str,
    mut f: F,
    policy: &RetryPolicy,
    cancelled: &AtomicBool,
) -> Result<Vec<Vec<f32>>, PipelineError>
where
    F: FnMut() -> Result<Vec<Vec<f32>>, PipelineError>,
{
    let mut attempt: u32 = 0;

    loop {
        // ── cancellation check ──────────────────────────────────────────
        if cancelled.load(Ordering::Acquire) {
            return Err(PipelineError::Cancelled);
        }

        attempt += 1;
        let last_attempt = attempt >= policy.max_attempts;

        match f() {
            Ok(vectors) => return Ok(vectors),
            Err(e) => {
                // fatal / immediate errors
                if matches!(
                    &e,
                    PipelineError::Auth { .. }
                        | PipelineError::BadRequest { .. }
                        | PipelineError::Cancelled
                ) {
                    return Err(e);
                }

                // ContextLengthExceeded is propagated, not retried
                if matches!(&e, PipelineError::ContextLengthExceeded(_)) {
                    return Err(e);
                }

                if last_attempt {
                    return Err(e);
                }

                // ── rate-limit: use Retry-After, no compounding ─────────
                let delay = if let PipelineError::RateLimited {
                    retry_after_secs: Some(secs),
                    ..
                } = &e
                {
                    Duration::from_secs(*secs)
                } else {
                    // exponential backoff with cap
                    let base = policy.base_delay.as_millis() as u64;
                    let raw = base.saturating_mul(1 << attempt.saturating_sub(1));
                    let capped = raw.min(policy.max_delay.as_millis() as u64);
                    Duration::from_millis(capped)
                };

                let _ = provider; // silence unused warning (provider kept for
                                  // future logging / tracing)

                sleep_with_jitter(delay, policy.jitter);
            }
        }
    }
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    use httpmock::prelude::*;

    // ── classify_http_response tests ─────────────────────────────────────

    #[test]
    fn classify_200_is_ok() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/ok");
            then.status(200).body("ok");
        });

        let response = reqwest::blocking::get(server.url("/ok")).unwrap();
        assert!(classify_http_response("test-provider", response).is_ok());
        mock.assert();
    }

    #[test]
    fn classify_401_is_auth_error() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/unauth");
            then.status(401).body("unauthorized");
        });

        let response = reqwest::blocking::get(server.url("/unauth")).unwrap();
        let err = classify_http_response("openai", response).unwrap_err();
        assert!(matches!(err, PipelineError::Auth { .. }));
        assert!(err.to_string().contains("openai"));
        mock.assert();
    }

    #[test]
    fn classify_429_is_rate_limited_with_retry_after() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/ratelimit");
            then.status(429)
                .header("Retry-After", "42")
                .body("too many requests");
        });

        let response = reqwest::blocking::get(server.url("/ratelimit")).unwrap();
        let err = classify_http_response("openai", response).unwrap_err();
        match err {
            PipelineError::RateLimited {
                provider,
                retry_after_secs,
            } => {
                assert_eq!(provider, "openai");
                assert_eq!(retry_after_secs, Some(42));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
        mock.assert();
    }

    #[test]
    fn classify_429_no_header_is_rate_limited_none() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/ratelimit-no-header");
            then.status(429).body("too many requests");
        });

        let response = reqwest::blocking::get(server.url("/ratelimit-no-header")).unwrap();
        let err = classify_http_response("openai", response).unwrap_err();
        match err {
            PipelineError::RateLimited {
                retry_after_secs, ..
            } => {
                assert_eq!(retry_after_secs, None);
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
        mock.assert();
    }

    #[test]
    fn classify_400_context_length_is_context_length_exceeded() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/ctx-len");
            then.status(400)
                .body("maximum context length exceeded for this model");
        });

        let response = reqwest::blocking::get(server.url("/ctx-len")).unwrap();
        let err = classify_http_response("openai", response).unwrap_err();
        assert!(matches!(err, PipelineError::ContextLengthExceeded(_)));
        assert!(err.to_string().contains("maximum context length"));
        mock.assert();
    }

    #[test]
    fn classify_400_other_is_bad_request() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/bad-req");
            then.status(400).body("invalid model name");
        });

        let response = reqwest::blocking::get(server.url("/bad-req")).unwrap();
        let err = classify_http_response("openai", response).unwrap_err();
        assert!(matches!(err, PipelineError::BadRequest { .. }));
        assert!(err.to_string().contains("invalid model name"));
        mock.assert();
    }

    #[test]
    fn classify_500_is_provider_error() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/server-error");
            then.status(500).body("internal error");
        });

        let response = reqwest::blocking::get(server.url("/server-error")).unwrap();
        let err = classify_http_response("openai", response).unwrap_err();
        assert!(matches!(err, PipelineError::ProviderError { .. }));
        assert!(err.to_string().contains("HTTP 500"));
        mock.assert();
    }

    // ── embed_with_retry tests ───────────────────────────────────────────

    fn cancelled_flag() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn default_policy() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1), // keep tests fast
            max_delay: Duration::from_secs(1),
            jitter: false, // deterministic for tests
        }
    }

    #[test]
    fn retry_succeeds_on_first_attempt() {
        let policy = default_policy();
        let cancelled = cancelled_flag();

        let result = embed_with_retry(
            "openai",
            || Ok(vec![vec![1.0, 2.0, 3.0]]),
            &policy,
            &cancelled,
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), vec![vec![1.0, 2.0, 3.0]]);
    }

    #[test]
    fn retry_succeeds_after_rate_limit() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_secs(1),
            jitter: false,
        };
        let cancelled = cancelled_flag();

        let mut call_count = 0u32;
        let result = embed_with_retry(
            "openai",
            || {
                call_count += 1;
                if call_count == 1 {
                    Err(PipelineError::RateLimited {
                        provider: "openai".into(),
                        retry_after_secs: None,
                    })
                } else {
                    Ok(vec![vec![4.0, 5.0]])
                }
            },
            &policy,
            &cancelled,
        );

        assert!(result.is_ok());
        assert_eq!(call_count, 2);
        assert_eq!(result.unwrap(), vec![vec![4.0, 5.0]]);
    }

    #[test]
    fn retry_exhausts_attempts_on_5xx() {
        let policy = RetryPolicy {
            max_attempts: 2,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_secs(1),
            jitter: false,
        };
        let cancelled = cancelled_flag();

        let mut call_count = 0u32;
        let result = embed_with_retry(
            "openai",
            || {
                call_count += 1;
                Err(PipelineError::ProviderError {
                    provider: "openai".into(),
                    message: "timeout".into(),
                })
            },
            &policy,
            &cancelled,
        );

        assert!(result.is_err());
        assert_eq!(call_count, 2); // max_attempts = 2
        assert!(matches!(
            result.unwrap_err(),
            PipelineError::ProviderError { .. }
        ));
    }

    #[test]
    fn retry_stops_on_cancellation() {
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_secs(1),
            jitter: false,
        };
        let cancelled = AtomicBool::new(false);

        let mut call_count = 0u32;
        let result = embed_with_retry(
            "openai",
            || {
                call_count += 1;
                if call_count == 2 {
                    // simulate external cancellation
                    cancelled.store(true, Ordering::Release);
                }
                Err(PipelineError::ProviderError {
                    provider: "openai".into(),
                    message: "timeout".into(),
                })
            },
            &policy,
            &cancelled,
        );

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PipelineError::Cancelled));
        assert_eq!(call_count, 2);
    }

    #[test]
    fn retry_propagates_context_length() {
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_secs(1),
            jitter: false,
        };
        let cancelled = cancelled_flag();

        let mut call_count = 0u32;
        let result = embed_with_retry(
            "openai",
            || {
                call_count += 1;
                Err(PipelineError::ContextLengthExceeded(
                    "too many tokens".into(),
                ))
            },
            &policy,
            &cancelled,
        );

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PipelineError::ContextLengthExceeded(_)
        ));
        assert_eq!(call_count, 1); // not retried
    }

    #[test]
    fn retry_does_not_compound_on_429() {
        // When a 429 has Retry-After, the delay should be exactly that
        // value — not 429 + exponential backoff stacked on top.
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_secs(30), // would be high if compounded
            max_delay: Duration::from_secs(60),
            jitter: false,
        };
        let cancelled = cancelled_flag();

        let mut call_count = 0u32;
        // Track what delays were used (approximate)
        let start = std::time::Instant::now();
        let result = embed_with_retry(
            "openai",
            || {
                call_count += 1;
                if call_count == 1 {
                    Err(PipelineError::RateLimited {
                        provider: "openai".into(),
                        retry_after_secs: Some(1), // 1 second
                    })
                } else {
                    Ok(vec![vec![1.0]])
                }
            },
            &policy,
            &cancelled,
        );
        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert_eq!(call_count, 2);

        // 1 s Retry-After + jitter=false → elapsed ≈ 1 s.
        // If compounding were happening, elapsed would be >> 2 s
        // (base_delay=30s × backoff). 2 s upper bound is generous.
        assert!(
            elapsed < Duration::from_secs(2),
            "expected ~1 s delay, got {elapsed:?} — delay may be compounding"
        );
    }
}
