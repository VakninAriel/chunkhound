#![forbid(unsafe_code)]

//! Stage B diagnostic / dimension-comparison matrix: runs the full
//! compression + recall@{1,3,5,10} + latency suite (same methodology as
//! `bench`) against an arbitrary fixture path, so results are directly
//! comparable across dimensions on the same matched corpus/query split
//! (produced by scripts/probe_native_dim.py + scripts/derive_1536.py).
//!
//! Usage: cargo run --release --bin probe_dim -- <fixture_path> [held_out_queries]
//!
//! Prints a human-readable report, then one `RESULT_JSON: {...}` line for
//! easy aggregation across multiple runs (one per dimension).

extern crate blas_src as _;

use std::env;
use std::fs;
use std::time::Instant;

use serde_json::json;
use turbovec::IdMapIndex;
use turbovec_poc::{bruteforce, fixture, metrics, normalize};

const K_MAX: usize = 10;
const RECALL_KS: &[usize] = &[1, 3, 5, 10];
const DEFAULT_HELD_OUT: usize = 300;

fn main() {
    let args: Vec<String> = env::args().collect();
    let fixture_path = args
        .get(1)
        .expect("usage: probe_dim <fixture_path> [held_out_queries]");
    let held_out: usize = args
        .get(2)
        .map(|s| s.parse().expect("held_out_queries must be a number"))
        .unwrap_or(DEFAULT_HELD_OUT);

    let fx = fixture::load(fixture_path);
    println!("Loaded {} vectors, dim={}", fx.ids.len(), fx.dim);

    let mut vectors = fx.vectors;
    for chunk in vectors.chunks_mut(fx.dim) {
        normalize(chunk);
    }

    let n = fx.ids.len();
    assert!(
        held_out < n,
        "held_out_queries must be smaller than the fixture size"
    );
    let split = n - held_out;
    let corpus_ids = &fx.ids[..split];
    let corpus_vectors = &vectors[..split * fx.dim];
    let query_ids = &fx.ids[split..];
    let query_vectors = &vectors[split * fx.dim..];

    println!(
        "Corpus: {} vectors, held-out queries: {}",
        corpus_ids.len(),
        query_ids.len()
    );

    // Ground truth (shared across both bit-widths below).
    let mut gt_top1 = Vec::with_capacity(query_ids.len());
    let mut gt_latencies_ms = Vec::with_capacity(query_ids.len());
    for qi in 0..query_ids.len() {
        let q = &query_vectors[qi * fx.dim..(qi + 1) * fx.dim];
        let start = Instant::now();
        let (_, ids) = bruteforce::top_k(corpus_vectors, corpus_ids, fx.dim, q, K_MAX);
        gt_latencies_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        gt_top1.push(ids[0]);
    }
    let bf_median = metrics::percentile(gt_latencies_ms.clone(), 50.0);
    let bf_p95 = metrics::percentile(gt_latencies_ms, 95.0);
    println!("Brute-force baseline: median {bf_median:.3}ms, p95 {bf_p95:.3}ms/query");

    let mut bit_width_results = serde_json::Map::new();

    for &bit_width in &[2usize, 4usize] {
        println!("\n=== dim={} bit_width={bit_width} ===", fx.dim);
        let mut index = IdMapIndex::new(fx.dim, bit_width).expect("valid dim/bit_width");
        index
            .add_with_ids(corpus_vectors, corpus_ids)
            .expect("ids match vector count, no duplicate ids");

        let snapshot_path = std::env::temp_dir().join(format!(
            "probe_dim_{}_{}_{}.tv",
            fx.dim,
            bit_width,
            std::process::id()
        ));
        index
            .write(&snapshot_path)
            .expect("failed to write index snapshot");
        let index_bytes = fs::metadata(&snapshot_path)
            .expect("snapshot file missing after write")
            .len() as usize;
        fs::remove_file(&snapshot_path).ok();
        let ratio = metrics::compression_ratio(fx.dim * 4, index_bytes, corpus_ids.len());
        println!(
            "Compression: {ratio:.2}x  ({} B/vector float32 -> {:.1} B/vector amortized)",
            fx.dim * 4,
            index_bytes as f64 / corpus_ids.len() as f64
        );

        let mut tv_top10: Vec<Vec<u64>> = Vec::with_capacity(query_ids.len());
        let mut tv_latencies_ms = Vec::with_capacity(query_ids.len());
        for qi in 0..query_ids.len() {
            let q = &query_vectors[qi * fx.dim..(qi + 1) * fx.dim];
            let start = Instant::now();
            let (_, ids) = index.search(q, K_MAX);
            tv_latencies_ms.push(start.elapsed().as_secs_f64() * 1000.0);
            tv_top10.push(ids);
        }
        let tv_median = metrics::percentile(tv_latencies_ms.clone(), 50.0);
        let tv_p95 = metrics::percentile(tv_latencies_ms, 95.0);

        let mut recall_map = serde_json::Map::new();
        for &k in RECALL_KS {
            let recall = metrics::recall_at_k(&gt_top1, &tv_top10, k);
            println!("Recall@{k:<2}: {recall:.4}");
            recall_map.insert(k.to_string(), json!(recall));
        }
        println!("TurboVec latency: median {tv_median:.3}ms, p95 {tv_p95:.3}ms/query");

        bit_width_results.insert(
            bit_width.to_string(),
            json!({
                "compression_ratio": ratio,
                "recall": recall_map,
                "latency_median_ms": tv_median,
                "latency_p95_ms": tv_p95,
            }),
        );
    }

    let result = json!({
        "fixture": fixture_path,
        "dim": fx.dim,
        "n_corpus": corpus_ids.len(),
        "n_queries": query_ids.len(),
        "brute_force_latency_median_ms": bf_median,
        "brute_force_latency_p95_ms": bf_p95,
        "bit_widths": bit_width_results,
    });
    println!("\nRESULT_JSON: {}", serde_json::to_string(&result).unwrap());
}
