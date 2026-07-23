use std::sync::atomic::AtomicBool;

use crate::embed::retry::{classify_http_status, embed_with_split, RetryPolicy};
use crate::embed::{BatchCallStats, EmbedBatchFn, EmbedBatchResult};
use crate::error::PipelineError;

/// Voyage AI embedding provider.
pub struct VoyageAiProvider {
    client: reqwest::blocking::Client,
    api_key: String,
    model: String,
    base_url: String,
    output_dims: Option<usize>,
}

impl VoyageAiProvider {
    pub fn new(
        api_key: String,
        model: String,
        base_url: Option<String>,
        output_dims: Option<usize>,
        ssl_verify: bool,
    ) -> Result<Self, PipelineError> {
        let default_base = "https://api.voyageai.com/v1".to_string();
        let raw_base = base_url.as_deref().unwrap_or(&default_base);
        let base_url = raw_base.strip_suffix('/').unwrap_or(raw_base).to_string();

        let mut builder =
            reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(120));
        if !ssl_verify {
            builder = builder.danger_accept_invalid_certs(true);
        }
        let client = builder.build().map_err(|e| PipelineError::ProviderError {
            provider: "voyageai".into(),
            message: format!("failed to build HTTP client: {e}"),
        })?;

        Ok(Self {
            client,
            api_key,
            model,
            base_url,
            output_dims,
        })
    }

    /// Send a batch embedding request and return raw vectors.
    pub fn embed_batch_raw(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PipelineError> {
        let url = format!("{}/embeddings", self.base_url);

        #[derive(serde::Serialize)]
        struct EmbedRequest<'a> {
            model: &'a str,
            input: &'a [String],
            input_type: &'a str,
            truncation: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            output_dimension: Option<usize>,
        }

        let body = EmbedRequest {
            model: &self.model,
            input: texts,
            input_type: "document",
            truncation: true,
            output_dimension: self.output_dims,
        };

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .map_err(|e| PipelineError::ProviderError {
                provider: "voyageai".into(),
                message: format!("HTTP request failed: {e}"),
            })?;

        let status = response.status().as_u16();
        if !(200..=299).contains(&status) {
            let retry_after_secs = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            let body = response.text().unwrap_or_default();
            return Err(
                classify_http_status("voyageai", status, retry_after_secs, &body).unwrap_or_else(
                    || PipelineError::ProviderError {
                        provider: "voyageai".into(),
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

        let vectors: Vec<Vec<f32>> = items.into_iter().map(|item| item.embedding).collect();

        if vectors.len() != texts.len() {
            return Err(PipelineError::ProviderError {
                provider: "voyageai".into(),
                message: format!("expected {} embeddings, got {}", texts.len(), vectors.len()),
            });
        }

        Ok(vectors)
    }
}

impl EmbedBatchFn for VoyageAiProvider {
    fn embed_batch(
        &self,
        texts: &[String],
        _provider: &str,
        _model: &str,
        _dims: usize,
    ) -> EmbedBatchResult {
        let cancelled = AtomicBool::new(false);
        let policy = RetryPolicy::default();
        let vectors = embed_with_split("voyageai", texts, &policy, &cancelled, |batch| {
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

    fn voyageai_cfg(server: &MockServer) -> VoyageAiProvider {
        VoyageAiProvider::new(
            "vp-test-key".into(),
            "voyage-3".into(),
            Some(server.url("")),
            None,
            true,
        )
        .unwrap()
    }

    fn embed_1024() -> Vec<f32> {
        vec![0.01; 1024]
    }

    fn voyageai_mock(server: &MockServer, vectors: Vec<Vec<f32>>) -> httpmock::Mock<'_> {
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
                .header("Authorization", "Bearer vp-test-key");
            then.status(200)
                .json_body(serde_json::json!({ "data": data }));
        })
    }

    // ── Test 1: returns correct count ────────────────────────────────────

    #[test]
    fn returns_correct_count() {
        let server = MockServer::start();
        let mock = voyageai_mock(&server, vec![embed_1024(), embed_1024(), embed_1024()]);
        let provider = voyageai_cfg(&server);

        let texts = make_texts(3);
        let result = provider.embed_batch(&texts, "voyageai", "voyage-3", 1024);
        assert_eq!(result.vectors.len(), 3);
        assert!(result.vectors.iter().all(|v| v.is_some()));
        mock.assert();
    }

    // ── Test 2: returns correct dims (1024) ──────────────────────────────

    #[test]
    fn returns_correct_dims() {
        let server = MockServer::start();
        let mock = voyageai_mock(&server, vec![embed_1024()]);
        let provider = voyageai_cfg(&server);

        let result = provider.embed_batch(&make_texts(1), "voyageai", "voyage-3", 1024);
        assert_eq!(result.vectors[0].as_ref().unwrap().len(), 1024);
        mock.assert();
    }

    // ── Test 3: sends output_dimension param ─────────────────────────────

    #[test]
    fn sends_output_dimension_param() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/embeddings")
                .header("Authorization", "Bearer vp-test-key")
                .json_body_partial(r#"{"output_dimension":256}"#);
            then.status(200).json_body(serde_json::json!({
                "data": [{"index": 0, "embedding": vec![0.01_f32; 256]}]
            }));
        });

        let provider = VoyageAiProvider::new(
            "vp-test-key".into(),
            "voyage-3".into(),
            Some(server.url("")),
            Some(256),
            true,
        )
        .unwrap();

        let result = provider.embed_batch(&make_texts(1), "voyageai", "voyage-3", 256);
        assert_eq!(result.vectors.len(), 1);
        assert_eq!(result.vectors[0].as_ref().unwrap().len(), 256);
        mock.assert();
    }

    // ── Test 4: always sends truncation=true ─────────────────────────────

    #[test]
    fn always_sends_truncation_true() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/embeddings")
                .header("Authorization", "Bearer vp-test-key")
                .json_body_partial(r#"{"truncation":true}"#);
            then.status(200).json_body(serde_json::json!({
                "data": [{"index": 0, "embedding": embed_1024()}]
            }));
        });

        let provider = voyageai_cfg(&server);

        let result = provider.embed_batch(&make_texts(1), "voyageai", "voyage-3", 1024);
        assert_eq!(result.vectors.len(), 1);
        assert!(result.vectors[0].is_some());
        mock.assert();
    }

    // ── Test 5: always sends input_type="document" ───────────────────────

    #[test]
    fn always_sends_input_type_document() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/embeddings")
                .header("Authorization", "Bearer vp-test-key")
                .json_body_partial(r#"{"input_type":"document"}"#);
            then.status(200).json_body(serde_json::json!({
                "data": [{"index": 0, "embedding": embed_1024()}]
            }));
        });

        let provider = voyageai_cfg(&server);

        let result = provider.embed_batch(&make_texts(1), "voyageai", "voyage-3", 1024);
        assert_eq!(result.vectors.len(), 1);
        assert!(result.vectors[0].is_some());
        mock.assert();
    }

    // ── Test 6: sorts by index ───────────────────────────────────────────

    #[test]
    fn sorts_by_index() {
        let server = MockServer::start();
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

        let provider = voyageai_cfg(&server);

        let result = provider.embed_batch(&make_texts(3), "voyageai", "voyage-3", 3);
        assert_eq!(result.vectors.len(), 3);
        assert_eq!(result.vectors[0].as_ref().unwrap(), &[1.0, 2.0, 3.0]);
        assert_eq!(result.vectors[1].as_ref().unwrap(), &[4.0, 5.0, 6.0]);
        assert_eq!(result.vectors[2].as_ref().unwrap(), &[0.1, 0.2, 0.3]);
        mock.assert();
    }

    // ── Test 7: context length exceeded splits batch ─────────────────────

    #[test]
    fn context_length_splits_batch() {
        let server = MockServer::start();

        // Full 2-item batch → 400 context-length error
        let full_batch_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/embeddings")
                .header("Authorization", "Bearer vp-test-key")
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
                .header("Authorization", "Bearer vp-test-key")
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

        let provider = voyageai_cfg(&server);

        let texts = make_texts(2);
        let result = provider.embed_batch(&texts, "voyageai", "voyage-3", 8);

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
