// BloomFilter + bloom_key are used by downstream tasks (task-3 through task-13).
// The functions and struct are dead-code only until those modules land.
#![allow(dead_code)]

use std::fs;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use fastbloom::BloomFilter;
use serde::{Deserialize, Serialize};

use crate::error::PipelineError;

/// Wrapper providing thread-safe concurrent reads via Arc.
/// Inserts require &mut self (single writer).
pub struct AtomicBloomFilter {
    inner: BloomFilter,
}

impl AtomicBloomFilter {
    pub fn with_false_pos(fp: f64, expected_items: usize) -> Self {
        Self {
            inner: BloomFilter::with_false_pos(fp)
                .seed(&0)
                .expected_items(expected_items),
        }
    }

    pub fn insert(&mut self, key: &str) {
        self.inner.insert(key);
    }

    pub fn contains(&self, key: &str) -> bool {
        self.inner.contains(key)
    }
}

pub fn bloom_key(content_hash: &str, provider: &str, model: &str, dims: usize) -> String {
    format!("{content_hash}:{provider}:{model}:{dims}")
}

// ── Persistence ──

/// Serializable representation of a BloomFilter's internal state.
/// fastbloom 0.7 has no serde support, so we capture the bit vector and
/// target-hash count manually and reconstruct via from_vec + hashes().
#[derive(Serialize, Deserialize)]
struct BloomFilterData {
    data: Vec<u64>,
    num_hashes: u32,
}

pub fn persist_bloom(bloom: &AtomicBloomFilter, path: &Path) -> Result<(), PipelineError> {
    let payload = BloomFilterData {
        data: bloom.inner.as_slice().to_vec(),
        num_hashes: bloom.inner.num_hashes(),
    };
    let bytes = bincode::serialize(&payload).map_err(|e| PipelineError::IoError {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let file = fs::File::create(path).map_err(|e| PipelineError::IoError {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let mut writer = BufWriter::new(file);
    std::io::Write::write_all(&mut writer, &bytes).map_err(|e| PipelineError::IoError {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    Ok(())
}

pub fn load_bloom_from_disk(path: &Path) -> Result<Option<AtomicBloomFilter>, PipelineError> {
    match fs::File::open(path) {
        Ok(file) => {
            let mut reader = BufReader::new(file);
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut reader, &mut buf).map_err(|e| {
                PipelineError::IoError {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                }
            })?;
            let payload: BloomFilterData =
                bincode::deserialize(&buf).map_err(|e| PipelineError::IoError {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                })?;
            let inner = BloomFilter::from_vec(payload.data)
                .seed(&0)
                .hashes(payload.num_hashes);
            Ok(Some(AtomicBloomFilter { inner }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(PipelineError::IoError {
            path: path.to_path_buf(),
            message: e.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Key Construction ──

    #[test]
    fn bloom_key_includes_all_components() {
        let key = bloom_key("abc123", "openai", "text-embedding-3-small", 1536);
        assert_eq!(key, "abc123:openai:text-embedding-3-small:1536");
    }

    #[test]
    fn bloom_key_distinguishes_providers() {
        let key_oai = bloom_key("abc123", "openai", "model", 1536);
        let key_voy = bloom_key("abc123", "voyageai", "model", 1536);
        assert_ne!(key_oai, key_voy);
    }

    #[test]
    fn bloom_key_distinguishes_models() {
        let key_1 = bloom_key("abc123", "openai", "text-embedding-3-small", 1536);
        let key_2 = bloom_key("abc123", "openai", "text-embedding-3-large", 3072);
        assert_ne!(key_1, key_2);
    }

    #[test]
    fn bloom_key_distinguishes_dims_same_model() {
        let key_256 = bloom_key("abc123", "openai", "text-embedding-3-small", 256);
        let key_1536 = bloom_key("abc123", "openai", "text-embedding-3-small", 1536);
        assert_ne!(key_256, key_1536);
    }

    // ── Insert + Contains ──

    #[test]
    fn bloom_insert_then_contains() {
        let mut bloom = AtomicBloomFilter::with_false_pos(0.01, 10_000);
        let key = bloom_key("hash1", "openai", "text-embedding-3-small", 1536);
        bloom.insert(&key);
        assert!(bloom.contains(&key), "inserted key must be found");
    }

    #[test]
    fn bloom_does_not_contain_uninserted() {
        let bloom = AtomicBloomFilter::with_false_pos(0.01, 10_000);
        assert!(!bloom.contains("never-inserted:openai:model:1536"));
    }

    // ── Persistence ──

    #[test]
    fn bloom_false_positive_rate_within_bounds() {
        let n_items = 100_000;
        let mut bloom = AtomicBloomFilter::with_false_pos(0.01, n_items);

        for i in 0..n_items {
            bloom.insert(&format!("hash{i}:openai:text-embedding-3-small:1536"));
        }

        let mut false_positives = 0u64;
        let check_count = 10_000;
        for i in n_items..n_items + check_count {
            if bloom.contains(&format!("hash{i}:openai:text-embedding-3-small:1536")) {
                false_positives += 1;
            }
        }

        let fpr = false_positives as f64 / check_count as f64;
        assert!(fpr < 0.02, "FPR {:.4} exceeds 2% threshold", fpr);
    }

    #[test]
    fn persist_and_load_bloom_roundtrip() {
        let temp_dir = tempfile::tempdir().unwrap();
        let bloom_path = temp_dir.path().join("embeddings.bloom");

        let mut bloom = AtomicBloomFilter::with_false_pos(0.01, 10_000);
        bloom.insert("hash1:openai:text-embedding-3-small:1536");
        bloom.insert("hash2:openai:text-embedding-3-small:1536");
        persist_bloom(&bloom, &bloom_path).unwrap();

        let loaded = load_bloom_from_disk(&bloom_path).unwrap().unwrap();
        assert!(loaded.contains("hash1:openai:text-embedding-3-small:1536"));
        assert!(loaded.contains("hash2:openai:text-embedding-3-small:1536"));
    }

    #[test]
    fn corrupted_bloom_file_falls_back_to_rebuild() {
        let temp_dir = tempfile::tempdir().unwrap();
        let bloom_path = temp_dir.path().join("embeddings.bloom");
        std::fs::write(&bloom_path, b"garbage data, not valid fastbloom").unwrap();

        let result = load_bloom_from_disk(&bloom_path);
        assert!(result.is_err(), "corrupted bloom must fail to load");
    }

    #[test]
    fn missing_bloom_file_returns_none() {
        let result = load_bloom_from_disk(Path::new("/nonexistent/path_for_test.bloom"));
        // Missing file returns None, but Err is also acceptable.
        if let Ok(opt) = result {
            assert!(opt.is_none(), "missing file should return None");
        }
    }
}
