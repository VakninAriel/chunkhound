pub mod callback;
pub mod token;

/// A batch embedding function — stateless, synchronous from Rust's perspective.
/// Each call is independent; concurrency is handled by the rayon caller.
#[allow(dead_code)]
pub trait EmbedBatchFn: Send + Sync {
    fn embed_batch(
        &self,
        texts: &[String],
        provider: &str,
        model: &str,
        dims: usize,
    ) -> EmbedBatchResult;
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct EmbedBatchResult {
    pub vectors: Vec<Option<Vec<f32>>>,
    pub stats: BatchCallStats,
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct BatchCallStats {
    pub api_calls: u32,
    pub total_latency_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockEmbedFn {
        vectors: Vec<Vec<f32>>,
        should_fail_indices: Vec<usize>,
    }

    impl EmbedBatchFn for MockEmbedFn {
        fn embed_batch(
            &self,
            texts: &[String],
            _provider: &str,
            _model: &str,
            _dims: usize,
        ) -> EmbedBatchResult {
            let mut result = Vec::with_capacity(texts.len());
            for i in 0..texts.len() {
                if self.should_fail_indices.contains(&i) {
                    result.push(None);
                } else if i < self.vectors.len() {
                    result.push(Some(self.vectors[i].clone()));
                } else {
                    result.push(Some(vec![1.0_f32; 8]));
                }
            }
            EmbedBatchResult {
                vectors: result,
                stats: BatchCallStats {
                    api_calls: 1,
                    total_latency_ms: 50,
                },
            }
        }
    }

    #[test]
    fn mock_embed_batch_returns_correct_count() {
        let mock = MockEmbedFn {
            vectors: vec![vec![1.0; 8], vec![2.0; 8], vec![3.0; 8]],
            should_fail_indices: vec![],
        };
        let texts: Vec<String> = (0..3).map(|i| format!("text {}", i)).collect();
        let result = mock.embed_batch(&texts, "openai", "model", 8);
        assert_eq!(result.vectors.len(), 3);
        assert!(result.vectors.iter().all(|v| v.is_some()));
    }

    #[test]
    fn mock_embed_batch_returns_correct_dims() {
        let mock = MockEmbedFn {
            vectors: vec![vec![0.1; 1536]],
            should_fail_indices: vec![],
        };
        let result = mock.embed_batch(&["text".into()], "openai", "text-embedding-3-small", 1536);
        assert_eq!(result.vectors[0].as_ref().unwrap().len(), 1536);
    }

    #[test]
    fn mock_embed_batch_partial_failure() {
        let mock = MockEmbedFn {
            vectors: vec![vec![1.0; 8], vec![2.0; 8], vec![3.0; 8]],
            should_fail_indices: vec![1],
        };
        let texts: Vec<String> = (0..3).map(|i| format!("text {}", i)).collect();
        let result = mock.embed_batch(&texts, "openai", "model", 8);
        assert!(result.vectors[0].is_some());
        assert!(result.vectors[1].is_none());
        assert!(result.vectors[2].is_some());
    }

    #[test]
    fn mock_embed_batch_all_failure() {
        let mock = MockEmbedFn {
            vectors: vec![],
            should_fail_indices: (0..5).collect(),
        };
        let texts: Vec<String> = (0..5).map(|i| format!("text {}", i)).collect();
        let result = mock.embed_batch(&texts, "openai", "model", 8);
        assert_eq!(result.vectors.len(), 5);
        assert!(result.vectors.iter().all(|v| v.is_none()));
    }

    #[test]
    fn mock_embed_batch_empty_input() {
        let mock = MockEmbedFn {
            vectors: vec![],
            should_fail_indices: vec![],
        };
        let result = mock.embed_batch(&[], "openai", "model", 8);
        assert!(result.vectors.is_empty());
    }

    #[test]
    fn mock_embed_batch_reports_stats() {
        let mock = MockEmbedFn {
            vectors: vec![vec![1.0; 8]],
            should_fail_indices: vec![],
        };
        let result = mock.embed_batch(&["text".into()], "openai", "model", 8);
        assert_eq!(result.stats.api_calls, 1);
        assert!(result.stats.total_latency_ms > 0);
    }

    #[test]
    fn embed_batch_fn_is_object_safe() {
        let mock = MockEmbedFn {
            vectors: vec![],
            should_fail_indices: vec![],
        };
        let _trait_obj: &dyn EmbedBatchFn = &mock;
    }
}
