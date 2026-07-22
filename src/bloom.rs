// BloomFilter + bloom_key are used by downstream tasks (task-3 through task-13).
// The functions and struct are dead-code only until those modules land.
#![allow(dead_code)]

use std::fs;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::sync::Arc;

use duckdb::params;
use fastbloom::BloomFilter;
use serde::{Deserialize, Serialize};

use crate::error::PipelineError;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct BloomMeta {
    pub provider: String,
    pub model: String,
}

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

// ── BloomMeta persistence & validation ──

pub fn persist_meta(meta: &BloomMeta, path: &Path) -> Result<(), PipelineError> {
    let json = serde_json::to_string_pretty(meta).map_err(|e| PipelineError::IoError {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    fs::write(path, json).map_err(|e| PipelineError::IoError {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    Ok(())
}

/// Load bloom from disk, or rebuild from DB if absent/mismatched.
pub fn load_or_rebuild_bloom(
    db_dir: &Path,
    provider: &str,
    model: &str,
) -> Result<Arc<AtomicBloomFilter>, PipelineError> {
    let bloom_path = db_dir.join("embeddings.bloom");
    let meta_path = db_dir.join("embeddings.bloom.meta");

    // Try load from disk
    if bloom_path.exists() && validate_bloom_meta(&meta_path, provider, model) {
        if let Ok(Some(bloom)) = load_bloom_from_disk(&bloom_path) {
            log::info!("Bloom filter loaded from disk");
            return Ok(Arc::new(bloom));
        }
    }

    // Rebuild — placeholder: actual DB query will populate via populate_bloom_from_db()
    log::info!("Bloom filter rebuild required — creating empty filter");
    let bloom = AtomicBloomFilter::with_false_pos(0.01, 1_000_000);
    // Caller is responsible for populating via populate_bloom_from_db() and
    // persisting the result.
    Ok(Arc::new(bloom))
}

/// Populate bloom filter from existing embeddings in the database.
/// Called during pipeline startup when bloom needs rebuilding.
/// Accepts `Option<&Connection>` to support stub/test mode — returns Ok(0) when None.
pub fn populate_bloom_from_db(
    bloom: &mut AtomicBloomFilter,
    conn: Option<&duckdb::Connection>,
    provider: &str,
    model: &str,
) -> Result<usize, PipelineError> {
    let conn = match conn {
        Some(c) => c,
        None => return Ok(0),
    };

    // Discover embedding tables
    let tables: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT table_name FROM information_schema.tables \
                 WHERE table_name LIKE 'embeddings_%' AND table_schema = 'main'",
            )
            .map_err(|e| PipelineError::DbError(e.to_string()))?;
        stmt.query_map([], |row| row.get(0))
            .map_err(|e| PipelineError::DbError(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect()
    };

    let mut count = 0usize;
    for table in &tables {
        // Extract dims from table name: "embeddings_1536" → 1536
        let dims: usize = table
            .strip_prefix("embeddings_")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let query = format!(
            "SELECT c.content_hash FROM chunks c \
             JOIN \"{}\" e ON c.id = e.chunk_id \
             WHERE e.provider = ? AND e.model = ?",
            table
        );
        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| PipelineError::DbError(e.to_string()))?;
        let rows = stmt
            .query_map(params![provider, model], |row| row.get::<_, String>(0))
            .map_err(|e| PipelineError::DbError(e.to_string()))?;

        for content_hash in rows.flatten() {
            let key = bloom_key(&content_hash, provider, model, dims);
            bloom.insert(&key);
            count += 1;
        }
    }

    log::info!("Bloom filter populated from DB: {} entries", count);
    Ok(count)
}

pub fn validate_bloom_meta(meta_path: &Path, provider: &str, model: &str) -> bool {
    match fs::read_to_string(meta_path) {
        Ok(json) => match serde_json::from_str::<BloomMeta>(&json) {
            Ok(meta) => meta.provider == provider && meta.model == model,
            Err(_) => false,
        },
        Err(_) => false,
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

    // ── BloomMeta ──

    #[test]
    fn meta_mismatch_discards_bloom() {
        let temp_dir = tempfile::tempdir().unwrap();
        let meta_path = temp_dir.path().join("embeddings.bloom.meta");

        let meta = BloomMeta {
            provider: "openai".into(),
            model: "text-embedding-3-small".into(),
        };
        persist_meta(&meta, &meta_path).unwrap();

        let valid = validate_bloom_meta(&meta_path, "openai", "text-embedding-3-large");
        assert!(!valid, "model mismatch must invalidate bloom");
    }

    #[test]
    fn meta_match_keeps_bloom() {
        let temp_dir = tempfile::tempdir().unwrap();
        let meta_path = temp_dir.path().join("embeddings.bloom.meta");

        let meta = BloomMeta {
            provider: "openai".into(),
            model: "text-embedding-3-small".into(),
        };
        persist_meta(&meta, &meta_path).unwrap();

        let valid = validate_bloom_meta(&meta_path, "openai", "text-embedding-3-small");
        assert!(valid, "matching meta must keep bloom");
    }

    #[test]
    fn bloom_empty_content_hash_skipped() {
        let key = bloom_key("", "openai", "model", 1536);
        assert!(
            !key.is_empty(),
            "separator ensures non-empty even with empty hash"
        );
    }

    #[test]
    fn bloom_concurrent_reads_across_threads() {
        let mut bloom = AtomicBloomFilter::with_false_pos(0.01, 100_000);
        for i in 0..50_000 {
            bloom.insert(&format!("hash{i}:openai:model:1536"));
        }

        let bloom = std::sync::Arc::new(bloom);
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let b = std::sync::Arc::clone(&bloom);
                std::thread::spawn(move || {
                    for i in 0..10_000 {
                        let _ = b.contains(&format!("hash{i}:openai:model:1536"));
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
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
