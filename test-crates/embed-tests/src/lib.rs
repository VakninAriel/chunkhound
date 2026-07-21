//! Integration tests for classify_python_embed_error.
//!
//! This crate is separate from chunkhound_native because that crate uses
//! pyo3's `extension-module` feature (for the cdylib target), which is
//! mutually exclusive with `auto-initialize` on Linux. The `extension-module`
//! feature suppresses the libpython link directive, making Python::with_gil
//! impossible from an inline test.
//!
//! By compiling classify_python_embed_error and PipelineError in a standalone
//! crate with `auto-initialize`, the test binary links against libpython and
//! can embed the interpreter.

use pyo3::prelude::*;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Duplicate of crate::error::PipelineError — must stay in sync with
// src/error.rs in chunkhound_native. This duplication is unavoidable because
// the cdylib crate uses extension-module while this test crate must use
// auto-initialize, and cargo unifies pyo3 features across dependency kinds.
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PipelineError {
    #[error("authentication failed for {provider}: {message}")]
    Auth { provider: String, message: String },

    #[error("bad request for {provider}: {message}")]
    BadRequest { provider: String, message: String },

    #[error("pipeline cancelled")]
    Cancelled,

    #[error("provider error for {provider}: {message}")]
    ProviderError { provider: String, message: String },

    #[error("rate limited by {provider}")]
    RateLimited {
        provider: String,
        retry_after_secs: Option<u64>,
    },

    #[error("database error: {0}")]
    DbError(String),

    #[error("I/O error at {path}: {message}")]
    IoError { path: PathBuf, message: String },
}

impl PipelineError {
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Auth { .. } | Self::BadRequest { .. } | Self::Cancelled
        )
    }
}

// ---------------------------------------------------------------------------
// Exact duplicate of src/embed/callback.rs:classify_python_embed_error
// ---------------------------------------------------------------------------

/// Classify a Python exception from the embed callback into a typed PipelineError.
/// Unknown exceptions default to ProviderError (non-fatal).
pub fn classify_python_embed_error(py: Python<'_>, err: &PyErr) -> PipelineError {
    let msg = err.to_string();
    let type_name = err
        .get_type_bound(py)
        .name()
        .map(|n| n.to_string())
        .unwrap_or_default();

    match type_name.as_str() {
        "AuthenticationError" => PipelineError::Auth {
            provider: "unknown".into(),
            message: msg,
        },
        "RateLimitError" => PipelineError::RateLimited {
            provider: "unknown".into(),
            retry_after_secs: None,
        },
        "BadRequestError" => {
            if msg.contains("context length") || msg.contains("maximum context length") {
                PipelineError::ProviderError {
                    provider: "unknown".into(),
                    message: msg,
                }
            } else {
                PipelineError::BadRequest {
                    provider: "unknown".into(),
                    message: msg,
                }
            }
        }
        "APITimeoutError" | "APIConnectionError" | "Timeout" | "ConnectionError" => {
            PipelineError::ProviderError {
                provider: "unknown".into(),
                message: msg,
            }
        }
        _ => PipelineError::ProviderError {
            provider: "unknown".into(),
            message: msg,
        },
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_any_python_error_as_non_fatal_by_default() {
        Python::with_gil(|py| {
            let err = py.eval_bound("1/0", None, None).unwrap_err(); // ZeroDivisionError
            let classified = classify_python_embed_error(py, &err);
            assert!(
                !classified.is_fatal(),
                "unknown Python exceptions default to non-fatal ProviderError"
            );
        });
    }

    #[test]
    fn classify_value_error_as_provider_error() {
        Python::with_gil(|py| {
            let err = py
                .eval_bound("int('not-a-number')", None, None)
                .unwrap_err();
            let classified = classify_python_embed_error(py, &err);
            assert!(!classified.is_fatal());
            assert!(matches!(classified, PipelineError::ProviderError { .. }));
        });
    }

    #[test]
    fn classify_preserves_error_message() {
        Python::with_gil(|py| {
            let err = PyErr::new::<pyo3::exceptions::PyValueError, _>("test message 123");
            let classified = classify_python_embed_error(py, &err);
            let display = classified.to_string();
            assert!(
                display.contains("test message 123"),
                "error message should be preserved: {}",
                display
            );
        });
    }
}
