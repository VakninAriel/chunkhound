#![forbid(unsafe_code)]

//! Standalone feasibility benchmark for the `turbovec` crate against this
//! repo's own real, indexed embeddings — see ../README.md for the full
//! write-up and go/no-go conclusion.

// turbovec unconditionally requires `ndarray`'s `blas` feature on Linux
// (see turbovec's Cargo.toml), so a BLAS implementation must be linked in.
// `blas-src` never gets referenced by name from Rust code — without this
// `extern crate`, cargo drops it as unused and the final link fails.
extern crate blas_src as _;

use std::fs;
use std::time::Instant;

use turbovec::IdMapIndex;
use turbovec_poc::{bruteforce, fixture, metrics, normalize};

const FIXTURE_PATH: &str = "fixtures/embeddings_256.bin";
const HELD_OUT_QUERIES: usize = 500;
const K: usize = 10;

fn main() {
    let fx = fixture::load(FIXTURE_PATH);
    println!("Loaded {} vectors, dim={}", fx.ids.len(), fx.dim);

    let mut vectors = fx.vectors;
    for chunk in vectors.chunks_mut(fx.dim) {
        normalize(chunk);
    }

    // Held-out split: the last HELD_OUT_QUERIES vectors become queries and
    // are excluded from the corpus/index, so recall reflects real
    // nearest-neighbor retrieval rather than trivial self-match.
    let n = fx.ids.len();
    let split = n - HELD_OUT_QUERIES;
    let corpus_ids = &fx.ids[..split];
    let corpus_vectors = &vectors[..split * fx.dim];
    let query_ids = &fx.ids[split..];
    let query_vectors = &vectors[split * fx.dim..];

    println!(
        "Corpus: {} vectors, held-out queries: {}",
        corpus_ids.len(),
        query_ids.len()
    );

    // Ground truth: brute-force cosine top-k for every held-out query.
    // Computed once and reused across both bit-widths below. Also captures
    // the top1-top2 score gap (Stage A diagnostic: is a near-tie in the
    // exact ranking what's letting quantization noise flip the winner?).
    let mut gt_top1 = Vec::with_capacity(query_ids.len());
    let mut gt_gap = Vec::with_capacity(query_ids.len());
    let mut gt_latencies_ms = Vec::with_capacity(query_ids.len());
    for qi in 0..query_ids.len() {
        let q = &query_vectors[qi * fx.dim..(qi + 1) * fx.dim];
        let start = Instant::now();
        let (scores, ids) = bruteforce::top_k(corpus_vectors, corpus_ids, fx.dim, q, K);
        gt_latencies_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        gt_top1.push(ids[0]);
        gt_gap.push((scores[0] - scores[1]) as f64);
    }
    let gt_median = metrics::percentile(gt_latencies_ms.clone(), 50.0);
    let gt_p95 = metrics::percentile(gt_latencies_ms, 95.0);
    println!(
        "Brute-force baseline: median {gt_median:.2}ms, p95 {gt_p95:.2}ms/query ({:.0} q/s)",
        1000.0 / gt_median
    );

    for &bit_width in &[2usize, 4usize] {
        println!("\n=== bit_width={bit_width} ===");
        let mut index = IdMapIndex::new(fx.dim, bit_width).expect("valid dim/bit_width");
        let rebuild_start = Instant::now();
        index
            .add_with_ids(corpus_vectors, corpus_ids)
            .expect("ids match vector count, no duplicate ids");
        let rebuild_elapsed = rebuild_start.elapsed();
        println!(
            "Cold-start rebuild cost (Option 3B/C: add_with_ids over the full corpus, \
             no re-embedding): {:.2}s for {} vectors",
            rebuild_elapsed.as_secs_f64(),
            corpus_ids.len()
        );

        // bit_width=4 is the canonical snapshot the `serve` demo binary
        // loads later; bit_width=2 is measured here but not kept.
        let snapshot_path = if bit_width == 4 {
            "fixtures/index_4bit.tv".to_string()
        } else {
            format!("fixtures/index_{bit_width}bit_tmp.tv")
        };
        index
            .write(&snapshot_path)
            .expect("failed to write index snapshot");
        let index_bytes = fs::metadata(&snapshot_path)
            .expect("snapshot file missing after write")
            .len() as usize;
        let ratio = metrics::compression_ratio(fx.dim * 4, index_bytes, corpus_ids.len());
        println!(
            "Compression: {ratio:.1}x  (float32 {} B/vector -> {:.1} B/vector amortized, incl. id table)",
            fx.dim * 4,
            index_bytes as f64 / corpus_ids.len() as f64
        );

        let mut tv_top1 = Vec::with_capacity(query_ids.len());
        let mut tv_top10: Vec<Vec<u64>> = Vec::with_capacity(query_ids.len());
        let mut tv_latencies_ms = Vec::with_capacity(query_ids.len());
        for qi in 0..query_ids.len() {
            let q = &query_vectors[qi * fx.dim..(qi + 1) * fx.dim];
            let start = Instant::now();
            let (_, ids) = index.search(q, K);
            tv_latencies_ms.push(start.elapsed().as_secs_f64() * 1000.0);
            tv_top1.push(*ids.first().unwrap_or(&u64::MAX));
            tv_top10.push(ids);
        }

        let recall_at_1 = gt_top1
            .iter()
            .zip(tv_top1.iter())
            .filter(|(a, b)| a == b)
            .count() as f64
            / query_ids.len() as f64;
        let recall_at_10 = metrics::recall_at_k(&gt_top1, &tv_top10, 10);
        let tv_median = metrics::percentile(tv_latencies_ms.clone(), 50.0);
        let tv_p95 = metrics::percentile(tv_latencies_ms, 95.0);

        println!("Recall@1  (exact top-1 match):     {recall_at_1:.4}");
        println!("Recall@10 (true top-1 within top-10): {recall_at_10:.4}");
        println!(
            "TurboVec latency: median {tv_median:.3}ms, p95 {tv_p95:.3}ms/query ({:.0} q/s)",
            1000.0 / tv_median
        );

        // Stage A diagnostic: do recall@1 misses cluster on queries where the
        // exact top1/top2 scores were nearly tied? If so, the gap is at least
        // partly ranking ambiguity (near-ties flipped by quantization noise),
        // not purely a TurboVec quantization defect.
        let (hit_gaps, miss_gaps): (Vec<f64>, Vec<f64>) =
            gt_top1.iter().zip(tv_top1.iter()).zip(gt_gap.iter()).fold(
                (Vec::new(), Vec::new()),
                |(mut hits, mut misses), ((a, b), &gap)| {
                    if a == b {
                        hits.push(gap);
                    } else {
                        misses.push(gap);
                    }
                    (hits, misses)
                },
            );
        println!(
            "Near-tie check: mean top1-top2 gap on hits = {:.4} (n={}), on misses = {:.4} (n={})",
            metrics::mean(&hit_gaps),
            hit_gaps.len(),
            metrics::mean(&miss_gaps),
            miss_gaps.len()
        );
    }

    println!(
        "\nNOTE: TurboQuantIndex::from_parts()/packed_codes()/scales()/tqplus_shift()/tqplus_scale() \
         were pub(crate) in the published turbovec 0.9.0 (crates.io) — NOT part of the public API. \
         This crate is now git-pinned (see Cargo.toml) to unreleased upstream commit ae31ba3, which \
         makes them public (PR #204, merged 2026-07-25, not yet released to crates.io). \
         tests/roundtrip.rs now has a passing from_parts_reconstructs_index_identical_to_original test \
         proving the design doc's 'reconstruct from embeddings_quantized rows without a whole snapshot' \
         fallback path IS achievable against that commit — see README.md for the git-pin caveat."
    );
}
