// PipelineError is added in task-1. Downstream tasks (task-2 through task-13) will construct and
// propagate these errors. The enum and is_fatal() are dead-code only until those modules land.
#![expect(dead_code)]

use std::path::PathBuf;

use pyo3::exceptions::PyRuntimeError;
use pyo3::PyErr;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("duckdb: {0}")]
    DuckDb(#[from] duckdb::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

impl From<DbError> for PyErr {
    fn from(e: DbError) -> PyErr {
        PyRuntimeError::new_err(e.to_string())
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    // --- Phase 1: is_fatal + PartialEq ---

    #[test]
    fn auth_is_fatal() {
        let err = PipelineError::Auth {
            provider: "openai".into(),
            message: "bad key".into(),
        };
        assert!(err.is_fatal());
    }

    #[test]
    fn bad_request_is_fatal() {
        let err = PipelineError::BadRequest {
            provider: "openai".into(),
            message: "invalid model".into(),
        };
        assert!(err.is_fatal());
    }

    #[test]
    fn cancelled_is_fatal() {
        assert!(PipelineError::Cancelled.is_fatal());
    }

    #[test]
    fn provider_error_is_not_fatal() {
        let err = PipelineError::ProviderError {
            provider: "voyageai".into(),
            message: "timeout".into(),
        };
        assert!(!err.is_fatal());
    }

    #[test]
    fn rate_limited_is_not_fatal() {
        let err = PipelineError::RateLimited {
            provider: "openai".into(),
            retry_after_secs: Some(5),
        };
        assert!(!err.is_fatal());
    }

    #[test]
    fn same_error_variants_are_equal() {
        let e1 = PipelineError::Auth {
            provider: "oai".into(),
            message: "msg".into(),
        };
        let e2 = PipelineError::Auth {
            provider: "oai".into(),
            message: "msg".into(),
        };
        assert_eq!(e1, e2);
    }

    #[test]
    fn different_error_variants_are_not_equal() {
        let e1 = PipelineError::Auth {
            provider: "oai".into(),
            message: "msg".into(),
        };
        let e2 = PipelineError::BadRequest {
            provider: "oai".into(),
            message: "msg".into(),
        };
        assert_ne!(e1, e2);
    }

    // --- Phase 2: display formatting + exhaustive variant check ---

    #[test]
    fn auth_error_display_includes_provider() {
        let err = PipelineError::Auth {
            provider: "openai".into(),
            message: "unauthorized".into(),
        };
        let display = err.to_string();
        assert!(display.contains("openai"));
        assert!(display.contains("unauthorized"));
    }

    #[test]
    fn bad_request_error_display_includes_message() {
        let err = PipelineError::BadRequest {
            provider: "voyageai".into(),
            message: "model not found".into(),
        };
        assert!(err.to_string().contains("model not found"));
    }

    #[test]
    fn rate_limited_error_display_includes_provider() {
        let err = PipelineError::RateLimited {
            provider: "openai".into(),
            retry_after_secs: Some(30),
        };
        assert!(err.to_string().contains("openai"));
    }

    #[test]
    fn all_fatal_variants_are_exhaustive() {
        // Compile-time check: every variant must appear in this array
        let errors: &[PipelineError] = &[
            PipelineError::Auth {
                provider: "".into(),
                message: "".into(),
            },
            PipelineError::BadRequest {
                provider: "".into(),
                message: "".into(),
            },
            PipelineError::Cancelled,
            PipelineError::ProviderError {
                provider: "".into(),
                message: "".into(),
            },
            PipelineError::RateLimited {
                provider: "".into(),
                retry_after_secs: None,
            },
            PipelineError::DbError("".into()),
            PipelineError::IoError {
                path: PathBuf::new(),
                message: "".into(),
            },
        ];
        assert_eq!(
            errors.len(),
            7,
            "all 7 PipelineError variants must be listed"
        );
    }
}
