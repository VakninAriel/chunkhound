#![forbid(unsafe_code)]

use std::fs;
use std::path::Path;

/// Flat little-endian dump produced by `scripts/dump_embeddings.py`:
/// magic "CHKNVEC1" + n:u32 + dim:u32, then per record chunk_id:i64 + dim*f32.
const MAGIC: &[u8] = b"CHKNVEC1";

pub struct Fixture {
    pub dim: usize,
    pub ids: Vec<u64>,
    pub vectors: Vec<f32>,
}

pub fn load(path: impl AsRef<Path>) -> Fixture {
    let bytes = fs::read(path).expect("failed to read fixture file");
    assert!(bytes.len() >= 16, "fixture file too short");
    assert_eq!(&bytes[0..8], MAGIC, "bad fixture magic");
    let n = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let dim = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;

    let record_size = 8 + dim * 4;
    let expected_len = 16 + n * record_size;
    assert_eq!(
        bytes.len(),
        expected_len,
        "fixture file size does not match header"
    );

    let mut ids = Vec::with_capacity(n);
    let mut vectors = Vec::with_capacity(n * dim);
    let mut offset = 16;
    for _ in 0..n {
        let chunk_id = i64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        ids.push(chunk_id as u64);
        offset += 8;
        for chunk in bytes[offset..offset + dim * 4].chunks_exact(4) {
            vectors.push(f32::from_le_bytes(chunk.try_into().unwrap()));
        }
        offset += dim * 4;
    }

    Fixture { dim, ids, vectors }
}
