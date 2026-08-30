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

fn query_batch() -> Vec<f32> {
    let mut q = deterministic_vectors(20, DIM, 1337);
    normalize_all(&mut q, DIM);
    q
}

/// Option 3(B)/(C): if `turbovec_indexes`'s snapshot is lost entirely (not
/// just one corrupt generation — every copy gone), rebuild by re-running
/// `add_with_ids()` against the durable float32 vectors ChunkHound already
/// stores in `embeddings_{dims}` today. No `from_parts()`, no packed-byte
/// storage, and critically no re-embedding — this is a pure local
/// re-quantization of data that was never at risk.
#[test]
fn rebuild_from_stored_floats_matches_original() {
    let mut vectors = deterministic_vectors(N, DIM, 42);
    normalize_all(&mut vectors, DIM);
    let ids: Vec<u64> = (0..N as u64).collect();

    let mut original = IdMapIndex::new(DIM, BIT_WIDTH).expect("valid dim/bit_width");
    original.add_with_ids(&vectors, &ids).expect("add_with_ids");

    let queries = query_batch();
    let before = original.search(&queries, 5);

    // Simulate total snapshot loss: no `.tv` file, no `IdMapIndex::load()`
    // anywhere in this path. "Recovery" is calling add_with_ids() again on
    // the same float32 vectors, standing in for reading embeddings_{dims}
    // rows out of DuckDB.
    let mut rebuilt = IdMapIndex::new(DIM, BIT_WIDTH).expect("valid dim/bit_width");
    rebuilt.add_with_ids(&vectors, &ids).expect("add_with_ids");

    let after = rebuilt.search(&queries, 5);
    assert_eq!(
        before, after,
        "index rebuilt from stored floats diverged from the original"
    );
}
