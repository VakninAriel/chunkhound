#![forbid(unsafe_code)]

/// Exact cosine-similarity top-k over a pre-normalized corpus — the
/// ground truth for recall measurement. O(n*dim) per query; fine at this
/// corpus's ~73K-vector scale, not meant to scale further.
pub fn top_k(
    corpus: &[f32],
    ids: &[u64],
    dim: usize,
    query: &[f32],
    k: usize,
) -> (Vec<f32>, Vec<u64>) {
    let n = ids.len();
    let mut scored: Vec<(f32, u64)> = Vec::with_capacity(n);
    for i in 0..n {
        let row = &corpus[i * dim..(i + 1) * dim];
        let dot: f32 = row.iter().zip(query.iter()).map(|(a, b)| a * b).sum();
        scored.push((dot, ids[i]));
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    scored.truncate(k);
    let scores = scored.iter().map(|(s, _)| *s).collect();
    let out_ids = scored.iter().map(|(_, id)| *id).collect();
    (scores, out_ids)
}
