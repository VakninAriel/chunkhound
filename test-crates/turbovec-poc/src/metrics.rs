#![forbid(unsafe_code)]

/// recall@k: fraction of queries whose true nearest-neighbor id (ground
/// truth top-1) appears within the first `k` retrieved ids. `retrieved` rows
/// may hold more than `k` ids (e.g. a shared top-10 list reused for several
/// k cutoffs) — only the first `k` of each row are considered.
pub fn recall_at_k(ground_truth_top1: &[u64], retrieved: &[Vec<u64>], k: usize) -> f64 {
    let n = ground_truth_top1.len();
    assert_eq!(n, retrieved.len());
    let hits = ground_truth_top1
        .iter()
        .zip(retrieved.iter())
        .filter(|(gt, row)| row[..k.min(row.len())].contains(gt))
        .count();
    hits as f64 / n as f64
}

pub fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

pub fn percentile(mut values: Vec<f64>, p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((p / 100.0) * (values.len() as f64 - 1.0)).round() as usize;
    values[idx]
}

/// Compression ratio of float32 storage vs. the amortized per-vector cost
/// of a serialized index file (which also includes the id table, codebook,
/// and calibration overhead, so this is deliberately the whole-file size
/// divided by vector count, not a raw bits-per-vector calculation).
pub fn compression_ratio(
    float32_bytes_per_vector: usize,
    index_file_bytes: usize,
    n_vectors: usize,
) -> f64 {
    let amortized = index_file_bytes as f64 / n_vectors as f64;
    float32_bytes_per_vector as f64 / amortized
}
