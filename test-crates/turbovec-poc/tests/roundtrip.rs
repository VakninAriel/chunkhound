#![forbid(unsafe_code)]

// See src/main.rs for why: turbovec unconditionally needs a linked BLAS on Linux.
extern crate blas_src as _;

use turbovec::IdMapIndex;

const DIM: usize = 64;
const N: usize = 200;
const BIT_WIDTH: usize = 4;

/// Deterministic pseudo-random vectors — a plain LCG is enough for a
/// reproducible, hermetic test; no need to depend on the real fixture file.
fn deterministic_vectors(n: usize, dim: usize, seed: u64) -> Vec<f32> {
    let mut state = seed;
    let mut out = Vec::with_capacity(n * dim);
    for _ in 0..n * dim {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let v = ((state >> 33) as f32 / u32::MAX as f32) * 2.0 - 1.0;
        out.push(v);
    }
    out
}

fn normalize_all(vectors: &mut [f32], dim: usize) {
    for chunk in vectors.chunks_mut(dim) {
        let norm: f32 = chunk.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in chunk.iter_mut() {
                *x /= norm;
            }
        }
    }
}

fn build_index() -> IdMapIndex {
    let mut vectors = deterministic_vectors(N, DIM, 42);
    normalize_all(&mut vectors, DIM);
    let ids: Vec<u64> = (0..N as u64).collect();
    let mut index = IdMapIndex::new(DIM, BIT_WIDTH).expect("valid dim/bit_width");
    index.add_with_ids(&vectors, &ids).expect("add_with_ids");
    index
}

fn query_batch() -> Vec<f32> {
    let mut q = deterministic_vectors(20, DIM, 1337);
    normalize_all(&mut q, DIM);
    q
}

/// The single most important claim in the design doc: an index persisted
/// via `write()` and reconstructed via `load()` must return identical
/// search results to the original in-memory index. This is the entire
/// "why do this in Rust" rationale — if this doesn't hold, nothing else
/// about the design matters.
#[test]
fn write_load_roundtrip_produces_identical_search_results() {
    let index = build_index();
    let queries = query_batch();

    let path =
        std::env::temp_dir().join(format!("turbovec_poc_roundtrip_{}.tv", std::process::id()));
    index.write(&path).expect("write index snapshot");

    let before = index.search(&queries, 5);
    let loaded = IdMapIndex::load(&path).expect("load index snapshot");
    let after = loaded.search(&queries, 5);

    std::fs::remove_file(&path).ok();

    assert_eq!(
        before, after,
        "search results diverged after write()/load() roundtrip"
    );
    assert_eq!(loaded.len(), index.len());
}

/// Option 1 test: as of turbovec commit `ae31ba3` (pinned in Cargo.toml,
/// unreleased upstream fix for issue #70/PR #204), `TurboQuantIndex::from_parts()`
/// and its supporting accessors are now public. This proves the design doc's
/// per-vector `embeddings_quantized` fallback path end to end: rebuild a
/// working, bit-identical-search index directly from packed bytes + scale +
/// calibration — no whole-index snapshot blob, no re-embedding.
///
/// Note: `IdMapIndex` still has no accessor for its inner `TurboQuantIndex`
/// (confirmed by reading `id_map.rs` at this same commit — no `inner()` or
/// equivalent was added alongside `from_parts()`), so this test works
/// directly with the bare, positional `TurboQuantIndex` and hand-rolls the
/// id-reattachment layer (a parallel `Vec<u64>`, slot -> chunk_id) the same
/// way `IdMapIndex` does internally. Any real implementation of the design
/// doc's `embeddings_quantized` table would need to do the same — `from_parts()`
/// being public does not by itself give you an `IdMapIndex`.
#[test]
fn from_parts_reconstructs_index_identical_to_original() {
    let mut vectors = deterministic_vectors(N, DIM, 42);
    normalize_all(&mut vectors, DIM);
    // Stand-in for chunk_id: insertion order into a bare TurboQuantIndex is
    // slot order, so this external Vec<u64> is exactly the id-map layer a
    // real `embeddings_quantized` reader would need to maintain itself.
    let ids: Vec<u64> = (0..N as u64).collect();

    let mut index = turbovec::TurboQuantIndex::new(DIM, BIT_WIDTH).expect("valid dim/bit_width");
    index.add_2d(&vectors, DIM).expect("add_2d");

    let queries = query_batch();
    let n_queries = queries.len() / DIM;
    let before = index.search(&queries, 5);
    let before_ids: Vec<Vec<u64>> = (0..n_queries)
        .map(|qi| {
            before
                .indices_for_query(qi)
                .iter()
                .map(|&slot| ids[slot as usize])
                .collect()
        })
        .collect();

    // Extract exactly what a durable `embeddings_quantized` row-per-vector
    // table would store, plus the two index-level scalars — no whole-index
    // snapshot blob (`write()`/`load()`) involved anywhere in this path.
    let dim_opt = index.dim_opt();
    let bit_width = index.bit_width();
    let n_vectors = index.len();
    let packed_codes = index.packed_codes().to_vec();
    let scales = index.scales().to_vec();
    let tqplus_shift = index.tqplus_shift().to_vec();
    let tqplus_scale = index.tqplus_scale().to_vec();

    let rebuilt = turbovec::TurboQuantIndex::from_parts(
        dim_opt,
        bit_width,
        n_vectors,
        packed_codes,
        scales,
        tqplus_shift,
        tqplus_scale,
    )
    .expect("from_parts should accept data extracted from a valid index");

    let after = rebuilt.search(&queries, 5);
    let after_ids: Vec<Vec<u64>> = (0..n_queries)
        .map(|qi| {
            after
                .indices_for_query(qi)
                .iter()
                .map(|&slot| ids[slot as usize])
                .collect()
        })
        .collect();

    for qi in 0..n_queries {
        assert_eq!(
            before.scores_for_query(qi),
            after.scores_for_query(qi),
            "scores diverged for query {qi} after from_parts() rebuild"
        );
    }
    assert_eq!(
        before_ids, after_ids,
        "retrieved chunk ids diverged after from_parts() rebuild"
    );
    assert_eq!(rebuilt.len(), index.len());
}
