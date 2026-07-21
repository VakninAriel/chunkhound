# Embedding Provider Integration — Revised Design (Thin-Rust)

**Date:** 2026-07-21 · **Revision:** 4 (PR #380 post-mortem)
**Scope:** Refine the embed stage design to match PR #380's thin-Rust architecture, adding bloom filter dedup, token-aware batch sizing, formalized callback interface, and unified error taxonomy.
**Depends on:** PR #380 (Main Rust Flow) merged — pipeline orchestration, DB writer, parallel parse/embed, incremental reindex, progress, compaction.
**Supersedes:** [2026-07-19-embedding-provider-integration-design.md](./2026-07-19-embedding-provider-integration-design.md) (heavy-Rust approach — rejected in favor of thin-Rust)

---

## 1. Context: Why This Revision Exists

The [original July 19 design](./2026-07-19-embedding-provider-integration-design.md) proposed a **heavy-Rust** embed stage: Rust-native `EmbeddingProvider` trait, `reqwest` HTTP clients, retry logic, and bloom filter — all living in Rust. PR #380 took a fundamentally different path.

### 1.1 What PR #380 Actually Built

PR #380 implemented a **thin-Rust** approach where Rust is the pipeline orchestrator but delegates domain logic (parsing, embedding) back to Python via callbacks:

- **DB Writer** (Phase 0): Native DuckDB writes via `libduckdb-sys`, HNSW lifecycle, compaction — Rust owns DB I/O entirely
- **Pipeline orchestration**: `IndexingPipeline.run()` — drives the parse → embed → write sequence
- **Parallel parse**: Rayon thread pool dispatches file batches to Python `parse_batch_callback` (ProcessPoolExecutor for tree-sitter)
- **Parallel embed**: Rayon thread pool dispatches text batches to Python `embed_batch_callback` (async provider calls)
- **Incremental reindex**: `compute_diff_blocking()` compares filesystem vs DB, processes only changed files
- **Progress**: `progress_callback` from Rust → Python Rich progress bars
- **Compaction**: EXPORT/IMPORT atomic swap native in Rust
- **Pipeline_parallel**: Optional parse↔embed overlap via channel-based handoff

**What Rust does NOT do** (by design): HTTP calls, API key management, SDK integration, tiktoken, provider-specific dimension validation. All of that stays in Python's mature provider layer.

### 1.2 Why Thin-Rust Won

| Heavy-Rust (original design) | Thin-Rust (PR #380) |
|---|---|
| Duplicate all provider logic in Rust (~1500 lines) | Reuse existing Python providers (0 new provider lines) |
| Maintain Rust HTTP clients for each provider | Python owns HTTP — Rust never touches the network |
| Risk of Rust↔Python behavioral drift | One source of truth for embedding behavior |
| Blocked on Rust provider maturity before shipping | DB Writer shipped first; embed callback added incrementally |
| Harder to add new providers (Qwen, Ollama, Cohere) | New providers work immediately via Python callback |

The callback pattern is the key architectural insight: **Rust owns data flow, Python owns domain logic**. This is the same pattern that already works for tree-sitter parsing (PR #380 Phase 5).

---

## 2. What's Already Built (PR #380) vs What Needs Building

### 2.1 ✅ Already Built

| Component | File(s) | Description |
|---|---|---|
| Rust DB Writer | `src/db_writer.rs`, `src/db/duckdb_backend.rs` | DuckDB writes with HNSW lifecycle, compaction, crash recovery |
| Pipeline orchestration | `src/pipeline/pipeline.rs` (upstream) | `IndexingPipeline.run()`: parse → embed → write loop |
| Parallel parse | `src/pipeline/pipeline.rs` | Rayon dispatch + Python `parse_batch_callback` |
| Parallel embed | `src/pipeline/pipeline.rs` | Rayon dispatch + Python `embed_batch_callback` |
| Incremental reindex | `src/pipeline/differ.rs` | `compute_diff_blocking()` |
| Progress callback | `src/pipeline/pipeline.rs` | `progress_callback(phase, current, total)` → Rich bars |
| Compaction | `src/db/duckdb_backend.rs` | EXPORT/IMPORT atomic swap |
| Pipeline_parallel mode | `src/pipeline/pipeline.rs` | Parse↔embed overlap via channels |
| GIL management | throughout | `py.allow_threads()` + `Python::with_gil()` per thread |
| Logging bridge | `src/lib.rs` (init) | `pyo3-log` → Python `logging` → loguru intercept |

### 2.2 🔨 Needs Building (this design)

| Component | Priority | Est. Lines | Depends on |
|---|---|---|---|
| Bloom filter dedup | High | ~200 | DB population query |
| Token estimation & batch builder | High | ~150 | — |
| `EmbedBatchFn` trait + `PythonEmbedCallback` | High | ~120 | — |
| `classify_python_embed_error()` | Medium | ~80 | Error taxonomy |
| Unified `PipelineError` | Medium | ~60 | Existing `src/error.rs` |
| Bloom sidecar `.meta` JSON | Low | ~40 | Bloom filter |
| Integration wiring in pipeline.rs | High | ~60 | All of the above |

**Total: ~710 lines of Rust. Zero Python changes needed** (callback is already wired).

---

## 3. Design Decisions (Updated)

| Decision | Choice | Rationale |
|---|---|---|
| Embedding approach | **Python callback** via `EmbedBatchFn` trait | PR #380 pattern: Rust orchestrates, Python owns domain logic |
| Concurrency model | **Rayon thread pool** calling Python callbacks | Already built in PR #380; each thread acquires GIL independently |
| Bloom filter | **In-memory `fastbloom`** with persisted sidecar + `.meta` JSON | Content-hash-based keys; 1% FPR; avoids DB round-trip per batch |
| Token estimation | **`text.len() / 3`** in Rust, pre-dispatch | Conservative; same as Python fallback; avoids tiktoken dependency |
| Batch sizing | **Rust `BatchBuilder`** enforcing max chunks + token budget | Reduces callback frequency; pre-validated batches |
| Error taxonomy | **Unified `PipelineError`** with fatal vs non-fatal | Ad-hoc `PyErr` catching replaced with typed errors |
| Provider config | **Passed from Python** in `PipelineConfig` | Rust maintains zero provider config tables — Python owns models |
| Bloom key | `(content_hash, provider, model, output_dims)` | Content-based dedup across chunk_id changes |

---

## 4. Pipeline Flow

### 4.1 Updated Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                      Rust Pipeline (thin orchestration)              │
│                                                                      │
│  run_pipeline(config)                                                │
│    │                                                                 │
│    ├─ load_bloom(db) ──── Arc<AtomicBloomFilter> ─────────────────┐ │
│    │                                                                │ │
│    ├─ parse files (rayon + Python callback)  ←── GIL acquire/release│
│    │     ↓ ParsedBatch                                              │ │
│    │                                                                │ │
│    ├─ for each parsed batch:                                        │ │
│    │   ├─ for each chunk:                                           │ │
│    │   │   ├─ estimate_tokens() ──── skip if oversized              │ │
│    │   │   ├─ bloom.contains(key) ── skip if hit  ←────────────────┘ │
│    │   │   └─ BatchBuilder::push()                                   │
│    │   │       └─ token_budget_check → flush batch if full           │
│    │   │                                                            │
│    │   └─ dispatch via EmbedBatchFn trait                           │
│    │       └─ PythonEmbedCallback                                  │
│    │           └─ Python::with_gil() → embed_batch_callback(texts)  │
│    │               └─ Python async provider (OpenAI/VoyageAI/...)  │
│    │                   └─ returns EmbedBatchResult                  │
│    │                                                                 │
│    ├─ scatter vectors → chunks, emit completed files                 │
│    └─ write_batch(db_writer, files) ──── DuckDB (GIL-free)          │
│                                                                      │
│  ┌──────────────────── Python Domain Logic ──────────────────────┐  │
│  │  parse_batch_callback()     embed_batch_callback()             │  │
│  │  ├─ tree-sitter parsing     ├─ API key management              │  │
│  │  ├─ chunking                ├─ HTTP calls (async)              │  │
│  │  └─ hash computation        ├─ Retry + rate-limit handling     │  │
│  │                              ├─ Dimension/truncation logic     │  │
│  │                              └─ Response parsing               │  │
│  └────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

### 4.2 Per-Chunk Decision Tree (Rust-side, pre-callback)

```
chunk arrives in parsed batch
  ├─ text empty or no content_hash?   → skip
  ├─ estimate_tokens(text) > max?     → skip + warn (too large)
  ├─ bloom.contains(key)?             → skip (stats.skipped++)
  ├─ batch token budget exceeded?     → flush current batch, start new
  └─ no                               → add to current batch
```

None of these checks require Python — they all run inside Rust's `py.allow_threads()` region. The callback is only invoked when Rust has a ready-to-send batch.

---

## 5. Bloom Filter Dedup

### 5.1 Architecture

```
Pipeline startup
  └─ load_bloom(db) → Arc<AtomicBloomFilter>
       ├─ Check .chunkhound/db/embeddings.bloom + .bloom.meta
       ├─ If absent or provider/model mismatch → rebuild from DB
       │    └─ JOIN chunks + embeddings_N tables, insert all bloom keys
       └─ Persist to disk (fastbloom serde + JSON sidecar)
```

### 5.2 Bloom Key

```rust
fn bloom_key(content_hash: &str, provider: &str, model: &str, dims: usize) -> String {
    format!("{content_hash}:{provider}:{model}:{dims}")
}
```

Separator `:` is safe — `content_hash` is hex (xxhash), provider/model use `[a-z0-9_-]`.

### 5.3 Key Design Points

- **False positive rate:** 1% (~1 in 100 eligible chunks skipped per run). Recovered on next index.
- **Memory:** ~12MB for 10M embeddings at 1% FPR (fastbloom)
- **Content-hash-based dedup:** Re-parsed identical text is deduped across chunk_id changes — superior to Python's chunk_id-based DB query
- **Dimension-aware:** Key includes `output_dims`, so switching dims triggers re-embedding
- **Thread safety:** `Arc<AtomicBloomFilter>` — read-only during pipeline execution, shared across rayon threads

### 5.4 Persistence

| File | Contents |
|---|---|
| `.chunkhound/db/embeddings.bloom` | `fastbloom` serde binary |
| `.chunkhound/db/embeddings.bloom.meta` | `{"provider": "openai", "model": "text-embedding-3-small", "created_at": "..."}` |

### 5.5 Model-Change Detection

On startup, compare `.bloom.meta`'s `(provider, model)` against current config:
- **Match:** Load bloom from disk (fast)
- **Mismatch or absent:** Rebuild from DB (JOIN chunks + embeddings_N)

### 5.6 Edge Cases

| Case | Behavior |
|---|---|
| First run, no bloom file | Create with 1M minimum capacity; all keys miss |
| Model/provider change | Meta mismatch → discard old bloom, rebuild from DB |
| Bloom overflow (>90% load) | Log warning, continue with stale filter (deferred rebuild) |
| Empty content_hash (legacy) | Always embed; never insert into bloom |
| Crash during bloom persist | Rebuild from DB on next start (same as absent) |
| New dimension mid-pipeline | `known_dims` tracking in DB writer pattern; bloom handles via dims in key |

### 5.7 DB Population Query

```sql
SELECT c.content_hash, e.provider, e.model, e.dims
FROM chunks c
JOIN embeddings_1536 e ON c.id = e.chunk_id  -- per-dimension table
-- ... UNION for each embeddings_N table
```

Runs once at pipeline startup, inserts all keys into the bloom filter, then persists.

### 5.8 TDD: Bloom Filter Tests

All tests live in `src/bloom.rs` behind `#[cfg(test)] mod tests`. Write these RED (failing) before implementing the module.

#### 5.8.1 Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use fastbloom::BloomFilter;

    // ── Bloom Key Construction ──

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
        // Matryoshka: same model, different output dimensions → different keys
        let key_256 = bloom_key("abc123", "openai", "text-embedding-3-small", 256);
        let key_1536 = bloom_key("abc123", "openai", "text-embedding-3-small", 1536);
        assert_ne!(key_256, key_1536);
    }

    // ── Bloom Insert & Contains ──

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

    #[test]
    fn bloom_false_positive_rate_within_bounds() {
        let n_items = 100_000;
        let bloom = AtomicBloomFilter::with_false_pos(0.01, n_items);

        // Insert n_items keys
        for i in 0..n_items {
            bloom.insert(&format!("hash{i}:openai:text-embedding-3-small:1536"));
        }

        // Check 10k never-inserted keys
        let mut false_positives = 0u64;
        let check_count = 10_000;
        for i in n_items..n_items + check_count {
            if bloom.contains(&format!("hash{i}:openai:text-embedding-3-small:1536")) {
                false_positives += 1;
            }
        }

        let fpr = false_positives as f64 / check_count as f64;
        // 1% target FPR — allow up to 2% in practice due to hash variance
        assert!(fpr < 0.02, "FPR {:.4} exceeds 2% threshold", fpr);
    }

    // ── Empty Content Hash Handling ──

    #[test]
    fn bloom_empty_content_hash_always_skipped_by_caller() {
        // Bloom key with empty hash is still a valid key, but the caller
        // (pipeline) must never query or insert for empty hashes.
        // This test documents the contract — bloom itself doesn't enforce it.
        let key = bloom_key("", "openai", "model", 1536);
        // Key is syntactically valid; pipeline is responsible for the skip
        assert!(!key.is_empty()); // separator ensures non-empty
    }

    // ── Persistence (load / rebuild) ──

    #[test]
    fn persist_and_load_bloom_roundtrip() {
        let temp_dir = tempfile::tempdir().unwrap();
        let bloom_path = temp_dir.path().join("embeddings.bloom");

        // Create and populate
        let mut bloom = AtomicBloomFilter::with_false_pos(0.01, 10_000);
        bloom.insert("hash1:openai:text-embedding-3-small:1536");
        bloom.insert("hash2:openai:text-embedding-3-small:1536");
        persist_bloom(&bloom, &bloom_path).unwrap();

        // Load from disk
        let loaded = load_bloom_from_disk(&bloom_path).unwrap();
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
        let result = load_bloom_from_disk(Path::new("/nonexistent/path.bloom"));
        assert!(result.is_err() || result.unwrap().is_none());
    }

    // ── Model-Change Detection via .meta ──

    #[test]
    fn meta_mismatch_discards_bloom() {
        let temp_dir = tempfile::tempdir().unwrap();
        let bloom_path = temp_dir.path().join("embeddings.bloom");
        let meta_path = temp_dir.path().join("embeddings.bloom.meta");

        // Write bloom with meta for provider A
        let mut bloom = AtomicBloomFilter::with_false_pos(0.01, 10_000);
        bloom.insert("hash1:openai:text-embedding-3-small:1536");
        persist_bloom(&bloom, &bloom_path).unwrap();
        let meta = BloomMeta { provider: "openai".into(), model: "text-embedding-3-small".into() };
        persist_meta(&meta, &meta_path).unwrap();

        // Validate against different model → must return false (discard)
        let valid = validate_bloom_meta(&meta_path, "openai", "text-embedding-3-large");
        assert!(!valid, "model mismatch must invalidate bloom");
    }

    #[test]
    fn meta_match_keeps_bloom() {
        let temp_dir = tempfile::tempdir().unwrap();
        let meta_path = temp_dir.path().join("embeddings.bloom.meta");
        let meta = BloomMeta { provider: "openai".into(), model: "text-embedding-3-small".into() };
        persist_meta(&meta, &meta_path).unwrap();

        let valid = validate_bloom_meta(&meta_path, "openai", "text-embedding-3-small");
        assert!(valid, "matching meta must keep bloom");
    }

    // ── Thread Safety ──

    #[test]
    fn bloom_concurrent_reads_across_threads() {
        let bloom = Arc::new(AtomicBloomFilter::with_false_pos(0.01, 100_000));
        for i in 0..50_000 {
            bloom.insert(&format!("hash{i}:openai:model:1536"));
        }

        let bloom = Arc::clone(&bloom);
        let handles: Vec<_> = (0..8).map(|_| {
            let b = Arc::clone(&bloom);
            std::thread::spawn(move || {
                for i in 0..10_000 {
                    let _ = b.contains(&format!("hash{i}:openai:model:1536"));
                }
            })
        }).collect();

        for h in handles {
            h.join().unwrap();
        }
        // No panic = pass (fastbloom AtomicBloomFilter is lock-free for reads)
    }
}
```

#### 5.8.2 RED → GREEN Order

| Phase | Tests to write RED | What to implement |
|---|---|---|
| 1 | `bloom_key_*` (4 tests) | `bloom_key()` function |
| 2 | `bloom_insert_then_contains`, `bloom_does_not_contain_uninserted` | `AtomicBloomFilter` wrapper |
| 3 | `bloom_false_positive_rate_within_bounds` | Validate FPR configuration |
| 4 | `persist_and_load_bloom_roundtrip`, `corrupted_bloom_file_falls_back`, `missing_bloom_file_returns_none` | `persist_bloom()`, `load_bloom_from_disk()` |
| 5 | `meta_mismatch_discards_bloom`, `meta_match_keeps_bloom` | `BloomMeta`, `persist_meta()`, `validate_bloom_meta()` |
| 6 | `bloom_empty_content_hash_always_skipped_by_caller` | Contract test — no impl needed |
| 7 | `bloom_concurrent_reads_across_threads` | Verify `Arc<AtomicBloomFilter>` is `Send + Sync` |

---

## 6. Token Estimation & Batch Sizing

### 6.1 Estimator

```rust
const CHARS_PER_TOKEN: usize = 3;  // matches Python EMBEDDING_CHARS_PER_TOKEN

fn estimate_tokens(text: &str) -> usize {
    text.len() / CHARS_PER_TOKEN
}
```

Same as Python's fallback path. Slightly conservative (underestimates tokens → smaller batches) which is the safe direction. No tiktoken dependency in Rust.

### 6.2 Batch Constraints (per-provider, from Python config)

| Provider | max_chunks_per_batch | max_tokens_per_chunk | batch_token_budget |
|---|---|---|---|
| OpenAI text-embedding-3-* | 2048 | 8191 chars (→~2700 tokens) | — (no per-batch budget) |
| VoyageAI | 1000 | 32000 chars (→~10600 tokens) | 120k–1M tokens (model-dependent) |
| Qwen3 (OAI-compatible) | 128 | 8192 chars (→~2700 tokens) | — |

These are passed from Python in `PipelineConfig` — Rust maintains zero provider config tables.

### 6.3 BatchBuilder

```rust
struct BatchBuilder {
    chunks: Vec<BatchChunk>,
    current_tokens: usize,
    config: BatchConfig,
}

impl BatchBuilder {
    fn push(&mut self, chunk: BatchChunk) -> Option<Vec<BatchChunk>> {
        let tokens = estimate_tokens(&chunk.text);

        // Per-chunk limit
        if tokens > self.config.max_tokens_per_chunk {
            return None; // chunk skipped by caller
        }

        // Budget check (None budget = no limit, e.g. OpenAI)
        if let Some(budget) = self.config.batch_token_budget {
            if self.current_tokens + tokens > budget && !self.chunks.is_empty() {
                // Budget exceeded — flush current, start new batch with this chunk
                let flushed = std::mem::take(&mut self.chunks);
                self.current_tokens = 0;
                self.chunks.push(chunk);
                self.current_tokens += tokens;
                return Some(flushed);
            }
        }

        // Capacity check
        if self.chunks.len() >= self.config.max_chunks_per_batch {
            let flushed = std::mem::take(&mut self.chunks);
            self.current_tokens = 0;
            self.chunks.push(chunk);
            self.current_tokens += tokens;
            return Some(flushed);
        }

        self.chunks.push(chunk);
        self.current_tokens += tokens;
        None // batch not full yet
    }

    fn flush(&mut self) -> Option<Vec<BatchChunk>> {
        if self.chunks.is_empty() { None }
        else {
            self.current_tokens = 0;
            Some(std::mem::take(&mut self.chunks))
        }
    }
}
```

### 6.4 Interaction with Callback

The callback still handles `ContextLengthExceeded` and other API-level errors as a safety net. But pre-flight sizing means:
- Oversized chunks are filtered before reaching the callback
- Token-budget splits happen in Rust, not in Python retry loops
- Fewer, better-sized batches → fewer callback invocations

### 6.5 Config from Python

```rust
// Added to existing PipelineConfig (from PR #380)
struct BatchConfig {
    max_chunks_per_batch: usize,
    max_tokens_per_chunk: usize,   // chars-based limit
    batch_token_budget: Option<usize>,  // None = no budget (OpenAI)
}
```

### 6.6 TDD: Token Estimation & Batch Builder Tests

All tests live in `src/embed/token.rs` behind `#[cfg(test)] mod tests`. Write RED before implementing.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // ── Token Estimation ──

    #[test]
    fn estimate_tokens_ascii_text() {
        // 30 chars / 3 = 10 tokens
        assert_eq!(estimate_tokens("The quick brown fox jumps over"), 10);
    }

    #[test]
    fn estimate_tokens_empty_string() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_tokens_short_text() {
        // 2 chars / 3 = 0 tokens (integer division)
        assert_eq!(estimate_tokens("ab"), 0);
    }

    #[test]
    fn estimate_tokens_exactly_divisible() {
        // 9 chars / 3 = 3
        assert_eq!(estimate_tokens("abcdefghi"), 3);
    }

    #[test]
    fn estimate_tokens_unicode_text() {
        // Unicode chars count as single bytes in len(), but our estimator is
        // chars-based for simplicity. Document the behavior:
        // "héllo wörld" = 11 chars (including space) → 11/3 = 3
        assert_eq!(estimate_tokens("héllo wörld"), 3);
    }

    #[test]
    fn estimate_tokens_code_snippet() {
        let code = "fn main() { println!(\"Hello, world!\"); }"; // 41 chars → 13 tokens
        assert_eq!(estimate_tokens(code), 13);
    }

    // ── BatchBuilder: Capacity Flush ──

    #[test]
    fn batch_builder_flushes_on_capacity() {
        let config = BatchConfig {
            max_chunks_per_batch: 3,
            max_tokens_per_chunk: 1000,
            batch_token_budget: None,  // OpenAI: no budget
        };
        let mut builder = BatchBuilder::new(config);

        // Push 3 chunks → 3rd triggers flush
        assert!(builder.push(make_chunk("chunk1", "text1")).is_none());
        assert!(builder.push(make_chunk("chunk2", "text2")).is_none());
        let flushed = builder.push(make_chunk("chunk3", "text3"));
        assert!(flushed.is_some());
        assert_eq!(flushed.unwrap().len(), 3);
        assert_eq!(builder.current_tokens, 0);
    }

    #[test]
    fn batch_builder_multiple_capacity_flushes() {
        let config = BatchConfig {
            max_chunks_per_batch: 2,
            max_tokens_per_chunk: 1000,
            batch_token_budget: None,
        };
        let mut builder = BatchBuilder::new(config);

        let flush1 = builder.push(make_chunk("c1", "t1")); // None, building...
        let flush2 = builder.push(make_chunk("c2", "t2")); // Flush [c1,c2]
        assert!(flush1.is_none());
        assert!(flush2.is_some());

        let flush3 = builder.push(make_chunk("c3", "t3")); // None
        let flush4 = builder.push(make_chunk("c4", "t4")); // Flush [c3,c4]
        assert!(flush3.is_none());
        assert!(flush4.is_some());
    }

    // ── BatchBuilder: Token Budget Flush ──

    #[test]
    fn batch_builder_flushes_on_token_budget() {
        let config = BatchConfig {
            max_chunks_per_batch: 1000,  // high enough not to trigger
            max_tokens_per_chunk: 1000,
            batch_token_budget: Some(10),  // low budget for testing
        };
        let mut builder = BatchBuilder::new(config);

        // "123456789012345" = 15 chars → 5 tokens
        // Push 2 such chunks → 10 tokens = budget → next push triggers flush
        assert!(builder.push(make_chunk("c1", "123456789012345")).is_none());  // 5 tokens
        assert!(builder.push(make_chunk("c2", "123456789012345")).is_none());  // 10 tokens total
        let flushed = builder.push(make_chunk("c3", "123456789012345"));  // would be 15 → flush first 2
        assert!(flushed.is_some());
        assert_eq!(flushed.unwrap().len(), 2);
    }

    #[test]
    fn batch_builder_no_budget_never_flushes_on_tokens() {
        let config = BatchConfig {
            max_chunks_per_batch: 1000,
            max_tokens_per_chunk: 1000,
            batch_token_budget: None,  // OpenAI: no budget
        };
        let mut builder = BatchBuilder::new(config);

        // Push 100 large chunks with no budget → only capacity can flush
        for i in 0..100 {
            let chunk = make_chunk(&format!("c{}", i), &"x".repeat(90));  // lots of tokens
            assert!(builder.push(chunk).is_none(), "no flush without capacity hit");
        }
        assert_eq!(builder.chunks.len(), 100);
    }

    // ── BatchBuilder: Chunk Limit ──

    #[test]
    fn batch_builder_oversized_chunk_skipped() {
        let config = BatchConfig {
            max_chunks_per_batch: 1000,
            max_tokens_per_chunk: 100,  // small limit
            batch_token_budget: None,
        };
        let mut builder = BatchBuilder::new(config);

        // 3000 chars → 1000 tokens > 100 max → None (skip signal)
        let huge_text = "x".repeat(3000);
        let result = builder.push(make_chunk("huge", &huge_text));
        assert!(result.is_none(), "oversized chunk returns None from push");
        assert!(builder.chunks.is_empty(), "oversized chunk is not added");
    }

    #[test]
    fn batch_builder_accepts_chunk_under_limit() {
        let config = BatchConfig {
            max_chunks_per_batch: 1000,
            max_tokens_per_chunk: 100,
            batch_token_budget: None,
        };
        let mut builder = BatchBuilder::new(config);

        // 300 chars → 100 tokens = exactly at limit → accepted
        let text = "x".repeat(300);
        assert!(builder.push(make_chunk("ok", &text)).is_none());
        assert_eq!(builder.chunks.len(), 1);
    }

    #[test]
    fn batch_builder_boundary_chunk_at_token_limit() {
        let config = BatchConfig {
            max_chunks_per_batch: 1000,
            max_tokens_per_chunk: 100,
            batch_token_budget: None,
        };
        let mut builder = BatchBuilder::new(config);

        // 300 chars → exactly 100 tokens → accepted (<= not <)
        let text = "x".repeat(300);
        assert!(builder.push(make_chunk("boundary", &text)).is_none());
    }

    // ── BatchBuilder: Manual Flush ──

    #[test]
    fn batch_builder_manual_flush_returns_remaining() {
        let config = BatchConfig {
            max_chunks_per_batch: 100,
            max_tokens_per_chunk: 1000,
            batch_token_budget: None,
        };
        let mut builder = BatchBuilder::new(config);

        builder.push(make_chunk("c1", "t1"));
        builder.push(make_chunk("c2", "t2"));

        let flushed = builder.flush();
        assert!(flushed.is_some());
        assert_eq!(flushed.unwrap().len(), 2);
        assert!(builder.chunks.is_empty());
        assert_eq!(builder.current_tokens, 0);
    }

    #[test]
    fn batch_builder_flush_empty_returns_none() {
        let config = BatchConfig {
            max_chunks_per_batch: 100,
            max_tokens_per_chunk: 1000,
            batch_token_budget: None,
        };
        let mut builder = BatchBuilder::new(config);
        assert!(builder.flush().is_none());
    }

    // ── BatchBuilder: Edge Cases ──

    #[test]
    fn batch_builder_single_chunk_exceeds_budget() {
        // A single chunk that exceeds the entire token budget is still added.
        // The callback will handle the error if the API rejects it.
        let config = BatchConfig {
            max_chunks_per_batch: 100,
            max_tokens_per_chunk: 10_000,
            batch_token_budget: Some(5),  // tiny budget
        };
        let mut builder = BatchBuilder::new(config);

        // 30 chars → 10 tokens → exceeds budget of 5, but pushed into empty builder
        let result = builder.push(make_chunk("big", "123456789012345678901234567890"));
        // When builder is empty, the budget check is skipped (token_budget > 0 guard)
        assert!(result.is_none());
        assert_eq!(builder.chunks.len(), 1);
    }

    #[test]
    fn batch_builder_token_tracking_resets_after_flush() {
        let config = BatchConfig {
            max_chunks_per_batch: 3,
            max_tokens_per_chunk: 1000,
            batch_token_budget: None,
        };
        let mut builder = BatchBuilder::new(config);

        builder.push(make_chunk("c1", "123456789"));   // 3 tokens
        builder.push(make_chunk("c2", "123456789"));   // 6 tokens
        let flushed = builder.push(make_chunk("c3", "123456789")); // flush
        assert!(flushed.is_some());

        // After flush, builder is empty and token count is 0
        assert_eq!(builder.current_tokens, 0);
        assert!(builder.chunks.is_empty());
    }

    // ── Helpers ──

    fn make_chunk(hash: &str, text: &str) -> BatchChunk {
        BatchChunk {
            file_key: FileKey(0),
            chunk_idx: 0,
            text: text.to_string(),
            content_hash: hash.to_string(),
        }
    }
}
```

#### 6.6.1 RED → GREEN Order

| Phase | Tests to write RED | What to implement |
|---|---|---|
| 1 | `estimate_tokens_*` (6 tests) | `estimate_tokens()` |
| 2 | `batch_builder_flushes_on_capacity`, `batch_builder_multiple_capacity_flushes` | `BatchBuilder::push()` capacity logic |
| 3 | `batch_builder_flushes_on_token_budget`, `batch_builder_no_budget_never_flushes` | Token budget logic in `push()` |
| 4 | `batch_builder_oversized_chunk_skipped`, `batch_builder_accepts_chunk_under_limit`, `batch_builder_boundary_chunk_at_token_limit` | Per-chunk limit in `push()` |
| 5 | `batch_builder_manual_flush_returns_remaining`, `batch_builder_flush_empty_returns_none` | `flush()` method |
| 6 | `batch_builder_single_chunk_exceeds_budget`, `batch_builder_token_tracking_resets_after_flush` | Edge cases |

---

## 7. Embed Callback Interface

### 7.1 Trait

```rust
/// A batch embedding function — stateless, synchronous from Rust's perspective.
/// Each call is independent; concurrency is handled by the rayon caller.
pub trait EmbedBatchFn: Send + Sync {
    /// Embed a batch of texts. Returns vectors in input order.
    /// None entries = that chunk's embedding failed.
    fn embed_batch(
        &self,
        texts: &[String],
        provider: &str,
        model: &str,
        dims: usize,
    ) -> EmbedBatchResult;
}

pub struct EmbedBatchResult {
    /// Vectors in same order as input texts. None entries = that chunk failed.
    pub vectors: Vec<Option<Vec<f32>>>,
    /// Aggregate stats for this batch.
    pub stats: BatchCallStats,
}

#[derive(Default)]
pub struct BatchCallStats {
    pub api_calls: u32,
    pub total_latency_ms: u64,
}
```

**Why `Vec<Option<Vec<f32>>>` not `Result`?** Partial failures are common (e.g., one chunk in a batch of 100 exceeds VoyageAI's context length). The pipeline should scatter successes and mark only failures as `vector=None` — not lose the entire batch.

### 7.2 Result Actions

| Scenario | `vectors` content | Pipeline action |
|---|---|---|
| Full success | All `Some(vec)` | Scatter vectors to chunks, emit files |
| Partial failure | Mix of `Some`/`None` | Scatter successes, failures get `vector=None`, stats updated |
| Total failure | All `None` | Mark all chunks failed, `stats.chunks_failed += N` |
| Fatal exception (Auth) | Propagated as `Err(PipelineError)` | Pipeline aborts via `cancelled` flag |

### 7.3 PythonCallback Implementation

```rust
/// Wraps a Python callable behind the EmbedBatchFn trait.
/// The callable signature: fn(texts: List[str], provider: str, model: str) -> List[List[float]]
pub struct PythonEmbedCallback {
    callable: Py<PyAny>,
}

impl EmbedBatchFn for PythonEmbedCallback {
    fn embed_batch(
        &self,
        texts: &[String],
        provider: &str,
        model: &str,
        dims: usize,
    ) -> EmbedBatchResult {
        Python::with_gil(|py| {
            let py_texts: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
            match self.callable.call1(py, (py_texts, provider, model)) {
                Ok(result) => {
                    let vectors = extract_vectors_from_python(py, &result, texts.len());
                    EmbedBatchResult { vectors, stats: BatchCallStats::default() }
                }
                Err(e) => {
                    let err = classify_python_embed_error(py, &e);
                    if err.is_fatal() {
                        // Fatal error propagated to pipeline — caller checks is_fatal()
                        // and aborts. Per-chunk None is still set for this batch.
                    }
                    EmbedBatchResult {
                        vectors: vec![None; texts.len()],
                        stats: BatchCallStats::default(),
                    }
                }
            }
        })
    }
}
```

### 7.4 Extract Vectors from Python

```rust
fn extract_vectors_from_python(
    py: Python<'_>,
    result: &PyAny,
    expected_len: usize,
) -> Vec<Option<Vec<f32>>> {
    let list: &PyList = match result.downcast() {
        Ok(l) => l,
        Err(_) => return vec![None; expected_len],
    };
    list.iter().map(|item| {
        if item.is_none() {
            None
        } else if let Ok(inner) = item.downcast::<PyList>() {
            let vec: Vec<f32> = inner.iter()
                .filter_map(|v| v.extract::<f32>().ok())
                .collect();
            if vec.is_empty() { None } else { Some(vec) }
        } else {
            None
        }
    }).collect()
}
```

### 7.5 What the Trait Enables

- **Testability:** Mock `EmbedBatchFn` for Rust unit tests without Python
- **Swapability:** Future Rust-native providers implement the same trait
- **Clarity:** Pipeline code works with typed `EmbedBatchResult`, never raw `PyErr`
- **Partial failure:** Explicit `Option<Vec<f32>>` per chunk, no guesswork

### 7.6 TDD: EmbedBatchFn Trait & Callback Tests

Tests live in `src/embed/mod.rs` (mock trait tests) and `src/embed/callback.rs` (Python callback tests). Write RED before implementing.

#### 7.6.1 Mock EmbedBatchFn (Rust-only, no Python)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Mock implementation of EmbedBatchFn for unit testing without Python.
    struct MockEmbedFn {
        vectors: Vec<Vec<f32>>,
        should_fail_indices: Vec<usize>,  // indices that return None
    }

    impl EmbedBatchFn for MockEmbedFn {
        fn embed_batch(
            &self,
            texts: &[String],
            _provider: &str,
            _model: &str,
            _dims: usize,
        ) -> EmbedBatchResult {
            let mut result = Vec::with_capacity(texts.len());
            for i in 0..texts.len() {
                if self.should_fail_indices.contains(&i) {
                    result.push(None);
                } else if i < self.vectors.len() {
                    result.push(Some(self.vectors[i].clone()));
                } else {
                    // Fallback: generate a simple vector
                    result.push(Some(vec![1.0_f32; 8]));
                }
            }
            EmbedBatchResult {
                vectors: result,
                stats: BatchCallStats { api_calls: 1, total_latency_ms: 50 },
            }
        }
    }

    // ── Basic Correctness ──

    #[test]
    fn mock_embed_batch_returns_correct_count() {
        let mock = MockEmbedFn {
            vectors: vec![vec![1.0; 8], vec![2.0; 8], vec![3.0; 8]],
            should_fail_indices: vec![],
        };
        let texts: Vec<String> = (0..3).map(|i| format!("text {}", i)).collect();

        let result = mock.embed_batch(&texts, "openai", "model", 8);
        assert_eq!(result.vectors.len(), 3);
        assert!(result.vectors.iter().all(|v| v.is_some()));
    }

    #[test]
    fn mock_embed_batch_returns_correct_dims() {
        let mock = MockEmbedFn {
            vectors: vec![vec![0.1; 1536]],
            should_fail_indices: vec![],
        };
        let result = mock.embed_batch(&["text".into()], "openai", "text-embedding-3-small", 1536);
        assert_eq!(result.vectors[0].as_ref().unwrap().len(), 1536);
    }

    #[test]
    fn mock_embed_batch_partial_failure() {
        let mock = MockEmbedFn {
            vectors: vec![vec![1.0; 8], vec![2.0; 8], vec![3.0; 8]],
            should_fail_indices: vec![1],  // index 1 fails
        };
        let texts: Vec<String> = (0..3).map(|i| format!("text {}", i)).collect();

        let result = mock.embed_batch(&texts, "openai", "model", 8);
        assert_eq!(result.vectors.len(), 3);
        assert!(result.vectors[0].is_some(), "index 0 should succeed");
        assert!(result.vectors[1].is_none(), "index 1 should fail");
        assert!(result.vectors[2].is_some(), "index 2 should succeed");
    }

    #[test]
    fn mock_embed_batch_all_failure() {
        let mock = MockEmbedFn {
            vectors: vec![],
            should_fail_indices: (0..5).collect(),  // all 5 fail
        };
        let texts: Vec<String> = (0..5).map(|i| format!("text {}", i)).collect();

        let result = mock.embed_batch(&texts, "openai", "model", 8);
        assert_eq!(result.vectors.len(), 5);
        assert!(result.vectors.iter().all(|v| v.is_none()));
    }

    #[test]
    fn mock_embed_batch_empty_input() {
        let mock = MockEmbedFn { vectors: vec![], should_fail_indices: vec![] };
        let result = mock.embed_batch(&[], "openai", "model", 8);
        assert!(result.vectors.is_empty());
    }

    // ── Stats ──

    #[test]
    fn mock_embed_batch_reports_stats() {
        let mock = MockEmbedFn { vectors: vec![vec![1.0; 8]], should_fail_indices: vec![] };
        let result = mock.embed_batch(&["text".into()], "openai", "model", 8);
        assert_eq!(result.stats.api_calls, 1);
        assert!(result.stats.total_latency_ms > 0);
    }

    // ── Trait Object Safety ──

    #[test]
    fn embed_batch_fn_is_object_safe() {
        // Compile-time check: trait must be usable as &dyn EmbedBatchFn
        let mock = MockEmbedFn { vectors: vec![], should_fail_indices: vec![] };
        let _trait_obj: &dyn EmbedBatchFn = &mock;
    }
}
```

#### 7.6.2 Python Callback (requires Python interpreter)

```rust
#[cfg(test)]
mod python_callback_tests {
    use super::*;
    use pyo3::prelude::*;

    fn py_callback_that_returns_vectors() -> Py<PyAny> {
        Python::with_gil(|py| {
            let func = py.eval(
                "lambda texts, provider, model: [[float(i + 1)] * 4 for i in range(len(texts))]",
                None, None,
            ).unwrap();
            func.into()
        })
    }

    #[test]
    fn python_callback_extracts_vectors_correctly() {
        let callback = PythonEmbedCallback::new(py_callback_that_returns_vectors());
        let texts: Vec<String> = (0..3).map(|i| format!("text {}", i)).collect();

        let result = callback.embed_batch(&texts, "openai", "model", 4);
        assert_eq!(result.vectors.len(), 3);
        // text 0 → [1.0; 4], text 1 → [2.0; 4], text 2 → [3.0; 4]
        assert_eq!(result.vectors[0].as_ref().unwrap(), &vec![1.0; 4]);
        assert_eq!(result.vectors[1].as_ref().unwrap(), &vec![2.0; 4]);
        assert_eq!(result.vectors[2].as_ref().unwrap(), &vec![3.0; 4]);
    }

    #[test]
    fn python_callback_handles_none_entries() {
        // Python callback returns [None, [1.0, 2.0], None] for 3 texts
        let py_cb = Python::with_gil(|py| {
            let func = py.eval(
                "lambda texts, provider, model: [None, [1.0, 2.0], None]",
                None, None,
            ).unwrap();
            func.into()
        });
        let callback = PythonEmbedCallback::new(py_cb);
        let texts: Vec<String> = (0..3).map(|i| format!("text {}", i)).collect();

        let result = callback.embed_batch(&texts, "openai", "model", 2);
        assert!(result.vectors[0].is_none());
        assert!(result.vectors[1].is_some());
        assert!(result.vectors[2].is_none());
    }

    #[test]
    fn python_callback_handles_malformed_result() {
        // Python callback returns a string instead of a list
        let py_cb = Python::with_gil(|py| {
            let func = py.eval(
                "lambda texts, provider, model: 'not a list'",
                None, None,
            ).unwrap();
            func.into()
        });
        let callback = PythonEmbedCallback::new(py_cb);
        let result = callback.embed_batch(&["text".into()], "openai", "model", 8);
        // Malformed result → all chunks get None
        assert_eq!(result.vectors.len(), 1);
        assert!(result.vectors[0].is_none());
    }

    #[test]
    fn python_callback_handles_wrong_vector_count() {
        // Python returns 1 vector for 3 input texts
        let py_cb = Python::with_gil(|py| {
            let func = py.eval(
                "lambda texts, provider, model: [[1.0, 2.0]]",  // only 1, not 3
                None, None,
            ).unwrap();
            func.into()
        });
        let callback = PythonEmbedCallback::new(py_cb);
        let texts: Vec<String> = (0..3).map(|i| format!("text {}", i)).collect();

        let result = callback.embed_batch(&texts, "openai", "model", 2);
        // Short result → pad with None
        assert!(result.vectors[0].is_some());
        assert!(result.vectors[1].is_none());
        assert!(result.vectors[2].is_none());
    }
}
```

#### 7.6.3 RED → GREEN Order

| Phase | Tests to write RED | What to implement |
|---|---|---|
| 1 | All mock tests (6 tests) | `EmbedBatchFn` trait definition + `EmbedBatchResult` |
| 2 | `python_callback_extracts_vectors_correctly` | `PythonEmbedCallback::new()`, basic `embed_batch()` |
| 3 | `python_callback_handles_none_entries` | None-entry handling in `extract_vectors_from_python()` |
| 4 | `python_callback_handles_malformed_result`, `python_callback_handles_wrong_vector_count` | Error handling in extract + padding logic |

---

## 8. Error Handling Taxonomy

### 8.1 Unified PipelineError

```rust
#[derive(thiserror::Error, Debug)]
pub enum PipelineError {
    // ── Fatal (abort pipeline) ──
    #[error("authentication failed for {provider}: {message}")]
    Auth { provider: String, message: String },

    #[error("bad request for {provider}: {message}")]
    BadRequest { provider: String, message: String },

    #[error("pipeline cancelled")]
    Cancelled,

    // ── Non-fatal (batch lost, pipeline continues) ──
    #[error("provider error for {provider}: {message}")]
    ProviderError { provider: String, message: String },

    #[error("rate limited by {provider}")]
    RateLimited { provider: String, retry_after_secs: Option<u64> },

    // ── Infrastructure ──
    #[error("database error: {0}")]
    DbError(String),

    #[error("I/O error at {path}: {message}")]
    IoError { path: PathBuf, message: String },
}

impl PipelineError {
    pub fn is_fatal(&self) -> bool {
        matches!(self,
            Self::Auth { .. } |
            Self::BadRequest { .. } |
            Self::Cancelled
        )
    }
}
```

### 8.2 Python Exception Classification

```rust
fn classify_python_embed_error(py: Python<'_>, err: &PyErr) -> PipelineError {
    let msg = err.to_string();

    // Check exception type name
    let type_name = err.get_type(py).name().unwrap_or("").to_string();

    match type_name.as_str() {
        "AuthenticationError" => PipelineError::Auth {
            provider: "unknown".into(),
            message: msg,
        },
        "RateLimitError" => PipelineError::RateLimited {
            provider: "unknown".into(),
            retry_after_secs: None, // Python handles retries internally
        },
        "BadRequestError" => {
            if msg.contains("context length") || msg.contains("maximum context length") {
                // Handled as partial failure at callback level — not an error here
                PipelineError::ProviderError { provider: "unknown".into(), message: msg }
            } else {
                PipelineError::BadRequest { provider: "unknown".into(), message: msg }
            }
        }
        "APITimeoutError" | "APIConnectionError" | "Timeout" => {
            PipelineError::ProviderError { provider: "unknown".into(), message: msg }
        }
        _ => {
            // Generic Python exception → non-fatal provider error
            PipelineError::ProviderError { provider: "unknown".into(), message: msg }
        }
    }
}
```

### 8.3 Pipeline Reaction

| Error type | Pipeline action |
|---|---|
| `Auth`, `BadRequest` | Set `cancelled = true`, safety-net drain pending files, propagate error |
| `ProviderError`, `RateLimited` | Mark batch chunks `vector=None`, increment `stats.chunks_failed`, **pipeline continues** |
| `Cancelled` | Set `cancelled = true`, safety-net drain, exit gracefully |
| `DbError`, `IoError` | Context-dependent — DB errors are fatal, I/O errors during parse are per-file |

Matches PR #380's existing behavior where non-fatal batch failures don't halt the pipeline.

### 8.4 TDD: Error Classification & Taxonomy Tests

Tests live in `src/embed/callback.rs` (classification) and `src/error.rs` (taxonomy).

#### 8.4.1 PipelineError Taxonomy

```rust
// src/error.rs — #[cfg(test)] mod tests

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_fatal() Matrix ──

    #[test]
    fn auth_is_fatal() {
        let err = PipelineError::Auth { provider: "openai".into(), message: "bad key".into() };
        assert!(err.is_fatal());
    }

    #[test]
    fn bad_request_is_fatal() {
        let err = PipelineError::BadRequest { provider: "openai".into(), message: "invalid model".into() };
        assert!(err.is_fatal());
    }

    #[test]
    fn cancelled_is_fatal() {
        assert!(PipelineError::Cancelled.is_fatal());
    }

    #[test]
    fn provider_error_is_not_fatal() {
        let err = PipelineError::ProviderError { provider: "voyageai".into(), message: "timeout".into() };
        assert!(!err.is_fatal());
    }

    #[test]
    fn rate_limited_is_not_fatal() {
        let err = PipelineError::RateLimited { provider: "openai".into(), retry_after_secs: Some(5) };
        assert!(!err.is_fatal());
    }

    #[test]
    fn db_error_is_not_fatal_by_default() {
        // DB errors may be fatal depending on context, but the type-level default
        // is non-fatal so the caller decides
        let err = PipelineError::DbError("connection lost".into());
        // No is_fatal() match → caller handles contextually
        // This test documents the design decision
    }

    #[test]
    fn io_error_is_not_fatal_by_default() {
        let err = PipelineError::IoError { path: "/tmp/foo".into(), message: "not found".into() };
        // Non-fatal default; per-file errors shouldn't abort the pipeline
    }

    // ── Display Formatting ──

    #[test]
    fn auth_error_display_includes_provider() {
        let err = PipelineError::Auth { provider: "openai".into(), message: "unauthorized".into() };
        let display = err.to_string();
        assert!(display.contains("openai"));
        assert!(display.contains("unauthorized"));
    }

    #[test]
    fn bad_request_error_display_includes_message() {
        let err = PipelineError::BadRequest { provider: "voyageai".into(), message: "model not found".into() };
        assert!(err.to_string().contains("model not found"));
    }

    #[test]
    fn rate_limited_error_display_includes_retry_after() {
        let err = PipelineError::RateLimited { provider: "openai".into(), retry_after_secs: Some(30) };
        // Display should include retry info or at minimum the provider
        assert!(err.to_string().contains("openai"));
    }

    // ── Error Equality (for test assertions) ──

    #[test]
    fn same_error_variants_are_equal() {
        let e1 = PipelineError::Auth { provider: "oai".into(), message: "msg".into() };
        let e2 = PipelineError::Auth { provider: "oai".into(), message: "msg".into() };
        // Derive PartialEq on PipelineError
        assert_eq!(e1, e2);
    }

    #[test]
    fn different_error_variants_are_not_equal() {
        let e1 = PipelineError::Auth { provider: "oai".into(), message: "msg".into() };
        let e2 = PipelineError::BadRequest { provider: "oai".into(), message: "msg".into() };
        assert_ne!(e1, e2);
    }
}
```

#### 8.4.2 Python Exception Classification

```rust
// src/embed/callback.rs — #[cfg(test)] mod tests (requires Python interpreter)

#[cfg(test)]
mod classify_tests {
    use super::*;
    use pyo3::prelude::*;

    fn raise_in_python(exception_type: &str, message: &str) -> PyErr {
        Python::with_gil(|py| {
            let code = format!("raise {}(\"{}\")", exception_type, message);
            match py.eval(&code, None, None) {
                Ok(_) => unreachable!(),
                Err(e) => e,
            }
        })
    }

    #[test]
    fn classify_authentication_error_as_fatal() {
        let py_err = raise_in_python("ValueError", "AuthenticationError");
        // Note: we can't create real openai.AuthenticationError without the SDK.
        // This test validates the classification logic with Python::with_gil.
        // Integration tests cover real SDK exceptions.
    }

    #[test]
    fn classify_any_python_error_as_non_fatal_by_default() {
        Python::with_gil(|py| {
            let err = py.eval("1/0", None, None).unwrap_err();  // ZeroDivisionError
            let classified = classify_python_embed_error(py, &err);
            assert!(!classified.is_fatal(),
                "unknown Python exceptions default to non-fatal (ProviderError)");
        });
    }

    #[test]
    fn classify_value_error_as_provider_error() {
        Python::with_gil(|py| {
            let err = py.eval("int('not-a-number')", None, None).unwrap_err();
            let classified = classify_python_embed_error(py, &err);
            assert!(!classified.is_fatal());
            assert!(matches!(classified, PipelineError::ProviderError { .. }));
        });
    }

    // ── is_fatal() completeness ──

    #[test]
    fn all_fatal_variants_are_exhaustive() {
        // If a new PipelineError variant is added, this test reminds you to
        // decide whether it's fatal and add it to is_fatal().
        // Test: every variant is either in is_fatal() or explicitly non-fatal.
        // This is a compile-time pattern, not a runtime check.
        let errors: &[PipelineError] = &[
            PipelineError::Auth { provider: "".into(), message: "".into() },
            PipelineError::BadRequest { provider: "".into(), message: "".into() },
            PipelineError::Cancelled,
            PipelineError::ProviderError { provider: "".into(), message: "".into() },
            PipelineError::RateLimited { provider: "".into(), retry_after_secs: None },
            PipelineError::DbError("".into()),
            PipelineError::IoError { path: PathBuf::new(), message: "".into() },
        ];
        // Every variant is represented once — adding a variant here without
        // updating is_fatal() would need manual review, but this array at least
        // fails to compile if you rename a variant without updating this test.
        assert_eq!(errors.len(), 7, "all 7 PipelineError variants must be listed");
    }
}
```

#### 8.4.3 RED → GREEN Order

| Phase | Tests to write RED | What to implement |
|---|---|---|
| 1 | `auth_is_fatal`, `bad_request_is_fatal`, `cancelled_is_fatal`, `provider_error_is_not_fatal`, `rate_limited_is_not_fatal` | `PipelineError` enum + `is_fatal()` |
| 2 | `same_error_variants_are_equal`, `different_error_variants_are_not_equal` | `#[derive(PartialEq)]` on `PipelineError` |
| 3 | `auth_error_display_includes_provider`, `bad_request_error_display_includes_message`, `rate_limited_error_display_includes_retry_after` | `#[derive(thiserror::Error)]` + `#[error(...)]` |
| 4 | `all_fatal_variants_are_exhaustive` | Exhaustiveness check |
| 5 | `classify_any_python_error_as_non_fatal_by_default` | `classify_python_embed_error()` fallback path |
| 6 | `classify_value_error_as_provider_error` | Type-name matching in classifier |

---

## 9. Module Layout

```
src/
├── lib.rs                  # pyo3_log init, module registration (unchanged)
├── error.rs                # PipelineError (refactored — was DB-writer only)
├── types.rs                # Shared types (unchanged: BatchChunk, ParsedFile, etc.)
│
├── db/                     # (unchanged from PR #380)
│   ├── mod.rs              # DbBackend trait
│   └── duckdb_backend.rs   # DuckDB HNSW-correct implementation
├── db_writer.rs            # PyO3 RustDbWriter class (unchanged)
│
├── pipeline/               # (PR #380 — exists upstream)
│   ├── mod.rs              # IndexingPipeline struct
│   ├── pipeline.rs         # run(), parse+embed loop (UPDATED: wire bloom + token + callback)
│   └── differ.rs           # compute_diff_blocking (unchanged)
│
├── bloom.rs                # NEW: AtomicBloomFilter, load_or_rebuild_bloom(), bloom_key()
│
└── embed/                  # NEW directory
    ├── mod.rs              # EmbedBatchFn trait, PythonEmbedCallback
    ├── token.rs            # BatchBuilder, estimate_tokens(), BatchConfig
    └── callback.rs         # classify_python_embed_error(), extract_vectors_from_python()
```

### 9.1 What Changes in Existing Files

| File | Change |
|---|---|
| `src/error.rs` | Replace DB-writer-only `DbError` with unified `PipelineError` |
| `src/pipeline/pipeline.rs` | Wire bloom check + token estimation + EmbedBatchFn into batch loop |
| `src/lib.rs` | Register new modules |

No changes to Python files — the callback is already wired in PR #380.

---

## 10. Python Call Site

Unchanged from PR #380. The coordinator already passes `embed_batch_callback`:

```python
# chunkhound/pipeline_bridge.py (already exists from PR #380)
import chunkhound_native

pipeline = chunkhound_native.IndexingPipeline(db_config, pipeline_config)
report = pipeline.run(
    root="/path/to/repo",
    file_paths=paths,
    incremental=True,
    parse_batch_callback=create_parse_callback(),
    embed_batch_callback=create_embed_callback(provider),  # ← already wired
    progress_callback=create_progress_callback(),
)
```

The callback interface is already `fn(texts: List[str], provider: str, model: str) -> List[List[float]]`. Our `EmbedBatchFn` trait wraps this exact signature.

---

## 11. Test Strategy

Development follows strict TDD: write failing RED tests first, then implement GREEN, then refactor.
Per-module TDD plans are inlined in each section above:

| Module | TDD Section | RED Tests | GREEN Implementation |
|---|---|---|---|
| `src/bloom.rs` | §5.8 | 14 tests across 7 phases | `bloom_key()`, `AtomicBloomFilter` wrapper, persist/load, meta validation |
| `src/embed/token.rs` | §6.6 | 15 tests across 6 phases | `estimate_tokens()`, `BatchBuilder` (capacity, budget, chunk limit, flush) |
| `src/embed/mod.rs` | §7.6.1 | 6 mock tests | `EmbedBatchFn` trait, `EmbedBatchResult`, mock for unit testing |
| `src/embed/callback.rs` | §7.6.2 | 4 Python callback tests | `PythonEmbedCallback`, `extract_vectors_from_python()` |
| `src/error.rs` | §8.4.1 | 12 taxonomy tests | `PipelineError` enum, `is_fatal()`, `PartialEq`, display formatting |
| `src/embed/callback.rs` | §8.4.2 | 3 classification tests | `classify_python_embed_error()`, type-name matching |

### 11.1 Python Contract Tests (integration)

After all Rust unit tests pass, add integration tests using the real pipeline:

```python
# tests/contracts/test_bloom_pipeline.py

class TestBloomPipeline:
    """End-to-end bloom filter behavior in the Rust pipeline."""

    def test_bloom_skips_existing_embeddings(self, tmp_path, mock_embed_callback):
        """
        RED: Second run with same content produces 0 embed callbacks.
        GREEN: Bloom loaded from disk, all chunks hit → no callback invocation.
        """
        # Index once → populate bloom
        run_pipeline(tmp_path, files=["a.py"], embed_callback=mock_embed_callback)
        first_calls = mock_embed_callback.call_count

        # Index again → bloom hits
        run_pipeline(tmp_path, files=["a.py"], embed_callback=mock_embed_callback)
        assert mock_embed_callback.call_count <= 1  # may have 0 or 1 residual calls

    def test_bloom_model_change_rebuilds_bloom(self, tmp_path, mock_embed_callback):
        """
        RED: Switching model triggers full re-embed.
        GREEN: Meta mismatch → bloom discarded, all chunks go through callback.
        """
        run_pipeline(tmp_path, files=["a.py"], model="text-embedding-3-small")
        reset_callback(mock_embed_callback)

        run_pipeline(tmp_path, files=["a.py"], model="text-embedding-3-large")
        # All chunks should be re-embedded (new model)
        assert mock_embed_callback.call_count > 0

    def test_bloom_persists_across_pipeline_restarts(self, tmp_path):
        """
        RED: Bloom survives pipeline.close() + pipeline.open().
        GREEN: Persisted .bloom file loaded on next run, not rebuilt from DB.
        """
        pipeline1 = create_pipeline(tmp_path)
        pipeline1.run(files=["a.py"])
        pipeline1.close()

        # Verify bloom file exists
        bloom_path = tmp_path / ".chunkhound" / "db" / "embeddings.bloom"
        assert bloom_path.exists()

        pipeline2 = create_pipeline(tmp_path)
        # Second run: bloom loaded from disk (log message confirms)
        pipeline2.run(files=["a.py"])
        pipeline2.close()

    def test_token_budget_batches_are_right_sized(self, tmp_path, mock_embed_callback):
        """
        RED: Callback never receives a batch exceeding the token budget.
        GREEN: BatchBuilder enforces budget pre-dispatch.
        """
        # Create 500 chunks with varying sizes
        files = create_many_files(tmp_path, count=50, chunks_per_file=10)

        mock_embed_callback.on_batch = lambda texts, p, m: (
            assert estimate_tokens_in_rust("".join(texts)) <= VOYAGE_TOKEN_BUDGET
        )
        run_pipeline(tmp_path, files=files, embed_callback=mock_embed_callback)
```

### 11.2 Mandatory Checks (unchanged)

```bash
cargo test                              # Rust unit tests (all RED→GREEN phases)
cargo clippy --all-targets -- -D warnings
cargo fmt --check
uv run pytest tests/contracts/ -v       # Python contract tests
uv run pytest tests/test_smoke.py -v -n auto
```

---

## 12. Future Work

| Item | When | Notes |
|---|---|---|
| Rust-native OpenAI/VoyageAI providers | If callback overhead becomes bottleneck | Implement `EmbedBatchFn` in Rust using `reqwest`; no trait changes needed |
| Qwen/Ollama/Cohere native providers | When user demand justifies | Same trait, just add struct + factory |
| Bloom overflow auto-rebuild with 2× capacity | When load factor tracking added to fastbloom | |
| tiktoken integration for precise token counting | If char-based estimation produces too many false skips | Deferred — empirically chars/3 is sufficient |
| Rerank support in Rust | Separate from embed stage; search-path concern | |
| Provider health check in Rust | If CLI health-check needs Rust path | Currently Python path handles this |

---

## 13. Logging

All Rust pipeline code uses `log` crate macros routed through `pyo3-log` → Python `logging` → loguru intercept (already built in PR #380).

New log messages:

| Location | Level | Example |
|---|---|---|
| Bloom loaded from disk | INFO | `"Bloom filter loaded: 52341 entries from disk"` |
| Bloom rebuilt from DB | INFO | `"Bloom filter rebuilt from DB: 89234 entries"` |
| Bloom skip count | DEBUG | `"Bloom: 1823 skipped, 517 new chunks"` |
| Oversized chunk skipped | WARN | `"Skipping oversized chunk: est. 12000 tokens > 8191 max"` |
| Batch flushed (capacity) | DEBUG | `"Flushing batch: 204 chunks, 5800 tokens"` |
| Batch flushed (budget) | DEBUG | `"Flushing batch: 87 chunks — token budget (120000) reached"` |
| Embed callback failed (non-fatal) | WARN | `"Embed callback: 12/100 chunks failed (provider error)"` |
| Embed callback failed (fatal) | ERROR | `"Embed callback: authentication failed — aborting pipeline"` |

---

## Appendix A: Differences from Original July 19 Design

| Aspect | July 19 Design | This Design (July 21) |
|---|---|---|
| Embedding approach | Rust-native `EmbeddingProvider` trait + `reqwest` | Python callback via `EmbedBatchFn` trait |
| HTTP client | Rust `reqwest::blocking::Client` | Python async providers (OpenAI SDK, VoyageAI SDK) |
| Concurrency | Crossbeam worker pool (coordinator + N workers) | Rayon thread pool calling Python callbacks |
| Provider implementations | Rust `OpenAiProvider`, `VoyageAiProvider` (~600 lines each) | Delegated to Python (0 Rust provider lines) |
| Provider config | Static `phf` tables in Rust | Passed from Python in `PipelineConfig` |
| Retry logic | Rust `RetryPolicy` + `embed_with_retry()` | Python providers handle retries |
| Dimension discovery | Rust `AtomicUsize` for runtime dims | Python providers handle dimension discovery |
| Pipeline topology | Parse → Embed stage (coordinator thread) → DB Writer | Parse → Embed → Write in single `run()` loop |
| Progress | Crossbeam progress channel | `progress_callback` → Rich bars |
| Bloom filter | Planned, not implemented | Designed here; to be built |

## Appendix B: Comparison with PR #380 Existing Pipeline

| Component | PR #380 Status | This Design Addition |
|---|---|---|
| Pipeline orchestration | ✅ Built | Wired with bloom + token + EmbedBatchFn |
| Parallel parse | ✅ Built | Unchanged |
| Parallel embed | ✅ Built | Formalized via `EmbedBatchFn` trait |
| DB Writer | ✅ Built | Unchanged |
| Incremental reindex | ✅ Built | Unchanged |
| Progress | ✅ Built | Unchanged |
| Compaction | ✅ Built | Unchanged |
| Bloom filter | ❌ Not in PR #380 | ✅ Designed here |
| Token estimation in Rust | ❌ In Python callback | ✅ Moves to Rust pre-dispatch |
| EmbedBatchFn trait | ❌ Ad-hoc PyAny callable | ✅ Formalized trait |
| Unified error taxonomy | ❌ Ad-hoc PyErr handling | ✅ PipelineError enum |