use std::sync::atomic::AtomicBool;

use crate::embed::retry::{classify_http_status, embed_with_split, RetryPolicy};
use crate::embed::{BatchCallStats, EmbedBatchFn, EmbedBatchResult};
use crate::error::PipelineError;

/// Configuration for an OpenAI-compatible embedding provider.
#[derive(Clone)]
pub struct EmbedConfig {
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub output_dims: Option<usize>,
    pub matryoshka: bool,
    pub api_version: Option<String>,
    pub ssl_verify: bool,
    /// Explicitly force Azure mode (default: None = auto-detect from base_url).
    pub is_azure: Option<bool>,
}

impl std::fmt::Debug for EmbedConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbedConfig")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("api_key", &"[REDACTED]")
            .field("base_url", &self.base_url)
            .field("output_dims", &self.output_dims)
            .field("matryoshka", &self.matryoshka)
            .field("api_version", &self.api_version)
            .field("ssl_verify", &self.ssl_verify)
            .field("is_azure", &self.is_azure)
            .finish()
    }
}

impl Default for EmbedConfig {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            model: "text-embedding-3-small".into(),
            api_key: String::new(),
            base_url: None,
            output_dims: None,
            matryoshka: false,
            api_version: None,
            ssl_verify: true,
            is_azure: None,
        }
    }
}

/// OpenAI / Azure OpenAI embedding provider.
pub struct OpenAiProvider {
    client: reqwest::blocking::Client,
    api_key: String,
    model: String,
    base_url: String,
    output_dims: Option<usize>,
    matryoshka: bool,
    is_azure: bool,
    api_version: Option<String>,
}

impl OpenAiProvider {
    pub fn new(cfg: &EmbedConfig) -> Result<Self, PipelineError> {
        let default_base = "https://api.openai.com/v1".to_string();
        let raw_base = cfg.base_url.as_deref().unwrap_or(&default_base);
        let is_azure = cfg.is_azure.unwrap_or_else(|| {
            raw_base.contains("openai.azure.com") || raw_base.contains("azure.com")
        });
        let base_url = raw_base.strip_suffix('/').unwrap_or(raw_base).to_string();

        let mut builder =
            reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(120));
        if !cfg.ssl_verify {
            builder = builder.danger_accept_invalid_certs(true);
        }
        let client = builder.build().map_err(|e| PipelineError::ProviderError {
            provider: cfg.provider.clone(),
            message: format!("failed to build HTTP client: {e}"),
        })?;

        Ok(Self {
            client,
            api_key: cfg.api_key.clone(),
            model: cfg.model.clone(),
            base_url,
            output_dims: cfg.output_dims,
            matryoshka: cfg.matryoshka,
            is_azure,
            api_version: cfg.api_version.clone(),
        })
    }

    /// Build the HTTP request for a batch of texts.
    fn build_request(
        &self,
        texts: &[String],
    ) -> Result<reqwest::blocking::RequestBuilder, PipelineError> {
        let url = format!("{}/embeddings", self.base_url);

        #[derive(serde::Serialize)]
        struct EmbedRequest<'a> {
            model: &'a str,
            input: &'a [String],
            #[serde(skip_serializing_if = "Option::is_none")]
            dimensions: Option<usize>,
        }

        let dimensions = if self.matryoshka {
            self.output_dims
        } else {
            None
        };

        let body = EmbedRequest {
            model: &self.model,
            input: texts,
            dimensions,
        };

        let mut req = self.client.post(&url).json(&body);

        if self.is_azure {
            if let Some(ref ver) = self.api_version {
                req = req.query(&[("api-version", ver.as_str())]);
            }
            req = req.header("api-key", &self.api_key);
        } else {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }

        Ok(req)
    }

    /// Send a batch embedding request and return raw vectors.
    pub fn embed_batch_raw(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PipelineError> {
        let request = self.build_request(texts)?;
        let response = request.send().map_err(|e| PipelineError::ProviderError {
            provider: "openai".into(),
            message: format!("HTTP request failed: {e}"),
        })?;

        // If the response is not a success, classify and return the error.
        // Otherwise parse the body directly (no double-send).
        let status = response.status().as_u16();
        if !(200..=299).contains(&status) {
            let retry_after_secs = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            let body = response.text().unwrap_or_default();
            return Err(
                classify_http_status("openai", status, retry_after_secs, &body).unwrap_or_else(
                    || PipelineError::ProviderError {
                        provider: "openai".into(),
                        message: format!("HTTP {status}"),
                    },
                ),
            );
        }

        #[derive(serde::Deserialize)]
        struct EmbedResponseItem {
            index: usize,
            embedding: Vec<f32>,
        }

        #[derive(serde::Deserialize)]
        struct EmbedResponse {
            data: Vec<EmbedResponseItem>,
        }

        let body: EmbedResponse = response.json().map_err(|e| {
            PipelineError::ResponseFormat(format!("failed to parse embedding response: {e}"))
        })?;

        let mut items = body.data;
        items.sort_by_key(|item| item.index);

        let mut vectors: Vec<Vec<f32>> = items.into_iter().map(|item| item.embedding).collect();

        // Client-side truncation: if matryoshka=false and output_dims is set,
        // slice and L2-normalize.
        if !self.matryoshka {
            if let Some(target_dims) = self.output_dims {
                for v in &mut vectors {
                    if v.len() > target_dims {
                        v.truncate(target_dims);
                        l2_normalize(v);
                    }
                }
            }
        }

        Ok(vectors)
    }
}

/// L2-normalize a vector in-place.
fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = (v.iter().map(|x| x * x).sum::<f32>()).sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

impl EmbedBatchFn for OpenAiProvider {
    fn embed_batch(
        &self,
        texts: &[String],
        _provider: &str,
        _model: &str,
        _dims: usize,
    ) -> EmbedBatchResult {
        let cancelled = AtomicBool::new(false);
        let policy = RetryPolicy::default();
        let vectors = embed_with_split("openai", texts, &policy, &cancelled, |batch| {
            self.embed_batch_raw(batch)
        });
        EmbedBatchResult {
            vectors,
            stats: BatchCallStats {
                api_calls: 1,
                total_latency_ms: 0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn make_texts(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("text {}", i)).collect()
    }

    fn openai_cfg(server: &MockServer) -> EmbedConfig {
        EmbedConfig {
            provider: "openai".into(),
            model: "text-embedding-3-small".into(),
            api_key: "sk-test-key".into(),
            base_url: Some(server.url("")),
            output_dims: None,
            matryoshka: false,
            api_version: None,
            ssl_verify: true,
            is_azure: None,
        }
    }

    fn openai_mock(server: &MockServer, vectors: Vec<Vec<f32>>) -> httpmock::Mock<'_> {
        let data: Vec<serde_json::Value> = vectors
            .into_iter()
            .enumerate()
            .map(|(i, embedding)| {
                serde_json::json!({
                    "index": i,
                    "embedding": embedding,
                })
            })
            .collect();
        server.mock(|when, then| {
            when.method(POST)
                .path("/embeddings")
                .header("Authorization", "Bearer sk-test-key");
            then.status(200)
                .json_body(serde_json::json!({ "data": data }));
        })
    }

    fn embed_1536() -> Vec<f32> {
        vec![0.01; 1536]
    }

    // ── Test 1: returns correct count ────────────────────────────────────

    #[test]
    fn returns_correct_count() {
        let server = MockServer::start();
        let mock = openai_mock(&server, vec![embed_1536(), embed_1536(), embed_1536()]);
        let cfg = openai_cfg(&server);
        let provider = OpenAiProvider::new(&cfg).unwrap();

        let texts = make_texts(3);
        let result = provider.embed_batch(&texts, "openai", "text-embedding-3-small", 1536);
        assert_eq!(result.vectors.len(), 3);
        assert!(result.vectors.iter().all(|v| v.is_some()));
        mock.assert();
    }

    // ── Test 2: returns correct dims ─────────────────────────────────────

    #[test]
    fn returns_correct_dims() {
        let server = MockServer::start();
        let mock = openai_mock(&server, vec![embed_1536()]);
        let cfg = openai_cfg(&server);
        let provider = OpenAiProvider::new(&cfg).unwrap();

        let result = provider.embed_batch(&make_texts(1), "openai", "text-embedding-3-small", 1536);
        assert_eq!(result.vectors[0].as_ref().unwrap().len(), 1536);
        mock.assert();
    }

    // ── Test 3: sends dimensions param for matryoshka ────────────────────

    #[test]
    fn sends_dimensions_param_for_matryoshka() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/embeddings")
                .header("Authorization", "Bearer sk-test-key")
                .json_body_partial(r#"{"model":"text-embedding-3-small","dimensions":512}"#);
            then.status(200).json_body(serde_json::json!({
                "data": [{"index": 0, "embedding": vec![0.01_f32; 512]}]
            }));
        });

        let cfg = EmbedConfig {
            matryoshka: true,
            output_dims: Some(512),
            ..openai_cfg(&server)
        };
        let provider = OpenAiProvider::new(&cfg).unwrap();

        let result = provider.embed_batch(&make_texts(1), "openai", "text-embedding-3-small", 512);
        assert_eq!(result.vectors.len(), 1);
        assert_eq!(result.vectors[0].as_ref().unwrap().len(), 512);
        mock.assert();
    }

    // ── Test 4: skips dimensions for non-matryoshka (ada-002) ─────────────────

    #[test]
    fn skips_dimensions_for_non_matryoshka() {
        let server = MockServer::start();
        // Ada-002 doesn't support matryoshka, so dimensions must NOT be sent.
        // The mock catches any body containing "dimensions" as a failure.
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/embeddings")
                .header("Authorization", "Bearer sk-test-key")
                .matches(|req| {
                    // Verify body does NOT contain "dimensions"
                    let body_bytes = req.body.as_deref().unwrap_or(b"");
                    let body_str = String::from_utf8_lossy(body_bytes);
                    !body_str.contains("dimensions")
                });
            then.status(200).json_body(serde_json::json!({
                "data": [{"index": 0, "embedding": vec![0.01_f32; 1536]}]
            }));
        });

        let cfg = EmbedConfig {
            provider: "openai".into(),
            model: "text-embedding-ada-002".into(),
            api_key: "sk-test-key".into(),
            base_url: Some(server.url("")),
            output_dims: Some(1536),
            matryoshka: false,
            api_version: None,
            ssl_verify: true,
            is_azure: None,
        };
        let provider = OpenAiProvider::new(&cfg).unwrap();

        let result = provider.embed_batch(&make_texts(1), "openai", "text-embedding-ada-002", 1536);
        assert_eq!(result.vectors.len(), 1);
        assert!(result.vectors[0].is_some());
        mock.assert();
    }

    // ── Test 5: sorts by index ───────────────────────────────────────────

    #[test]
    fn sorts_by_index() {
        let server = MockServer::start();
        // Return items out of order
        let mock = server.mock(|when, then| {
            when.method(POST).path("/embeddings");
            then.status(200).json_body(serde_json::json!({
                "data": [
                    {"index": 2, "embedding": vec![0.1, 0.2, 0.3]},
                    {"index": 0, "embedding": vec![1.0, 2.0, 3.0]},
                    {"index": 1, "embedding": vec![4.0, 5.0, 6.0]},
                ]
            }));
        });

        let cfg = openai_cfg(&server);
        let provider = OpenAiProvider::new(&cfg).unwrap();

        let result = provider.embed_batch(&make_texts(3), "openai", "text-embedding-3-small", 3);
        assert_eq!(result.vectors.len(), 3);
        assert_eq!(result.vectors[0].as_ref().unwrap(), &[1.0, 2.0, 3.0]);
        assert_eq!(result.vectors[1].as_ref().unwrap(), &[4.0, 5.0, 6.0]);
        assert_eq!(result.vectors[2].as_ref().unwrap(), &[0.1, 0.2, 0.3]);
        mock.assert();
    }

    // ── Test 6: Azure uses api-key header ────────────────────────────────

    #[test]
    fn azure_uses_api_key_header() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/embeddings")
                .header("api-key", "azure-api-key-123");
            then.status(200).json_body(serde_json::json!({
                "data": [{"index": 0, "embedding": vec![0.1; 8]}]
            }));
        });

        let cfg = EmbedConfig {
            provider: "azure".into(),
            model: "text-embedding-3-small".into(),
            api_key: "azure-api-key-123".into(),
            base_url: Some(server.url("")),
            output_dims: None,
            matryoshka: false,
            api_version: Some("2024-02-01".into()),
            ssl_verify: true,
            is_azure: Some(true),
        };
        let provider = OpenAiProvider::new(&cfg).unwrap();

        let result = provider.embed_batch(&make_texts(1), "azure", "text-embedding-3-small", 8);
        assert_eq!(result.vectors.len(), 1);
        assert!(result.vectors[0].is_some());
        mock.assert();
    }

    // ── Test 7: Azure appends api-version ────────────────────────────────

    #[test]
    fn azure_appends_api_version() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/embeddings")
                .query_param("api-version", "2024-02-01");
            then.status(200).json_body(serde_json::json!({
                "data": [{"index": 0, "embedding": vec![0.1; 8]}]
            }));
        });

        let cfg = EmbedConfig {
            provider: "azure".into(),
            model: "text-embedding-3-small".into(),
            api_key: "some-key".into(),
            base_url: Some(server.url("")),
            output_dims: None,
            matryoshka: false,
            api_version: Some("2024-02-01".into()),
            ssl_verify: true,
            is_azure: Some(true),
        };
        let provider = OpenAiProvider::new(&cfg).unwrap();

        let result = provider.embed_batch(&make_texts(1), "azure", "text-embedding-3-small", 8);
        assert_eq!(result.vectors.len(), 1);
        assert!(result.vectors[0].is_some());
        mock.assert();
    }

    // ── Test 8: client-side truncation slices + normalizes ───────────────

    #[test]
    fn client_side_truncation_slices_and_normalizes() {
        let server = MockServer::start();
        // Return 1536-dim vectors. We'll request output_dims=3 with matryoshka=false,
        // so the provider truncates to 3 and L2-normalizes.
        let large_embedding: Vec<f32> = (0..1536).map(|i| (i + 1) as f32).collect();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/embeddings")
                // No "dimensions" in body for non-matryoshka
                .matches(|req| {
                    let body_bytes = req.body.as_deref().unwrap_or(b"");
                    let body_str = String::from_utf8_lossy(body_bytes);
                    !body_str.contains("dimensions")
                });
            then.status(200).json_body(serde_json::json!({
                "data": [{"index": 0, "embedding": large_embedding}]
            }));
        });

        let cfg = EmbedConfig {
            provider: "openai".into(),
            model: "text-embedding-3-small".into(),
            api_key: "sk-test-key".into(),
            base_url: Some(server.url("")),
            output_dims: Some(3),
            matryoshka: false,
            api_version: None,
            ssl_verify: true,
            is_azure: None,
        };
        let provider = OpenAiProvider::new(&cfg).unwrap();

        let result = provider.embed_batch(&make_texts(1), "openai", "text-embedding-3-small", 1536);
        assert_eq!(result.vectors.len(), 1);
        let v = result.vectors[0].as_ref().unwrap();
        assert_eq!(v.len(), 3, "should be truncated to 3 dims");
        // Verify L2 normalization: norm should be 1.0
        let norm: f32 = (v.iter().map(|x| x * x).sum::<f32>()).sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-6,
            "should be L2-normalized, got norm={norm}"
        );
        mock.assert();
    }

    // ── Test 9: context length exceeded splits the batch ────────────────

    #[test]
    fn context_length_splits_batch() {
        let server = MockServer::start();

        // Full 2-item batch → 400 context-length error
        let full_batch_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/embeddings")
                .header("Authorization", "Bearer sk-test-key")
                .matches(|req| {
                    let body_bytes = req.body.as_deref().unwrap_or(b"");
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body_bytes) {
                        v["input"].as_array().map(|a| a.len()) == Some(2)
                    } else {
                        false
                    }
                });
            then.status(400)
                .body("maximum context length exceeded for this model");
        });

        // Single-item batches → success
        let single_item_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/embeddings")
                .header("Authorization", "Bearer sk-test-key")
                .matches(|req| {
                    let body_bytes = req.body.as_deref().unwrap_or(b"");
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body_bytes) {
                        v["input"].as_array().map(|a| a.len()) == Some(1)
                    } else {
                        false
                    }
                });
            then.status(200).json_body(serde_json::json!({
                "data": [{"index": 0, "embedding": vec![0.1_f32; 8]}]
            }));
        });

        let cfg = openai_cfg(&server);
        let provider = OpenAiProvider::new(&cfg).unwrap();

        let texts = make_texts(2);
        let result = provider.embed_batch(&texts, "openai", "text-embedding-3-small", 8);

        assert_eq!(result.vectors.len(), 2);
        assert!(
            result.vectors[0].is_some(),
            "first vector should be Some after split"
        );
        assert!(
            result.vectors[1].is_some(),
            "second vector should be Some after split"
        );
        full_batch_mock.assert();
        single_item_mock.assert_hits(2);
    }
}
