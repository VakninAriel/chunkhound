#![forbid(unsafe_code)]

// See src/main.rs for why: turbovec unconditionally needs a linked BLAS on Linux.
extern crate blas_src as _;

use std::path::Path;

use turbovec::IdMapIndex;
use turbovec_poc::snapshot;

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

fn corrupt_file(path: &Path) {
    let mut bytes = std::fs::read(path).expect("read file to corrupt");
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF; // flip bits -- guaranteed to differ from the original
    std::fs::write(path, bytes).expect("write corrupted bytes");
}

/// Option 3(C): durability engineering on the one snapshot mechanism the
/// *published* turbovec 0.9.0 API gives us for free (`IdMapIndex::write()`/
/// `load()`) — no `from_parts()` needed at all. Rotation + checksums mean a
/// single corrupt generation doesn't lose the index; only every generation
/// failing at once (simulated here) is the accepted catastrophic path.
#[test]
fn corrupt_generation_falls_through_to_next() {
    let index = build_index();
    let queries = query_batch();
    let before = index.search(&queries, 5);

    let dir = std::env::temp_dir().join(format!("turbovec_poc_rotation_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let gen0 = dir.join("gen_0.tv");
    let gen1 = dir.join("gen_1.tv");

    snapshot::write_checksummed(&index, &gen0).expect("write gen0");
    snapshot::write_checksummed(&index, &gen1).expect("write gen1");

    // Corrupt generation 0's snapshot bytes after the fact — its checksum
    // sidecar still records the original, correct checksum, so this must
    // be *detected*, not silently loaded as if nothing happened.
    corrupt_file(&gen0);

    let recovered = snapshot::load_any_verified(&[gen0.clone(), gen1.clone()])
        .expect("should fall through to gen1 after gen0 fails its checksum");
    let after = recovered.search(&queries, 5);
    assert_eq!(
        before, after,
        "recovered index diverged from the pre-corruption baseline"
    );

    // Now corrupt every generation — no recoverable local snapshot at all.
    corrupt_file(&gen1);
    assert!(
        snapshot::load_any_verified(&[gen0, gen1]).is_none(),
        "load_any_verified should report failure cleanly, not panic or \
         return a garbage index, when every generation is corrupt"
    );

    std::fs::remove_dir_all(&dir).ok();
}
