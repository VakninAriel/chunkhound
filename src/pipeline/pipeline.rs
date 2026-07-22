// Stub IndexingPipeline — wires bloom + token + EmbedBatchFn into a compilable,
// testable pipeline that exercises the embed path with in-memory test data.
// Real file scanning and DB writing will be integrated when PR #380 lands.

use std::path::PathBuf;

use crate::bloom::{bloom_key, load_or_rebuild_bloom};
use crate::embed::token::{estimate_tokens, BatchBuilder, BatchChunk, BatchConfig};
use crate::embed::EmbedBatchFn;
use crate::error::PipelineError;

/// Configuration for the indexing pipeline.
#[allow(dead_code)]
pub struct PipelineConfig {
    pub db_path: PathBuf,
    pub embed_batch_callback: Box<dyn EmbedBatchFn>,
    pub provider: String,
    pub model: String,
    pub output_dims: usize,
    pub max_chunks_per_batch: usize,
    pub incremental: bool,
}

impl std::fmt::Debug for PipelineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineConfig")
            .field("db_path", &self.db_path)
            .field("embed_batch_callback", &"<dyn EmbedBatchFn>")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("output_dims", &self.output_dims)
            .field("max_chunks_per_batch", &self.max_chunks_per_batch)
            .field("incremental", &self.incremental)
            .finish()
    }
}

/// Statistics collected during a pipeline run.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct PipelineStats {
    pub chunks_processed: u64,
    pub chunks_skipped: u64,
    pub batches_sent: u64,
    pub embeddings_sent: u64,
    pub chunks_failed: u64,
}

/// Stub indexing pipeline — exercises bloom → token → EmbedBatchFn wiring.
#[allow(dead_code)]
pub struct IndexingPipeline;

impl IndexingPipeline {
    /// Run the pipeline on a set of test chunks.
    ///
    /// In stub mode this processes in-memory chunks; the real implementation
    /// (PR #380) will scan files from disk and emit results to the DB.
    #[allow(dead_code)]
    pub fn run(
        config: PipelineConfig,
        chunks: &[BatchChunk],
    ) -> Result<PipelineStats, PipelineError> {
        // ── Bloom initialisation ──
        let bloom = load_or_rebuild_bloom(&config.db_path, &config.provider, &config.model)?;
        // In the real pipeline (PR #380), populate_bloom_from_db() would be called here
        // with a DuckDB connection to fill the bloom from existing embeddings.
        // In stub mode, the bloom starts empty and is not persisted after the run.

        // ── Batch builder ──
        let batch_config = BatchConfig {
            max_chunks_per_batch: config.max_chunks_per_batch,
            max_tokens_per_chunk: 8192,
            batch_token_budget: None,
        };
        let max_tokens_per_chunk = batch_config.max_tokens_per_chunk;
        let mut builder = BatchBuilder::new(batch_config);
        let mut stats = PipelineStats::default();

        for chunk in chunks {
            stats.chunks_processed += 1;

            // 1. Skip oversized chunks
            if estimate_tokens(&chunk.text) > max_tokens_per_chunk {
                log::warn!(
                    "Skipping oversized chunk: est. {} tokens > {} max",
                    estimate_tokens(&chunk.text),
                    max_tokens_per_chunk
                );
                stats.chunks_skipped += 1;
                continue;
            }

            // 2. Bloom check
            let key = bloom_key(
                &chunk.content_hash,
                &config.provider,
                &config.model,
                config.output_dims,
            );
            if bloom.contains(&key) {
                stats.chunks_skipped += 1;
                continue;
            }

            // 3. Add to batch builder
            let batch_chunk = BatchChunk {
                text: chunk.text.clone(),
                content_hash: chunk.content_hash.clone(),
            };
            if let Some(flushed) = builder.push(batch_chunk) {
                dispatch_batch(
                    &flushed,
                    config.embed_batch_callback.as_ref(),
                    &config.provider,
                    &config.model,
                    config.output_dims,
                    &mut stats,
                );
            }
        }

        // Flush remaining
        if let Some(remaining) = builder.flush() {
            dispatch_batch(
                &remaining,
                config.embed_batch_callback.as_ref(),
                &config.provider,
                &config.model,
                config.output_dims,
                &mut stats,
            );
        }

        Ok(stats)
    }
}

/// Dispatch a single batch to the embedding callback and record stats.
#[allow(dead_code)]
fn dispatch_batch(
    chunks: &[BatchChunk],
    embed_fn: &dyn EmbedBatchFn,
    provider: &str,
    model: &str,
    dims: usize,
    stats: &mut PipelineStats,
) {
    let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
    let result = embed_fn.embed_batch(&texts, provider, model, dims);

    stats.batches_sent += 1;
    stats.embeddings_sent += result.vectors.iter().filter(|v| v.is_some()).count() as u64;
    stats.chunks_failed += result.vectors.iter().filter(|v| v.is_none()).count() as u64;

    // In the real pipeline, vectors would be scattered back to chunks and
    // persisted to the DB. Also, successfully embedded chunks would be
    // inserted into the bloom filter here.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::token::BatchChunk;
    use crate::embed::{BatchCallStats, EmbedBatchResult};

    /// A mock embed function that always returns fixed-dimension vectors.
    struct TestEmbedFn {
        dims: usize,
        fail_indices: Vec<usize>,
    }

    impl EmbedBatchFn for TestEmbedFn {
        fn embed_batch(
            &self,
            texts: &[String],
            _provider: &str,
            _model: &str,
            _dims: usize,
        ) -> EmbedBatchResult {
            let vectors: Vec<Option<Vec<f32>>> = texts
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    if self.fail_indices.contains(&i) {
                        None
                    } else {
                        Some(vec![1.0_f32; self.dims])
                    }
                })
                .collect();
            EmbedBatchResult {
                vectors,
                stats: BatchCallStats {
                    api_calls: 1,
                    total_latency_ms: 10,
                },
            }
        }
    }

    fn make_chunk(hash: &str, text: &str) -> BatchChunk {
        BatchChunk {
            content_hash: hash.to_string(),
            text: text.to_string(),
        }
    }

    // ── Pipeline integration tests ──

    #[test]
    fn stub_pipeline_processes_all_chunks() {
        let temp_dir = tempfile::tempdir().unwrap();
        let embed_fn = TestEmbedFn {
            dims: 8,
            fail_indices: vec![],
        };
        let config = PipelineConfig {
            db_path: temp_dir.path().to_path_buf(),
            embed_batch_callback: Box::new(embed_fn),
            provider: "test".into(),
            model: "test-model".into(),
            output_dims: 8,
            max_chunks_per_batch: 5,
            incremental: false,
        };

        let chunks: Vec<BatchChunk> = (0..10)
            .map(|i| make_chunk(&format!("hash{}", i), &format!("chunk text {}", i)))
            .collect();

        let stats = IndexingPipeline::run(config, &chunks).unwrap();
        assert_eq!(stats.chunks_processed, 10);
        assert_eq!(stats.chunks_skipped, 0);
        assert!(stats.batches_sent >= 2); // 10 chunks, max 5 per batch → at least 2
        assert_eq!(stats.chunks_failed, 0);
    }

    #[test]
    fn stub_pipeline_bloom_dedup_skips_duplicates() {
        let temp_dir = tempfile::tempdir().unwrap();
        let embed_fn = TestEmbedFn {
            dims: 8,
            fail_indices: vec![],
        };
        let config = PipelineConfig {
            db_path: temp_dir.path().to_path_buf(),
            embed_batch_callback: Box::new(embed_fn),
            provider: "test".into(),
            model: "test-model".into(),
            output_dims: 8,
            max_chunks_per_batch: 5,
            incremental: false,
        };

        // First pass: 3 unique chunks
        let chunks1: Vec<BatchChunk> = vec![
            make_chunk("h1", "text one"),
            make_chunk("h2", "text two"),
            make_chunk("h3", "text three"),
        ];
        let stats1 = IndexingPipeline::run(config, &chunks1).unwrap();
        assert_eq!(stats1.chunks_processed, 3);
        assert_eq!(stats1.chunks_skipped, 0);

        // Second pass: same 3 chunks (new pipeline, fresh bloom → no dedup in stub)
        // In real pipeline, bloom would persist and skip them. Stub bloom is fresh each run.
        let embed_fn2 = TestEmbedFn {
            dims: 8,
            fail_indices: vec![],
        };
        let config2 = PipelineConfig {
            db_path: temp_dir.path().to_path_buf(),
            embed_batch_callback: Box::new(embed_fn2),
            provider: "test".into(),
            model: "test-model".into(),
            output_dims: 8,
            max_chunks_per_batch: 5,
            incremental: false,
        };
        let stats2 = IndexingPipeline::run(config2, &chunks1).unwrap();
        assert_eq!(stats2.chunks_processed, 3);
        // Bloom is fresh each run in stub mode → all get processed
        assert_eq!(stats2.chunks_skipped, 0);
    }

    #[test]
    fn stub_pipeline_skips_oversized_chunks() {
        let temp_dir = tempfile::tempdir().unwrap();
        let embed_fn = TestEmbedFn {
            dims: 8,
            fail_indices: vec![],
        };
        let config = PipelineConfig {
            db_path: temp_dir.path().to_path_buf(),
            embed_batch_callback: Box::new(embed_fn),
            provider: "test".into(),
            model: "test-model".into(),
            output_dims: 8,
            max_chunks_per_batch: 5,
            incremental: false,
        };

        let oversized_text = "x".repeat(30000); // 30000/3 = 10000 tokens > 8192 max
        let chunks: Vec<BatchChunk> = vec![
            make_chunk("h1", "normal chunk"),
            make_chunk("h2", &oversized_text),
            make_chunk("h3", "another normal"),
        ];

        let stats = IndexingPipeline::run(config, &chunks).unwrap();
        assert_eq!(stats.chunks_processed, 3);
        assert_eq!(stats.chunks_skipped, 1); // oversized h2 skipped
        assert_eq!(stats.chunks_failed, 0);
    }

    #[test]
    fn stub_pipeline_handles_partial_failures() {
        let temp_dir = tempfile::tempdir().unwrap();
        let embed_fn = TestEmbedFn {
            dims: 8,
            fail_indices: vec![1], // second chunk in the first batch fails
        };
        let config = PipelineConfig {
            db_path: temp_dir.path().to_path_buf(),
            embed_batch_callback: Box::new(embed_fn),
            provider: "test".into(),
            model: "test-model".into(),
            output_dims: 8,
            max_chunks_per_batch: 3,
            incremental: false,
        };

        let chunks: Vec<BatchChunk> = (0..3)
            .map(|i| make_chunk(&format!("hash{}", i), &format!("text {}", i)))
            .collect();

        let stats = IndexingPipeline::run(config, &chunks).unwrap();
        assert_eq!(stats.chunks_processed, 3);
        assert_eq!(stats.chunks_skipped, 0);
        assert_eq!(stats.chunks_failed, 1);
        assert_eq!(stats.embeddings_sent, 2);
    }

    #[test]
    fn stub_pipeline_empty_chunks_produces_zero_batches() {
        let temp_dir = tempfile::tempdir().unwrap();
        let embed_fn = TestEmbedFn {
            dims: 8,
            fail_indices: vec![],
        };
        let config = PipelineConfig {
            db_path: temp_dir.path().to_path_buf(),
            embed_batch_callback: Box::new(embed_fn),
            provider: "test".into(),
            model: "test-model".into(),
            output_dims: 8,
            max_chunks_per_batch: 5,
            incremental: false,
        };

        let stats = IndexingPipeline::run(config, &[]).unwrap();
        assert_eq!(stats.chunks_processed, 0);
        assert_eq!(stats.chunks_skipped, 0);
        assert_eq!(stats.batches_sent, 0);
    }

    #[test]
    fn stub_pipeline_single_chunk_single_batch() {
        let temp_dir = tempfile::tempdir().unwrap();
        let embed_fn = TestEmbedFn {
            dims: 1536,
            fail_indices: vec![],
        };
        let config = PipelineConfig {
            db_path: temp_dir.path().to_path_buf(),
            embed_batch_callback: Box::new(embed_fn),
            provider: "openai".into(),
            model: "text-embedding-3-small".into(),
            output_dims: 1536,
            max_chunks_per_batch: 10,
            incremental: false,
        };

        let chunks = vec![make_chunk("single", "one chunk")];
        let stats = IndexingPipeline::run(config, &chunks).unwrap();
        assert_eq!(stats.batches_sent, 1);
        assert_eq!(stats.embeddings_sent, 1);
        assert_eq!(stats.chunks_failed, 0);
    }

    #[test]
    fn stub_pipeline_stats_are_correct_for_mixed_scenario() {
        let temp_dir = tempfile::tempdir().unwrap();
        // Fail every 3rd chunk (global indices). Use single batch so local=global.
        let embed_fn = TestEmbedFn {
            dims: 16,
            fail_indices: vec![2, 5, 8],
        };
        let config = PipelineConfig {
            db_path: temp_dir.path().to_path_buf(),
            embed_batch_callback: Box::new(embed_fn),
            provider: "test".into(),
            model: "test-model".into(),
            output_dims: 16,
            max_chunks_per_batch: 20,
            incremental: false,
        };

        let chunks: Vec<BatchChunk> = (0..9)
            .map(|i| make_chunk(&format!("hash{}", i), &format!("text {}", i)))
            .collect();

        let stats = IndexingPipeline::run(config, &chunks).unwrap();
        assert_eq!(stats.chunks_processed, 9);
        assert_eq!(stats.chunks_skipped, 0);
        assert_eq!(stats.chunks_failed, 3);
        assert_eq!(stats.embeddings_sent, 6);
        assert_eq!(stats.batches_sent, 1);
    }

    #[test]
    fn pipeline_config_is_debug() {
        let embed_fn = TestEmbedFn {
            dims: 8,
            fail_indices: vec![],
        };
        let config = PipelineConfig {
            db_path: PathBuf::from("/tmp/test"),
            embed_batch_callback: Box::new(embed_fn),
            provider: "p".into(),
            model: "m".into(),
            output_dims: 8,
            max_chunks_per_batch: 10,
            incremental: true,
        };
        let debug = format!("{:?}", config);
        assert!(debug.contains("p"));
        assert!(debug.contains("m"));
        assert!(debug.contains("8"));
    }

    // ── Contract: Bloom Pipeline Integration ──

    /// RED: Second run with same content produces fewer embed callbacks than first.
    /// GREEN: Pre-populated bloom causes pipeline to skip matching chunks.
    ///
    /// The stub pipeline does not persist bloom internally (that happens in PR #380),
    /// so this test manually persists a pre-populated bloom from the bloom module and
    /// verifies the pipeline's load_or_rebuild_bloom path reads it back.
    #[test]
    fn contract_bloom_prepopulation_causes_pipeline_skips() {
        let temp_dir = tempfile::tempdir().unwrap();
        let bloom_path = temp_dir.path().join("embeddings.bloom");
        let meta_path = temp_dir.path().join("embeddings.bloom.meta");

        // Pre-populate bloom with keys for hash0 and hash2 (not hash1)
        let mut bloom = crate::bloom::AtomicBloomFilter::with_false_pos(0.01, 10_000);
        bloom.insert(&bloom_key("hash0", "test", "test-model", 8));
        bloom.insert(&bloom_key("hash2", "test", "test-model", 8));
        crate::bloom::persist_bloom(&bloom, &bloom_path).unwrap();
        crate::bloom::persist_meta(
            &crate::bloom::BloomMeta {
                provider: "test".into(),
                model: "test-model".into(),
            },
            &meta_path,
        )
        .unwrap();

        let embed_fn = TestEmbedFn {
            dims: 8,
            fail_indices: vec![],
        };
        let config = PipelineConfig {
            db_path: temp_dir.path().to_path_buf(),
            embed_batch_callback: Box::new(embed_fn),
            provider: "test".into(),
            model: "test-model".into(),
            output_dims: 8,
            max_chunks_per_batch: 10,
            incremental: false,
        };

        let chunks: Vec<BatchChunk> = vec![
            make_chunk("hash0", "text 0"),
            make_chunk("hash1", "text 1"),
            make_chunk("hash2", "text 2"),
        ];

        let stats = IndexingPipeline::run(config, &chunks).unwrap();
        assert_eq!(stats.chunks_processed, 3);
        assert_eq!(
            stats.chunks_skipped, 2,
            "hash0 and hash2 must be skipped via bloom hit"
        );
        assert_eq!(
            stats.batches_sent, 1,
            "only hash1 should be sent for embedding"
        );
        assert_eq!(stats.embeddings_sent, 1);
        assert_eq!(stats.chunks_failed, 0);
    }

    /// RED: Bloom survives pipeline close + reopen.
    /// GREEN: Persisted bloom + meta files survive roundtrip; load_or_rebuild_bloom
    /// recovers previously inserted keys rather than creating a fresh filter.
    #[test]
    fn contract_bloom_persists_across_restarts() {
        let temp_dir = tempfile::tempdir().unwrap();
        let bloom_path = temp_dir.path().join("embeddings.bloom");
        let meta_path = temp_dir.path().join("embeddings.bloom.meta");

        // First "run": create and persist bloom
        let mut bloom = crate::bloom::AtomicBloomFilter::with_false_pos(0.01, 10_000);
        let key = bloom_key("abc123", "openai", "text-embedding-3-small", 1536);
        bloom.insert(&key);
        bloom.insert(&bloom_key(
            "xyz789",
            "openai",
            "text-embedding-3-small",
            1536,
        ));
        crate::bloom::persist_bloom(&bloom, &bloom_path).unwrap();
        crate::bloom::persist_meta(
            &crate::bloom::BloomMeta {
                provider: "openai".into(),
                model: "text-embedding-3-small".into(),
            },
            &meta_path,
        )
        .unwrap();

        // Verify bloom files exist on disk
        assert!(
            bloom_path.exists(),
            "Bloom file must persist to disk at {:?}",
            bloom_path
        );
        assert!(
            meta_path.exists(),
            "Bloom meta must persist to disk at {:?}",
            meta_path
        );

        // Reload bloom directly — verify key recovery
        let loaded = crate::bloom::load_bloom_from_disk(&bloom_path)
            .unwrap()
            .expect("must load persisted bloom");
        assert!(
            loaded.contains(&key),
            "loaded bloom must contain previously-inserted keys"
        );
        assert!(loaded.contains("abc123:openai:text-embedding-3-small:1536"));

        // load_or_rebuild_bloom must use persisted bloom (not rebuild from empty)
        let reloaded =
            load_or_rebuild_bloom(temp_dir.path(), "openai", "text-embedding-3-small").unwrap();
        assert!(
            reloaded.contains(&key),
            "load_or_rebuild_bloom must recover persisted keys"
        );
    }

    /// RED: Callback never receives a batch exceeding the configured max size.
    /// GREEN: BatchBuilder token budget flushes batches before budget is exceeded.
    #[test]
    fn contract_token_budget_limits_batch_size() {
        // Each chunk = 15 chars → 5 tokens (15 / 3). Budget = 10 tokens.
        // So at most 2 chunks per batch before flushing.
        let config = crate::embed::token::BatchConfig {
            max_chunks_per_batch: 100,
            max_tokens_per_chunk: 10_000,
            batch_token_budget: Some(10),
        };
        let mut builder = crate::embed::token::BatchBuilder::new(config);

        // 5 tokens → ok, no flush
        assert!(builder.push(make_chunk("c1", "123456789012345")).is_none());
        // 10 tokens → ok, no flush (at budget)
        assert!(builder.push(make_chunk("c2", "123456789012345")).is_none());
        // 15 tokens would exceed 10 → flush old batch, start new with c3
        let flushed = builder.push(make_chunk("c3", "123456789012345"));
        assert!(flushed.is_some(), "must flush before exceeding budget");
        assert_eq!(
            flushed.unwrap().len(),
            2,
            "flushed batch must contain 2 chunks"
        );

        // Builder now has only c3 (5 tokens)
        // Push c4 → 10 tokens, no flush
        assert!(builder.push(make_chunk("c4", "123456789012345")).is_none());

        // Final flush — remaining 2 chunks
        let remaining = builder.flush();
        assert!(remaining.is_some());
        assert_eq!(remaining.unwrap().len(), 2);
    }

    /// RED: Rust pipeline produces deterministic, predictable output for known input.
    /// GREEN: Pipeline stats match expected values for a fixed chunk set.
    #[test]
    fn contract_output_stats_match_expected() {
        let temp_dir = tempfile::tempdir().unwrap();
        let embed_fn = TestEmbedFn {
            dims: 8,
            fail_indices: vec![],
        };
        let config = PipelineConfig {
            db_path: temp_dir.path().to_path_buf(),
            embed_batch_callback: Box::new(embed_fn),
            provider: "test".into(),
            model: "test-model".into(),
            output_dims: 8,
            max_chunks_per_batch: 2,
            incremental: false,
        };

        // 5 chunks, batch capacity = 2 → ceil(5/2) = 3 batches
        let chunks: Vec<BatchChunk> = (0..5)
            .map(|i| make_chunk(&format!("hash{}", i), &format!("text {}", i)))
            .collect();

        let stats = IndexingPipeline::run(config, &chunks).unwrap();

        // All chunks processed, no bloom (empty), no failures
        assert_eq!(stats.chunks_processed, 5);
        assert_eq!(stats.chunks_skipped, 0);
        assert_eq!(stats.batches_sent, 3, "ceil(5 / 2) = 3 batches");
        assert_eq!(stats.embeddings_sent, 5, "all 5 embedded");
        assert_eq!(stats.chunks_failed, 0);
    }
}
