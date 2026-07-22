// Provider factory: route to Rust-native providers for openai/voyageai,
// falling back to PythonEmbedCallback when py_callback is provided for unknown providers.

use pyo3::prelude::*;

use crate::embed::openai::EmbedConfig;
use crate::embed::openai::OpenAiProvider;
use crate::embed::voyageai::VoyageAiProvider;
use crate::embed::EmbedBatchFn;
use crate::embed::PythonEmbedCallback;
use crate::error::PipelineError;

/// Create the appropriate EmbedBatchFn for the given provider configuration.
///
/// - "openai"  → `OpenAiProvider` (Rust-native)
/// - "voyageai" → `VoyageAiProvider` (Rust-native)
/// - Fallback → `PythonEmbedCallback` if `py_callback` is `Some`, otherwise error.
#[allow(dead_code)]
pub fn create_embed_fn(
    cfg: &EmbedConfig,
    py_callback: Option<Py<PyAny>>,
) -> Result<Box<dyn EmbedBatchFn>, PipelineError> {
    match cfg.provider.as_str() {
        "openai" => Ok(Box::new(OpenAiProvider::new(cfg)?)),
        "voyageai" => Ok(Box::new(VoyageAiProvider::new(
            cfg.api_key.clone(),
            cfg.model.clone(),
            cfg.base_url.clone(),
            cfg.output_dims,
            cfg.ssl_verify,
        )?)),
        _ => {
            if let Some(callable) = py_callback {
                Ok(Box::new(PythonEmbedCallback::new(callable)))
            } else {
                Err(PipelineError::BadRequest {
                    provider: cfg.provider.clone(),
                    message: format!(
                        "unsupported provider '{}': no native provider and no Python callback configured",
                        cfg.provider
                    ),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test 1: openai routing ───────────────────────────────────────────

    #[test]
    fn routes_openai_to_native_provider() {
        let cfg = EmbedConfig {
            provider: "openai".into(),
            model: "text-embedding-3-small".into(),
            api_key: "sk-test".into(),
            base_url: None,
            output_dims: None,
            matryoshka: false,
            api_version: None,
            ssl_verify: true,
            is_azure: None,
        };
        let result = create_embed_fn(&cfg, None);
        assert!(
            result.is_ok(),
            "openai provider must construct successfully"
        );
    }

    // ── Test 2: voyageai routing ─────────────────────────────────────────

    #[test]
    fn routes_voyageai_to_native_provider() {
        let cfg = EmbedConfig {
            provider: "voyageai".into(),
            model: "voyage-3".into(),
            api_key: "vp-test".into(),
            base_url: None,
            output_dims: None,
            matryoshka: false,
            api_version: None,
            ssl_verify: true,
            is_azure: None,
        };
        let result = create_embed_fn(&cfg, None);
        assert!(
            result.is_ok(),
            "voyageai provider must construct successfully"
        );
    }

    // ── Test 3: unknown provider with callback → fallback ────────────────

    #[test]
    fn unknown_provider_falls_back_to_python_callback() {
        let cfg = EmbedConfig {
            provider: "cohere".into(),
            model: "embed-english-v3".into(),
            api_key: "test-key".into(),
            base_url: None,
            output_dims: None,
            matryoshka: false,
            api_version: None,
            ssl_verify: true,
            is_azure: None,
        };
        pyo3::prepare_freethreaded_python();
        Python::with_gil(|py| {
            // A valid Python callable that just returns True
            let cb = py
                .eval_bound("lambda texts, provider, model: True", None, None)
                .unwrap()
                .unbind();
            let result = create_embed_fn(&cfg, Some(cb));
            assert!(
                result.is_ok(),
                "unknown provider with callback must fall back"
            );
        });
    }

    // ── Test 4: unknown provider without callback → error ────────────────

    #[test]
    fn unknown_provider_without_callback_is_error() {
        let cfg = EmbedConfig {
            provider: "cohere".into(),
            model: "embed-english-v3".into(),
            api_key: "test-key".into(),
            base_url: None,
            output_dims: None,
            matryoshka: false,
            api_version: None,
            ssl_verify: true,
            is_azure: None,
        };
        let result = create_embed_fn(&cfg, None);
        assert!(
            result.is_err(),
            "unknown provider without callback must error"
        );
    }
}
