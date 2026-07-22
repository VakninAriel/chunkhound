// Pipeline benchmarks — exercises the stub IndexingPipeline with varying
// chunk counts and batch sizes, measuring throughput and bloom hit rate.
//
// No criterion dependency; benchmarks are #[test] functions that self-report
// timing via stdout. Run with:
//   cargo test pipeline::bench -- --nocapture
//
// Python-vs-Rust parity comparison is DEFERRED — the stub pipeline is not
// yet exposed through PyO3/chunkhound_native (PR #380).

use std::time::Instant;

use crate::bloom::{bloom_key, persist_bloom, persist_meta, AtomicBloomFilter, BloomMeta};
use crate::embed::token::BatchChunk;
use crate::embed::{BatchCallStats, EmbedBatchFn, EmbedBatchResult};
use crate::pipeline::pipeline::{IndexingPipeline, PipelineConfig, PipelineStats};

// ── Mock embed function with configurable latency ──

struct BenchEmbedFn {
    dims: usize,
    /// Simulated per-batch latency in microseconds
    latency_us: u64,
}

impl EmbedBatchFn for BenchEmbedFn {
    fn embed_batch(
        &self,
        texts: &[String],
        _provider: &str,
        _model: &str,
        _dims: usize,
    ) -> EmbedBatchResult {
        if self.latency_us > 0 {
            std::thread::sleep(std::time::Duration::from_micros(self.latency_us));
        }
        let vectors: Vec<Option<Vec<f32>>> = texts
            .iter()
            .map(|_| Some(vec![1.0_f32; self.dims]))
            .collect();
        EmbedBatchResult {
            vectors,
            stats: BatchCallStats {
                api_calls: 1,
                total_latency_ms: self.latency_us / 1000,
            },
        }
    }
}

// ── Helpers ──

fn make_chunk(hash: &str, text: &str) -> BatchChunk {
    BatchChunk {
        content_hash: hash.to_string(),
        text: text.to_string(),
    }
}

/// Generate n synthetic chunks with realistic-looking code content.
fn generate_chunks(n: usize) -> Vec<BatchChunk> {
    (0..n)
        .map(|i| {
            let hash = format!("{:016x}", i);
            let text = format!(
                "fn process_item_{i}(&self, input: &[u8]) -> Result<Vec<u8>, Error> {{\n    \
                 let validated = self.validator.validate(input)?;\n    \
                 let transformed = self.transformer.transform(&validated)?;\n    \
                 self.cache.store(&transformed)?;\n    \
                 Ok(transformed)\n}}",
            );
            make_chunk(&hash, &text)
        })
        .collect()
}

/// Print a formatted benchmark result line.
fn report_run(label: &str, stats: &PipelineStats, elapsed: std::time::Duration) {
    let elapsed_s = elapsed.as_secs_f64();
    let throughput = if elapsed_s > 0.0 {
        stats.chunks_processed as f64 / elapsed_s
    } else {
        f64::INFINITY
    };
    let skip_rate = if stats.chunks_processed > 0 {
        (stats.chunks_skipped as f64 / stats.chunks_processed as f64) * 100.0
    } else {
        0.0
    };

    println!(
        "  {:<20} | {:>7} ch | {:>5} sk ({:>5.1}%) | {:>4} ba | {:>6} emb | {:>4} fail | {:>8.2}s | {:>10.0} ch/s",
        label,
        stats.chunks_processed,
        stats.chunks_skipped,
        skip_rate,
        stats.batches_sent,
        stats.embeddings_sent,
        stats.chunks_failed,
        elapsed_s,
        throughput,
    );
}

fn make_config(temp_dir: &tempfile::TempDir, dims: usize, max_per_batch: usize) -> PipelineConfig {
    PipelineConfig {
        db_path: temp_dir.path().to_path_buf(),
        embed_batch_callback: Box::new(BenchEmbedFn {
            dims,
            latency_us: 0,
        }),
        provider: "bench".into(),
        model: "bench-model".into(),
        output_dims: dims,
        max_chunks_per_batch: max_per_batch,
        incremental: false,
    }
}

// ── Benchmark Tests ──

/// Baseline: small workload with no bloom, no latency.
#[test]
fn bench_small_100_chunks() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = make_config(&temp_dir, 768, 20);
    let chunks = generate_chunks(100);

    let start = Instant::now();
    let stats = IndexingPipeline::run(config, &chunks).unwrap();
    let elapsed = start.elapsed();

    println!("\n─── bench_small_100_chunks ───");
    report_run("small (100 ch)", &stats, elapsed);
}

/// Medium workload: 1,000 chunks.
#[test]
fn bench_medium_1k_chunks() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = make_config(&temp_dir, 768, 50);
    let chunks = generate_chunks(1_000);

    let start = Instant::now();
    let stats = IndexingPipeline::run(config, &chunks).unwrap();
    let elapsed = start.elapsed();

    println!("\n─── bench_medium_1k_chunks ───");
    report_run("medium (1k ch)", &stats, elapsed);
}

/// Large workload: 10,000 chunks, pushing batch/embed path hard.
#[test]
fn bench_large_10k_chunks() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = make_config(&temp_dir, 1536, 100);
    let chunks = generate_chunks(10_000);

    let start = Instant::now();
    let stats = IndexingPipeline::run(config, &chunks).unwrap();
    let elapsed = start.elapsed();

    println!("\n─── bench_large_10k_chunks ───");
    report_run("large (10k ch)", &stats, elapsed);
}

/// Varying batch sizes with fixed workload (1k chunks).
#[test]
fn bench_batch_size_sweep() {
    let temp_dir = tempfile::tempdir().unwrap();
    let chunks = generate_chunks(1_000);

    println!("\n─── bench_batch_size_sweep (1k chunks) ───");
    println!(
        "  {:<20} | {:>7} | {:>5} | {:>5} | {:>4} | {:>6} | {:>4} | {:>8} | {:>10}",
        "Batch", "Chunks", "Sk", "Sk%", "Bat", "Emb", "Fail", "Time", "Ch/s"
    );

    for &batch_size in &[10, 25, 50, 100, 200] {
        let config = make_config(&temp_dir, 768, batch_size);
        let start = Instant::now();
        let stats = IndexingPipeline::run(config, &chunks).unwrap();
        let elapsed = start.elapsed();
        report_run(&format!("batch={}", batch_size), &stats, elapsed);
    }
}

/// Bloom hit rate impact: 50% pre-populated bloom.
#[test]
fn bench_bloom_hit_rate_50pct() {
    let temp_dir = tempfile::tempdir().unwrap();
    let bloom_path = temp_dir.path().join("embeddings.bloom");
    let meta_path = temp_dir.path().join("embeddings.bloom.meta");

    // Pre-populate bloom with keys for even-indexed chunks (50% hit rate)
    let expected_items = 10_000;
    let mut bloom = AtomicBloomFilter::with_false_pos(0.01, expected_items);
    let chunks = generate_chunks(1_000);
    for i in (0..chunks.len()).step_by(2) {
        bloom.insert(&bloom_key(
            &chunks[i].content_hash,
            "bench",
            "bench-model",
            768,
        ));
    }
    persist_bloom(&bloom, &bloom_path).unwrap();
    persist_meta(
        &BloomMeta {
            provider: "bench".into(),
            model: "bench-model".into(),
        },
        &meta_path,
    )
    .unwrap();

    let config = make_config(&temp_dir, 768, 50);
    let start = Instant::now();
    let stats = IndexingPipeline::run(config, &chunks).unwrap();
    let elapsed = start.elapsed();

    println!("\n─── bench_bloom_hit_rate_50pct (1k chunks, 50% pre-populated) ───");
    report_run("bloom=50% hit", &stats, elapsed);

    // Contract: ~50% skipped
    let skip_pct = (stats.chunks_skipped as f64 / stats.chunks_processed as f64) * 100.0;
    assert!(
        (45.0..=55.0).contains(&skip_pct),
        "Expected ~50% bloom hit rate, got {:.1}%",
        skip_pct
    );
    assert_eq!(
        stats.embeddings_sent, 500,
        "With 50% skip, expect 500 embeddings, got {}",
        stats.embeddings_sent
    );
}

/// Bloom hit rate impact: 90% pre-populated bloom (heavy incremental use).
#[test]
fn bench_bloom_hit_rate_90pct() {
    let temp_dir = tempfile::tempdir().unwrap();
    let bloom_path = temp_dir.path().join("embeddings.bloom");
    let meta_path = temp_dir.path().join("embeddings.bloom.meta");

    let expected_items = 10_000;
    let mut bloom = AtomicBloomFilter::with_false_pos(0.01, expected_items);
    let chunks = generate_chunks(1_000);

    // Populate 900 of 1000 keys
    for chunk in chunks.iter().take(900) {
        bloom.insert(&bloom_key(&chunk.content_hash, "bench", "bench-model", 768));
    }
    persist_bloom(&bloom, &bloom_path).unwrap();
    persist_meta(
        &BloomMeta {
            provider: "bench".into(),
            model: "bench-model".into(),
        },
        &meta_path,
    )
    .unwrap();

    let config = make_config(&temp_dir, 768, 50);
    let start = Instant::now();
    let stats = IndexingPipeline::run(config, &chunks).unwrap();
    let elapsed = start.elapsed();

    println!("\n─── bench_bloom_hit_rate_90pct (1k chunks, 90% pre-populated) ───");
    report_run("bloom=90% hit", &stats, elapsed);

    let skip_pct = (stats.chunks_skipped as f64 / stats.chunks_processed as f64) * 100.0;
    assert!(
        (85.0..=95.0).contains(&skip_pct),
        "Expected ~90% bloom hit rate, got {:.1}%",
        skip_pct
    );
}

/// Bloom hit rate impact: 0% (empty bloom = baseline).
#[test]
fn bench_bloom_hit_rate_0pct() {
    let temp_dir = tempfile::tempdir().unwrap();
    let chunks = generate_chunks(1_000);
    let config = make_config(&temp_dir, 768, 50);

    let start = Instant::now();
    let stats = IndexingPipeline::run(config, &chunks).unwrap();
    let elapsed = start.elapsed();

    println!("\n─── bench_bloom_hit_rate_0pct (1k chunks, empty bloom) ───");
    report_run("bloom=0% hit", &stats, elapsed);
    assert_eq!(stats.chunks_skipped, 0);
}

/// Varying dimensions: 384 (small), 768 (medium), 1536 (large), 3072 (extra-large).
#[test]
fn bench_dimension_sweep() {
    let temp_dir = tempfile::tempdir().unwrap();
    let chunks = generate_chunks(1_000);

    println!("\n─── bench_dimension_sweep (1k chunks, batch=50) ───");
    for dims in &[384, 768, 1536, 3072] {
        let mut config = make_config(&temp_dir, *dims, 50);
        config.embed_batch_callback = Box::new(BenchEmbedFn {
            dims: *dims,
            latency_us: 0,
        });
        let start = Instant::now();
        let stats = IndexingPipeline::run(config, &chunks).unwrap();
        let elapsed = start.elapsed();
        report_run(&format!("dim={}", dims), &stats, elapsed);
    }
}

/// Oversized chunk filtering: mix of normal and huge chunks.
#[test]
fn bench_oversized_chunk_filtering() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut chunks = generate_chunks(900);
    // Append 100 oversized chunks
    for i in 0..100 {
        chunks.push(make_chunk(
            &format!("oversized_{:016x}", i),
            &"x".repeat(30_000), // 10,000 tokens > 8192 max
        ));
    }

    let config = make_config(&temp_dir, 768, 50);
    let start = Instant::now();
    let stats = IndexingPipeline::run(config, &chunks).unwrap();
    let elapsed = start.elapsed();

    println!("\n─── bench_oversized_chunk_filtering (900 normal + 100 huge) ───");
    report_run("oversized mix", &stats, elapsed);
    assert_eq!(
        stats.chunks_processed, 1000,
        "all 1000 chunks counted as processed"
    );
    assert_eq!(
        stats.chunks_skipped, 100,
        "100 oversized chunks should be skipped"
    );
    assert_eq!(
        stats.embeddings_sent, 900,
        "only 900 normal chunks should be embedded"
    );
}
