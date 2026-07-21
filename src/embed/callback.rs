use pyo3::prelude::*;
use pyo3::types::PyList;

use crate::error::PipelineError;

/// Extract vectors from a Python callback return value.
/// Handles: List[List[float]], List[None], mixed, malformed, wrong count.
pub fn extract_vectors_from_python(
    _py: Python<'_>,
    result: &Bound<'_, PyAny>,
    expected_len: usize,
) -> Vec<Option<Vec<f32>>> {
    let list: &Bound<'_, PyList> = match result.downcast::<PyList>() {
        Ok(l) => l,
        Err(_) => return vec![None; expected_len],
    };

    let mut vectors: Vec<Option<Vec<f32>>> = Vec::with_capacity(expected_len);
    for item in list.iter() {
        if item.is_none() {
            vectors.push(None);
        } else if let Ok(inner) = item.downcast::<PyList>() {
            let vec: Vec<f32> = inner
                .iter()
                .filter_map(|v| v.extract::<f32>().ok())
                .collect();
            if vec.is_empty() {
                vectors.push(None);
            } else {
                vectors.push(Some(vec));
            }
        } else {
            vectors.push(None);
        }
    }

    // Pad with None if Python returned fewer vectors than expected
    while vectors.len() < expected_len {
        vectors.push(None);
    }

    vectors
}

/// Classify a Python exception from the embed callback into a typed PipelineError.
/// Unknown exceptions default to ProviderError (non-fatal).
pub fn classify_python_embed_error(py: Python<'_>, err: &PyErr) -> PipelineError {
    let msg = err.to_string();
    let type_name = err.get_type_bound(py).name().map(|n| n.to_string()).unwrap_or_default();

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


