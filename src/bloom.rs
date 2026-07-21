// BloomFilter + bloom_key are used by downstream tasks (task-3 through task-13).
// The functions and struct are dead-code only until those modules land.
#![allow(dead_code)]

use fastbloom::BloomFilter;

/// Wrapper providing thread-safe concurrent reads via Arc.
/// Inserts require &mut self (single writer).
pub struct AtomicBloomFilter {
    inner: BloomFilter,
}

impl AtomicBloomFilter {
    pub fn with_false_pos(fp: f64, expected_items: usize) -> Self {
        Self {
            inner: BloomFilter::with_false_pos(fp).expected_items(expected_items),
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
}
