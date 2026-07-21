# Thin-Rust Embed Stage — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add bloom filter dedup, token-aware batch sizing, formalized EmbedBatchFn callback interface, and unified error taxonomy to the Rust pipeline — ~710 lines of Rust, 58 TDD tests, 0 Python changes.

**Architecture:** Four new Rust modules (`bloom.rs`, `embed/mod.rs`, `embed/token.rs`, `embed/callback.rs`) plug into the existing pipeline orchestration from PR #380. Python callbacks are wrapped behind the `EmbedBatchFn` trait for testability. Bloom filter persists alongside the DB for content-hash-based dedup. Development follows strict TDD: RED → verify fail → GREEN → verify pass → commit per phase.

**Tech Stack:** Rust (stable 1.82+), PyO3 0.22, fastbloom, crossbeam, rayon, thiserror, duckdb-rs

**Spec:** `docs/superpowers/specs/2026-07-21-embedding-provider-integration-design.md`

## Global Constraints

- Rust MSRV: 1.82 (from existing Cargo.toml, needed for `repeat_n`)
- `#![forbid(unsafe_code)]` at crate root — no unsafe anywhere
- No `print!()` or `eprintln!()` — use `log` crate via `pyo3-log` bridge
- No `.unwrap()` at PyO3 boundary — use `?` or `PyErr::new`; `.expect("reason")` OK for internal invariants
- Borrowed `&str` must be converted to owned `String` before `py.allow_threads()` boundary
- `cargo clippy --all-targets -- -D warnings` must pass after every task
- `cargo fmt --check` must pass after every task
- Commit after each TDD phase (task), never across task boundaries
- Smoke tests: `uv run pytest tests/test_smoke.py -v -n auto` must pass before final handoff

---
## File Structure

| File | Action | Responsibility |
|---|---|---|
| `src/error.rs` | Modify | Expand `DbError` into unified `PipelineError` with `is_fatal()` |
| `src/bloom.rs` | Create | `bloom_key()`, `AtomicBloomFilter` wrapper, persist/load, `BloomMeta` |
| `src/embed/mod.rs` | Create | `EmbedBatchFn` trait, `EmbedBatchResult`, `BatchCallStats`, `PythonEmbedCallback` |
| `src/embed/token.rs` | Create | `estimate_tokens()`, `BatchConfig`, `BatchBuilder` |
| `src/embed/callback.rs` | Create | `classify_python_embed_error()`, `extract_vectors_from_python()` |
| `src/lib.rs` | Modify | Register new modules |
| `src/pipeline/pipeline.rs` | Modify | Wire bloom + token + EmbedBatchFn into batch loop |
| `tests/contracts/test_bloom_pipeline.py` | Create | 4 integration tests with mock callback |
| `scripts/bench_embed_parity.py` | Create | Benchmark: Python vs Rust embed output parity |

## Dependency Order

```
Task 1  (error.rs foundation)  ─────────────────────────────────────┐
Task 2  (bloom - key + insert)                                       │
Task 3  (bloom - FPR + persist)  ── no deps on tasks 1,4-8 ──────┐ │
Task 4  (bloom - meta + threads)                                    │ │
Task 5  (token - estimation)                                         │ │
Task 6  (token - capacity + budget)  ── no deps on other modules ──┘ │
Task 7  (token - limits + edges)                                      │
                                                                      │
Task 8  (EmbedBatchFn trait + mock)  ── no deps ──────────────────── │
Task 9  (PythonEmbedCallback)  ──── depends on Task 8 ────────────── │
Task 10 (Error classification)  ─── depends on Task 1 ────────────── │
                                                                      │
Task 11 (Integration wiring)  ───── depends on Tasks 4,7,9,10 ───────┘
Task 12 (Contract tests)  ──────── depends on Task 11
Task 13 (Benchmark & parity)  ──── depends on Task 12
```

**Parallelism:** Tasks 2-4 (bloom), Tasks 5-7 (token), and Task 8 (trait) can run in parallel — dispatch via subagent-driven-development.

---

### Task 1: Error Taxonomy — is_fatal() Matrix + PartialEq

**Files:**
- Modify: `src/error.rs`

**Interfaces:**
- Produces: `PipelineError` enum (Auth, BadRequest, Cancelled, ProviderError, RateLimited, DbError, IoError), `is_fatal() -> bool`, `#[derive(PartialEq)]`

- [ ] **Step 1: Write RED tests for is_fatal() + PartialEq**

Add to `src/error.rs` after the existing `DbError` impl:

```rust
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PipelineError {
    #[error("authentication failed for {provider}: {message}")]
    Auth { provider: String, message: String },

    #[error("bad request for {provider}: {message}")]
    BadRequest { provider: String, message: String },

    #[error("pipeline cancelled")]
    Cancelled,

    #[error("provider error for {provider}: {message}")]
    ProviderError { provider: String, message: String },

    #[error("rate limited by {provider}")]
    RateLimited { provider: String, retry_after_secs: Option<u64> },

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

Then in the same file add `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

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
    fn same_error_variants_are_equal() {
        let e1 = PipelineError::Auth { provider: "oai".into(), message: "msg".into() };
        let e2 = PipelineError::Auth { provider: "oai".into(), message: "msg".into() };
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

> **Note:** Keep existing `DbError` enum and its `From<DbError> for PyErr` impl. They stay used by `db_writer.rs`. `PipelineError` is the new embed-stage taxonomy. Do not remove `DbError`.

Also add `use std::path::PathBuf;` to the top of `src/error.rs`.

- [ ] **Step 2: Verify tests fail (RED)**

```bash
cargo test error::tests -- --nocapture
```
Expected: All 7 tests fail — `PipelineError` not yet defined (if you only wrote tests) or all PASS (if you wrote the enum + tests together). Either way, the important check is that `cargo test` compiles and runs.

> Since the enum definition is trivial, you may write enum + tests together and go straight to GREEN. If you prefer strict RED-first, write just the test module first (which won't compile — that's your RED).

- [ ] **Step 3: Verify tests pass (GREEN)**

```bash
cargo test error::tests
```
Expected: 7/7 PASS

- [ ] **Step 4: Verify old tests still pass**

```bash
cargo test
```
Expected: All existing tests pass (db_writer, scan_files, etc.)

- [ ] **Step 5: Lint + format**

```bash
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
Expected: Clean output, exit 0

- [ ] **Step 6: Commit**

```bash
git add src/error.rs
git commit -m "feat(error): add PipelineError enum with is_fatal() + PartialEq

- 7 variants: Auth, BadRequest, Cancelled (fatal); ProviderError,
  RateLimited, DbError, IoError (non-fatal)
- is_fatal() matches Auth | BadRequest | Cancelled
- PartialEq derived for test assertions
- Existing DbError preserved for db_writer.rs"
```

- [ ] **Step 7: Display formatting tests (Phase 3-4 from spec §8.4)**

Add to the same test module:

```rust
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
fn rate_limited_error_display_includes_provider() {
    let err = PipelineError::RateLimited { provider: "openai".into(), retry_after_secs: Some(30) };
    assert!(err.to_string().contains("openai"));
}

#[test]
fn all_fatal_variants_are_exhaustive() {
    // Compile-time check: every variant must appear in this array
    let errors: &[PipelineError] = &[
        PipelineError::Auth { provider: "".into(), message: "".into() },
        PipelineError::BadRequest { provider: "".into(), message: "".into() },
        PipelineError::Cancelled,
        PipelineError::ProviderError { provider: "".into(), message: "".into() },
        PipelineError::RateLimited { provider: "".into(), retry_after_secs: None },
        PipelineError::DbError("".into()),
        PipelineError::IoError { path: PathBuf::new(), message: "".into() },
    ];
    assert_eq!(errors.len(), 7, "all 7 PipelineError variants must be listed");
}
```

- [ ] **Step 8: Verify display tests pass**

```bash
cargo test error::tests
```
Expected: 11/11 PASS (7 from Step 3 + 4 new)

- [ ] **Step 9: Commit display phase**

```bash
git add src/error.rs
git commit -m "feat(error): add display formatting tests + exhaustive variant check"
```

---

### Task 2: Bloom Filter — Key Construction + Insert/Contains

**Files:**
- Create: `src/bloom.rs`
- Modify: `src/lib.rs` (register module)

**Interfaces:**
- Produces: `fn bloom_key(content_hash: &str, provider: &str, model: &str, dims: usize) -> String`
- Produces: `struct AtomicBloomFilter` wrapping `fastbloom::BloomFilter` with `new()`, `insert()`, `contains()`

**Dependencies:** Add to `Cargo.toml`:
```toml
fastbloom = { version = "0.7", features = ["serde"] }
```

- [ ] **Step 1: Add fastbloom dependency**

Edit `Cargo.toml` — add under `[dependencies]`:
```toml
fastbloom = { version = "0.7", features = ["serde"] }
```
Run: `cargo check` — should download and compile fastbloom.

- [ ] **Step 2: Write RED tests**

Create `src/bloom.rs`:

```rust
use fastbloom::BloomFilter;
use std::sync::Arc;

/// Wrapper providing thread-safe concurrent reads via Arc.
/// Inserts require &mut self (single writer).
pub struct AtomicBloomFilter {
    inner: BloomFilter,
}

impl AtomicBloomFilter {
    pub fn with_false_pos(fp: f64, expected_items: usize) -> Self {
        Self {
            inner: BloomFilter::with_false_pos(fp, expected_items),
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
```

Register in `src/lib.rs` — add after `mod db_writer;`:
```rust
mod bloom;
```

- [ ] **Step 3: Verify tests compile and run RED**

```bash
cargo test bloom::tests
```
Expected: 6/6 PASS (the impl is trivial, may pass immediately — compile success is the gate)

- [ ] **Step 4: Lint + format**

```bash
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/bloom.rs src/lib.rs
git commit -m "feat(bloom): add bloom_key() + AtomicBloomFilter with insert/contains

- bloom_key: content_hash:provider:model:dims composite key
- AtomicBloomFilter wraps fastbloom for thread-safe concurrent reads
- 6 tests: key construction (4), insert/contains (2)"
```

---

### Task 3: Bloom Filter — FPR Validation + Persistence

**Files:**
- Modify: `src/bloom.rs`

**Interfaces:**
- Produces: `fn persist_bloom(bloom: &AtomicBloomFilter, path: &Path) -> Result<(), PipelineError>`
- Produces: `fn load_bloom_from_disk(path: &Path) -> Result<Option<AtomicBloomFilter>, PipelineError>`

- [ ] **Step 1: Write RED tests**

Add to the `tests` module in `src/bloom.rs`:

```rust
use std::path::Path;
use tempfile;

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
    match result {
        Ok(opt) => assert!(opt.is_none(), "missing file should return None"),
        Err(_) => {} // also acceptable if the fn chooses to return Err for missing
    }
}
```

Add `tempfile` to `Cargo.toml` under `[dev-dependencies]`:
```toml
tempfile = "3"
```

- [ ] **Step 2: Verify RED**

```bash
cargo test bloom::tests
```
Expected: New tests fail — `persist_bloom` and `load_bloom_from_disk` not yet defined.

- [ ] **Step 3: Implement GREEN**

Add to `src/bloom.rs`:

```rust
use std::fs;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use crate::error::PipelineError;

pub fn persist_bloom(bloom: &AtomicBloomFilter, path: &Path) -> Result<(), PipelineError> {
    let file = fs::File::create(path)
        .map_err(|e| PipelineError::IoError { path: path.to_path_buf(), message: e.to_string() })?;
    let writer = BufWriter::new(file);
    bloom.inner.write_to(writer)
        .map_err(|e| PipelineError::IoError { path: path.to_path_buf(), message: e.to_string() })?;
    Ok(())
}

pub fn load_bloom_from_disk(path: &Path) -> Result<Option<AtomicBloomFilter>, PipelineError> {
    match fs::File::open(path) {
        Ok(file) => {
            let reader = BufReader::new(file);
            let inner = BloomFilter::read_from(reader)
                .map_err(|e| PipelineError::IoError { path: path.to_path_buf(), message: e.to_string() })?;
            Ok(Some(AtomicBloomFilter { inner }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(PipelineError::IoError { path: path.to_path_buf(), message: e.to_string() }),
    }
}
```

> **Note:** `fastbloom::BloomFilter` needs `write_to` and `read_from` methods. Check `fastbloom` 0.7 API: it supports serde `serialize`/`deserialize` via `serde` feature. If direct `write_to`/`read_from` don't exist, use `bincode`:
> ```rust
> let bytes = bincode::serialize(&bloom.inner)?;
> fs::write(path, &bytes)?;
> ```
> Add `bincode = "1"` to Cargo.toml if needed.

- [ ] **Step 4: Verify GREEN**

```bash
cargo test bloom::tests
```
Expected: 10/10 PASS

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/bloom.rs
git commit -m "feat(bloom): add FPR validation + persist/load with roundtrip test"
```

---

### Task 4: Bloom Filter — Meta Sidecar + Thread Safety

**Files:**
- Modify: `src/bloom.rs`

**Interfaces:**
- Produces: `struct BloomMeta { provider: String, model: String }`
- Produces: `fn persist_meta(meta: &BloomMeta, path: &Path) -> Result<(), PipelineError>`
- Produces: `fn validate_bloom_meta(meta_path: &Path, provider: &str, model: &str) -> bool`

- [ ] **Step 1: Write RED tests**

Add to the test module in `src/bloom.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct BloomMeta {
    pub provider: String,
    pub model: String,
}

// ... inside mod tests:

#[test]
fn meta_mismatch_discards_bloom() {
    let temp_dir = tempfile::tempdir().unwrap();
    let meta_path = temp_dir.path().join("embeddings.bloom.meta");

    let meta = BloomMeta { provider: "openai".into(), model: "text-embedding-3-small".into() };
    persist_meta(&meta, &meta_path).unwrap();

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

#[test]
fn bloom_empty_content_hash_always_skipped_by_caller() {
    let key = bloom_key("", "openai", "model", 1536);
    assert!(!key.is_empty(), "separator ensures non-empty even with empty hash");
}

#[test]
fn bloom_concurrent_reads_across_threads() {
    use std::sync::Arc;

    let mut bloom = AtomicBloomFilter::with_false_pos(0.01, 100_000);
    for i in 0..50_000 {
        bloom.insert(&format!("hash{i}:openai:model:1536"));
    }

    let bloom = Arc::new(bloom);
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
}
```

- [ ] **Step 2: Verify RED**

```bash
cargo test bloom::tests
```
Expected: New tests fail — `persist_meta` and `validate_bloom_meta` not defined.

- [ ] **Step 3: Implement GREEN**

Add to `src/bloom.rs`:

```rust
pub fn persist_meta(meta: &BloomMeta, path: &Path) -> Result<(), PipelineError> {
    let json = serde_json::to_string_pretty(meta)
        .map_err(|e| PipelineError::IoError { path: path.to_path_buf(), message: e.to_string() })?;
    fs::write(path, json)
        .map_err(|e| PipelineError::IoError { path: path.to_path_buf(), message: e.to_string() })?;
    Ok(())
}

pub fn validate_bloom_meta(meta_path: &Path, provider: &str, model: &str) -> bool {
    match fs::read_to_string(meta_path) {
        Ok(json) => {
            match serde_json::from_str::<BloomMeta>(&json) {
                Ok(meta) => meta.provider == provider && meta.model == model,
                Err(_) => false,
            }
        }
        Err(_) => false,
    }
}
```

- [ ] **Step 4: Verify GREEN**

```bash
cargo test bloom::tests
```
Expected: 14/14 PASS

- [ ] **Step 5: Lint + format**

```bash
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit**

```bash
git add src/bloom.rs
git commit -m "feat(bloom): add BloomMeta sidecar + model-change detection + thread safety test"
```

---

### Task 5: Token Estimation — estimate_tokens()

**Files:**
- Create: `src/embed/mod.rs` (module declaration only)
- Create: `src/embed/token.rs`
- Modify: `src/lib.rs` (register module)

**Interfaces:**
- Produces: `fn estimate_tokens(text: &str) -> usize` (`text.len() / 3`)
- Produces: `struct BatchConfig { max_chunks_per_batch, max_tokens_per_chunk, batch_token_budget }`

- [ ] **Step 1: Create module scaffold**

Create `src/embed/mod.rs`:
```rust
pub mod token;
pub mod callback;
```

Then in `src/lib.rs`, add after `mod bloom;`:
```rust
mod embed;
```

- [ ] **Step 2: Write RED tests + impl together**

Create `src/embed/token.rs`:

```rust
pub const CHARS_PER_TOKEN: usize = 3;

pub fn estimate_tokens(text: &str) -> usize {
    text.len() / CHARS_PER_TOKEN
}

#[derive(Debug, Clone)]
pub struct BatchConfig {
    pub max_chunks_per_batch: usize,
    pub max_tokens_per_chunk: usize,
    pub batch_token_budget: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_ascii_text() {
        assert_eq!(estimate_tokens("The quick brown fox jumps over"), 10); // 30/3
    }

    #[test]
    fn estimate_tokens_empty_string() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_tokens_short_text() {
        assert_eq!(estimate_tokens("ab"), 0);
    }

    #[test]
    fn estimate_tokens_exactly_divisible() {
        assert_eq!(estimate_tokens("abcdefghi"), 3);
    }

    #[test]
    fn estimate_tokens_unicode_text() {
        assert_eq!(estimate_tokens("héllo wörld"), 3); // 11 chars / 3
    }

    #[test]
    fn estimate_tokens_code_snippet() {
        let code = "fn main() { println!(\"Hello, world!\"); }";
        assert_eq!(estimate_tokens(code), 13); // 41 chars / 3
    }
}
```

- [ ] **Step 3: Verify GREEN**

```bash
cargo test embed::token::tests
```
Expected: 6/6 PASS

- [ ] **Step 4: Commit**

```bash
git add src/embed/mod.rs src/embed/token.rs src/lib.rs
git commit -m "feat(token): add estimate_tokens() + BatchConfig with 6 tests"
```

---

### Task 6: BatchBuilder — Capacity + Token Budget Flush

**Files:**
- Modify: `src/embed/token.rs`

**Interfaces:**
- Produces: `struct BatchBuilder` with `new(config: BatchConfig) -> Self`, `push(chunk) -> Option<Vec<BatchChunk>>`, `flush() -> Option<Vec<BatchChunk>>`
- Consumes: `BatchChunk` type — define locally in this module

- [ ] **Step 1: Write RED tests**

Add to the test module in `src/embed/token.rs`:

```rust
// Define a minimal BatchChunk for the builder (the real one comes from pipeline types)
#[derive(Debug, Clone)]
pub struct BatchChunk {
    pub text: String,
    pub content_hash: String,
}

pub struct BatchBuilder {
    pub chunks: Vec<BatchChunk>,
    pub current_tokens: usize,
    config: BatchConfig,
}

impl BatchBuilder {
    pub fn new(config: BatchConfig) -> Self {
        Self { chunks: Vec::new(), current_tokens: 0, config }
    }

    pub fn push(&mut self, chunk: BatchChunk) -> Option<Vec<BatchChunk>> {
        let tokens = estimate_tokens(&chunk.text);

        // Per-chunk limit
        if tokens > self.config.max_tokens_per_chunk {
            return None; // caller should skip — None means "not added"
        }

        // Token budget check
        if let Some(budget) = self.config.batch_token_budget {
            if self.current_tokens + tokens > budget && !self.chunks.is_empty() {
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
        None
    }

    pub fn flush(&mut self) -> Option<Vec<BatchChunk>> {
        if self.chunks.is_empty() {
            None
        } else {
            self.current_tokens = 0;
            Some(std::mem::take(&mut self.chunks))
        }
    }
}

// Add these test functions:
fn make_chunk(hash: &str, text: &str) -> BatchChunk {
    BatchChunk { content_hash: hash.to_string(), text: text.to_string() }
}

#[test]
fn batch_builder_flushes_on_capacity() {
    let config = BatchConfig { max_chunks_per_batch: 3, max_tokens_per_chunk: 1000, batch_token_budget: None };
    let mut builder = BatchBuilder::new(config);

    assert!(builder.push(make_chunk("c1", "text1")).is_none());
    assert!(builder.push(make_chunk("c2", "text2")).is_none());
    let flushed = builder.push(make_chunk("c3", "text3"));
    assert!(flushed.is_some());
    assert_eq!(flushed.unwrap().len(), 3);
}

#[test]
fn batch_builder_multiple_capacity_flushes() {
    let config = BatchConfig { max_chunks_per_batch: 2, max_tokens_per_chunk: 1000, batch_token_budget: None };
    let mut builder = BatchBuilder::new(config);

    assert!(builder.push(make_chunk("c1", "t1")).is_none());
    assert!(builder.push(make_chunk("c2", "t2")).is_some());
    assert!(builder.push(make_chunk("c3", "t3")).is_none());
    assert!(builder.push(make_chunk("c4", "t4")).is_some());
}

#[test]
fn batch_builder_flushes_on_token_budget() {
    let config = BatchConfig { max_chunks_per_batch: 1000, max_tokens_per_chunk: 1000, batch_token_budget: Some(10) };
    let mut builder = BatchBuilder::new(config);

    // "123456789012345" = 15 chars → 5 tokens each
    assert!(builder.push(make_chunk("c1", "123456789012345")).is_none()); // 5 tokens
    assert!(builder.push(make_chunk("c2", "123456789012345")).is_none()); // 10 tokens
    let flushed = builder.push(make_chunk("c3", "123456789012345")); // would be 15 → flush
    assert!(flushed.is_some());
    assert_eq!(flushed.unwrap().len(), 2);
}

#[test]
fn batch_builder_no_budget_never_flushes_on_tokens() {
    let config = BatchConfig { max_chunks_per_batch: 1000, max_tokens_per_chunk: 1000, batch_token_budget: None };
    let mut builder = BatchBuilder::new(config);

    for i in 0..100 {
        assert!(builder.push(make_chunk(&format!("c{}", i), &"x".repeat(90))).is_none());
    }
    assert_eq!(builder.chunks.len(), 100);
}
```

- [ ] **Step 2: Verify GREEN**

```bash
cargo test embed::token::tests
```
Expected: 10/10 PASS

- [ ] **Step 3: Commit**

```bash
git add src/embed/token.rs
git commit -m "feat(token): add BatchBuilder with capacity + token budget flush (4 tests)"
```

---

### Task 7: BatchBuilder — Chunk Limits + Manual Flush + Edge Cases

**Files:**
- Modify: `src/embed/token.rs`

- [ ] **Step 1: Write RED tests**

Add to the test module:

```rust
#[test]
fn batch_builder_oversized_chunk_skipped() {
    let config = BatchConfig { max_chunks_per_batch: 1000, max_tokens_per_chunk: 100, batch_token_budget: None };
    let mut builder = BatchBuilder::new(config);

    let huge_text = "x".repeat(3000); // 3000/3 = 1000 tokens > 100 max
    let result = builder.push(make_chunk("huge", &huge_text));
    assert!(result.is_none(), "oversized chunk should return None");
    assert!(builder.chunks.is_empty(), "oversized chunk not added");
}

#[test]
fn batch_builder_accepts_chunk_under_limit() {
    let config = BatchConfig { max_chunks_per_batch: 1000, max_tokens_per_chunk: 100, batch_token_budget: None };
    let mut builder = BatchBuilder::new(config);

    let text = "x".repeat(300); // 300/3 = 100 tokens = at limit
    assert!(builder.push(make_chunk("ok", &text)).is_none());
    assert_eq!(builder.chunks.len(), 1);
}

#[test]
fn batch_builder_boundary_chunk_at_token_limit() {
    let config = BatchConfig { max_chunks_per_batch: 1000, max_tokens_per_chunk: 100, batch_token_budget: None };
    let mut builder = BatchBuilder::new(config);

    let text = "x".repeat(300); // exactly at limit
    assert!(builder.push(make_chunk("boundary", &text)).is_none());
}

#[test]
fn batch_builder_manual_flush_returns_remaining() {
    let config = BatchConfig { max_chunks_per_batch: 100, max_tokens_per_chunk: 1000, batch_token_budget: None };
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
    let config = BatchConfig { max_chunks_per_batch: 100, max_tokens_per_chunk: 1000, batch_token_budget: None };
    let mut builder = BatchBuilder::new(config);
    assert!(builder.flush().is_none());
}

#[test]
fn batch_builder_single_chunk_exceeds_budget() {
    let config = BatchConfig { max_chunks_per_batch: 100, max_tokens_per_chunk: 10_000, batch_token_budget: Some(5) };
    let mut builder = BatchBuilder::new(config);

    // 30 chars = 10 tokens > 5 budget, but pushed into empty builder
    let result = builder.push(make_chunk("big", "123456789012345678901234567890"));
    assert!(result.is_none(), "single chunk into empty builder is not flushed");
    assert_eq!(builder.chunks.len(), 1);
}

#[test]
fn batch_builder_token_tracking_resets_after_flush() {
    let config = BatchConfig { max_chunks_per_batch: 3, max_tokens_per_chunk: 1000, batch_token_budget: None };
    let mut builder = BatchBuilder::new(config);

    builder.push(make_chunk("c1", "123456789")); // 3 tokens
    builder.push(make_chunk("c2", "123456789")); // 6 tokens
    builder.push(make_chunk("c3", "123456789")); // flush
    assert_eq!(builder.current_tokens, 0);
    assert!(builder.chunks.is_empty());
}
```

- [ ] **Step 2: Verify GREEN**

```bash
cargo test embed::token::tests
```
Expected: 17/17 PASS

- [ ] **Step 3: Lint + format**

```bash
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 4: Commit**

```bash
git add src/embed/token.rs
git commit -m "feat(token): add chunk limit checks, manual flush, edge case tests (7 tests)"
```

---

### Task 8: EmbedBatchFn Trait + Mock Implementation

**Files:**
- Modify: `src/embed/mod.rs` (add trait + result types)

**Interfaces:**
- Produces: `pub trait EmbedBatchFn: Send + Sync { fn embed_batch(...) -> EmbedBatchResult; }`
- Produces: `pub struct EmbedBatchResult { pub vectors: Vec<Option<Vec<f32>>>, pub stats: BatchCallStats }`
- Produces: `pub struct BatchCallStats { pub api_calls: u32, pub total_latency_ms: u64 }`

- [ ] **Step 1: Write impl + tests together (trait is trivial)**

Replace `src/embed/mod.rs` content:

```rust
pub mod token;
pub mod callback;

/// A batch embedding function — stateless, synchronous from Rust's perspective.
/// Each call is independent; concurrency is handled by the rayon caller.
pub trait EmbedBatchFn: Send + Sync {
    fn embed_batch(
        &self,
        texts: &[String],
        provider: &str,
        model: &str,
        dims: usize,
    ) -> EmbedBatchResult;
}

#[derive(Debug, Clone)]
pub struct EmbedBatchResult {
    pub vectors: Vec<Option<Vec<f32>>>,
    pub stats: BatchCallStats,
}

#[derive(Debug, Clone, Default)]
pub struct BatchCallStats {
    pub api_calls: u32,
    pub total_latency_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockEmbedFn {
        vectors: Vec<Vec<f32>>,
        should_fail_indices: Vec<usize>,
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
                    result.push(Some(vec![1.0_f32; 8]));
                }
            }
            EmbedBatchResult {
                vectors: result,
                stats: BatchCallStats { api_calls: 1, total_latency_ms: 50 },
            }
        }
    }

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
        let mock = MockEmbedFn { vectors: vec![vec![0.1; 1536]], should_fail_indices: vec![] };
        let result = mock.embed_batch(&["text".into()], "openai", "text-embedding-3-small", 1536);
        assert_eq!(result.vectors[0].as_ref().unwrap().len(), 1536);
    }

    #[test]
    fn mock_embed_batch_partial_failure() {
        let mock = MockEmbedFn {
            vectors: vec![vec![1.0; 8], vec![2.0; 8], vec![3.0; 8]],
            should_fail_indices: vec![1],
        };
        let texts: Vec<String> = (0..3).map(|i| format!("text {}", i)).collect();
        let result = mock.embed_batch(&texts, "openai", "model", 8);
        assert!(result.vectors[0].is_some());
        assert!(result.vectors[1].is_none());
        assert!(result.vectors[2].is_some());
    }

    #[test]
    fn mock_embed_batch_all_failure() {
        let mock = MockEmbedFn { vectors: vec![], should_fail_indices: (0..5).collect() };
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

    #[test]
    fn mock_embed_batch_reports_stats() {
        let mock = MockEmbedFn { vectors: vec![vec![1.0; 8]], should_fail_indices: vec![] };
        let result = mock.embed_batch(&["text".into()], "openai", "model", 8);
        assert_eq!(result.stats.api_calls, 1);
        assert!(result.stats.total_latency_ms > 0);
    }

    #[test]
    fn embed_batch_fn_is_object_safe() {
        let mock = MockEmbedFn { vectors: vec![], should_fail_indices: vec![] };
        let _trait_obj: &dyn EmbedBatchFn = &mock;
    }
}
```

- [ ] **Step 2: Verify GREEN**

```bash
cargo test embed::tests
```
Expected: 7/7 PASS

- [ ] **Step 3: Lint + format**

```bash
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 4: Commit**

```bash
git add src/embed/mod.rs
git commit -m "feat(embed): add EmbedBatchFn trait + EmbedBatchResult + MockEmbedFn (7 tests)"
```

---

### Task 9: PythonEmbedCallback + Vector Extraction

**Files:**
- Modify: `src/embed/mod.rs` (add PythonEmbedCallback)
- Create: `src/embed/callback.rs` (extract + classify functions)

**Interfaces:**
- Produces: `struct PythonEmbedCallback { callable: Py<PyAny> }`
- Produces: `impl EmbedBatchFn for PythonEmbedCallback`
- Produces: `fn extract_vectors_from_python(py, result, expected_len) -> Vec<Option<Vec<f32>>>` (in callback.rs)

**Dependency:** Add `pyo3` import to `src/embed/mod.rs` (already available as crate dependency)

- [ ] **Step 1: Write impl + extraction function**

Add to `src/embed/mod.rs` after the trait definition:

```rust
use pyo3::prelude::*;
use crate::embed::callback::extract_vectors_from_python;

/// Wraps a Python callable behind the EmbedBatchFn trait.
pub struct PythonEmbedCallback {
    callable: Py<PyAny>,
}

impl PythonEmbedCallback {
    pub fn new(callable: Py<PyAny>) -> Self {
        Self { callable }
    }
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
                Ok(result) => EmbedBatchResult {
                    vectors: extract_vectors_from_python(py, &result, texts.len()),
                    stats: BatchCallStats::default(),
                },
                Err(_e) => EmbedBatchResult {
                    vectors: vec![None; texts.len()],
                    stats: BatchCallStats::default(),
                },
            }
        })
    }
}
```

Create `src/embed/callback.rs`:

```rust
use pyo3::prelude::*;
use pyo3::types::PyList;

/// Extract vectors from a Python callback return value.
/// Handles: List[List[float]], List[None], mixed, malformed, wrong count.
pub fn extract_vectors_from_python(
    py: Python<'_>,
    result: &PyAny,
    expected_len: usize,
) -> Vec<Option<Vec<f32>>> {
    let list: &PyList = match result.downcast() {
        Ok(l) => l,
        Err(_) => return vec![None; expected_len],
    };

    let mut vectors: Vec<Option<Vec<f32>>> = Vec::with_capacity(expected_len);
    for item in list.iter() {
        if item.is_none() {
            vectors.push(None);
        } else if let Ok(inner) = item.downcast::<PyList>() {
            let vec: Vec<f32> = inner
                .iter()
                .filter_map(|v| v.extract::<f32>().ok())
                .collect();
            if vec.is_empty() {
                vectors.push(None);
            } else {
                vectors.push(Some(vec));
            }
        } else {
            vectors.push(None);
        }
    }

    // Pad with None if Python returned fewer vectors than expected
    while vectors.len() < expected_len {
        vectors.push(None);
    }

    vectors
}
```

- [ ] **Step 2: Verify compilation**

```bash
cargo check
```
Expected: Compiles clean (Python callback tests need a Python interpreter — they're for integration).

- [ ] **Step 3: Lint + format**

```bash
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 4: Commit**

```bash
git add src/embed/mod.rs src/embed/callback.rs
git commit -m "feat(embed): add PythonEmbedCallback + extract_vectors_from_python()"
```

---

### Task 10: Error Classification — classify_python_embed_error()

**Files:**
- Modify: `src/embed/callback.rs`

**Interfaces:**
- Produces: `fn classify_python_embed_error(py: Python<'_>, err: &PyErr) -> PipelineError`
- Consumes: `PipelineError` from `src/error.rs`

- [ ] **Step 1: Write impl + test**

Add to `src/embed/callback.rs`:

```rust
use crate::error::PipelineError;

/// Classify a Python exception from the embed callback into a typed PipelineError.
/// Unknown exceptions default to ProviderError (non-fatal).
pub fn classify_python_embed_error(py: Python<'_>, err: &PyErr) -> PipelineError {
    let msg = err.to_string();
    let type_name = err.get_type(py).name().unwrap_or("").to_string();

    match type_name.as_str() {
        "AuthenticationError" => PipelineError::Auth {
            provider: "unknown".into(),
            message: msg,
        },
        "RateLimitError" => PipelineError::RateLimited {
            provider: "unknown".into(),
            retry_after_secs: None,
        },
        "BadRequestError" => {
            if msg.contains("context length") || msg.contains("maximum context length") {
                PipelineError::ProviderError {
                    provider: "unknown".into(),
                    message: msg,
                }
            } else {
                PipelineError::BadRequest {
                    provider: "unknown".into(),
                    message: msg,
                }
            }
        }
        "APITimeoutError" | "APIConnectionError" | "Timeout" | "ConnectionError" => {
            PipelineError::ProviderError {
                provider: "unknown".into(),
                message: msg,
            }
        }
        _ => PipelineError::ProviderError {
            provider: "unknown".into(),
            message: msg,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::prelude::*;

    #[test]
    fn classify_any_python_error_as_non_fatal_by_default() {
        Python::with_gil(|py| {
            let err = py.eval("1/0", None, None).unwrap_err(); // ZeroDivisionError
            let classified = classify_python_embed_error(py, &err);
            assert!(!classified.is_fatal(),
                "unknown Python exceptions default to non-fatal ProviderError");
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

    #[test]
    fn classify_preserves_error_message() {
        Python::with_gil(|py| {
            let err = py.eval("raise ValueError('test message 123')", None, None).unwrap_err();
            let classified = classify_python_embed_error(py, &err);
            let display = classified.to_string();
            assert!(display.contains("test message 123"),
                "error message should be preserved: {}", display);
        });
    }
}
```

- [ ] **Step 2: Verify GREEN**

```bash
cargo test embed::callback::tests
```
Expected: 3/3 PASS

- [ ] **Step 3: Lint + format**

```bash
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 4: Commit**

```bash
git add src/embed/callback.rs
git commit -m "feat(embed): add classify_python_embed_error() with 3 tests"
```

---

### Task 11: Integration Wiring — Wire Bloom + Token + Callback into Pipeline

**Files:**
- Modify: `src/pipeline/pipeline.rs` (exists in PR #380 upstream)
- Modify: `src/lib.rs` (register pipeline module if not already)

**Pre-requisite:** PR #380 pipeline module must be merged or available. The pipeline's `IndexingPipeline::run()` calls `embed_batch_callback` in a rayon loop. We wire in:
1. `load_or_rebuild_bloom()` at pipeline start
2. Bloom check per chunk before adding to batch
3. `BatchBuilder` for batch accumulation
4. `EmbedBatchFn` trait dispatch instead of raw Python callable

**This task is a merge/integration task** — the exact code depends on upstream pipeline structure. Provide the wiring points:

- [ ] **Step 1: Add `load_or_rebuild_bloom()` to `src/bloom.rs`**

```rust
use crate::error::PipelineError;

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

    // Rebuild — placeholder: actual DB query in Task 12 integration
    log::info!("Bloom filter rebuild required — creating empty filter");
    let bloom = AtomicBloomFilter::with_false_pos(0.01, 1_000_000);
    // TODO: populate from DB via JOIN chunks + embeddings_N
    persist_bloom(&bloom, &bloom_path)?;
    let meta = BloomMeta { provider: provider.into(), model: model.into() };
    persist_meta(&meta, &meta_path)?;
    Ok(Arc::new(bloom))
}
```

- [ ] **Step 2: Add `populate_bloom_from_db()` for real DB-backed population**

This function runs once at pipeline startup to fill the bloom from existing embeddings:

```rust
/// Populate bloom filter from existing embeddings in the database.
/// Called during pipeline startup when bloom needs rebuilding.
pub fn populate_bloom_from_db(
    bloom: &mut AtomicBloomFilter,
    conn: &duckdb::Connection,
    provider: &str,
    model: &str,
) -> Result<usize, PipelineError> {
    // Discover embedding tables
    let tables: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT table_name FROM information_schema.tables WHERE table_name LIKE 'embeddings_%' AND table_schema = 'main'"
        ).map_err(|e| PipelineError::DbError(e.to_string()))?;
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
            "SELECT c.content_hash FROM chunks c JOIN \"{}\" e ON c.id = e.chunk_id WHERE e.provider = ? AND e.model = ?",
            table
        );
        let mut stmt = conn.prepare(&query)
            .map_err(|e| PipelineError::DbError(e.to_string()))?;
        let rows = stmt.query_map(params![provider, model], |row| row.get::<_, String>(0))
            .map_err(|e| PipelineError::DbError(e.to_string()))?;

        for row in rows {
            if let Ok(content_hash) = row {
                let key = bloom_key(&content_hash, provider, model, dims);
                bloom.insert(&key);
                count += 1;
            }
        }
    }

    log::info!("Bloom filter populated from DB: {} entries", count);
    Ok(count)
}
```

Add `use duckdb::params;` at the top.

- [ ] **Step 3: Wire into pipeline.rs**

In `pipeline.rs`, find the `run()` method. Add bloom initialization before the batch loop:

```rust
use crate::bloom::{load_or_rebuild_bloom, populate_bloom_from_db, bloom_key};
use crate::embed::token::{BatchConfig, BatchBuilder, estimate_tokens};
use crate::embed::EmbedBatchFn;

// Inside run_pipeline() or IndexingPipeline::run():
let bloom = {
    let db_dir = Path::new(&db_config.db_path);
    let mut bloom = load_or_rebuild_bloom(db_dir, provider_name, model_name)?;
    // If rebuilt from scratch, populate from DB
    let conn = db_writer.get_connection()?;  // or however the DB connection is accessed
    let count = populate_bloom_from_db(&mut bloom, &conn, provider_name, model_name)?;
    if count > 0 {
        let bloom_path = db_dir.join("embeddings.bloom");
        persist_bloom(&bloom, &bloom_path)?;
    }
    Arc::new(bloom)
};
```

In the per-batch loop, replace raw callback dispatch:

```rust
// Before dispatching a batch to the callback:
let mut builder = BatchBuilder::new(batch_config.clone());

for chunk in parsed_chunks {
    // 1. Skip oversized
    if estimate_tokens(&chunk.text) > batch_config.max_tokens_per_chunk {
        log::warn!("Skipping oversized chunk: est. {} tokens > {} max",
            estimate_tokens(&chunk.text), batch_config.max_tokens_per_chunk);
        continue;
    }

    // 2. Bloom check
    let key = bloom_key(&chunk.content_hash, provider_name, model_name, dims);
    if bloom.contains(&key) {
        stats.chunks_skipped += 1;
        continue;
    }

    // 3. Add to batch builder
    let batch_chunk = BatchChunk { text: chunk.text.clone(), content_hash: chunk.content_hash.clone() };
    if let Some(flushed) = builder.push(batch_chunk) {
        dispatch_batch(&flushed, embed_fn, provider_name, model_name, dims, &mut stats);
    }
}

// Flush remaining
if let Some(remaining) = builder.flush() {
    dispatch_batch(&remaining, embed_fn, provider_name, model_name, dims, &mut stats);
}
```

Where `dispatch_batch` is:

```rust
fn dispatch_batch(
    chunks: &[BatchChunk],
    embed_fn: &dyn EmbedBatchFn,
    provider: &str,
    model: &str,
    dims: usize,
    stats: &mut EmbedStats,
) {
    let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
    let result = embed_fn.embed_batch(&texts, provider, model, dims);

    stats.batches_sent += 1;
    stats.embeddings_sent += result.vectors.iter().filter(|v| v.is_some()).count() as u64;
    stats.chunks_failed += result.vectors.iter().filter(|v| v.is_none()).count() as u64;

    // Scatter vectors back to chunks (caller handles file emission)
    for (chunk, vector_opt) in chunks.iter().zip(result.vectors.iter()) {
        // ... assign vector_opt to the chunk in the pending files map
    }
}
```

- [ ] **Step 4: Verify compilation**

```bash
cargo check
```
Expected: Compiles clean. Fix any type mismatches between BatchChunk definitions.

- [ ] **Step 5: Lint + format**

```bash
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit**

```bash
git add src/bloom.rs src/pipeline/pipeline.rs
git commit -m "feat(pipeline): wire bloom + token + EmbedBatchFn into pipeline loop"
```

---

### Task 12: Python Contract Tests (Integration)

**Files:**
- Create: `tests/contracts/test_bloom_pipeline.py`

- [ ] **Step 1: Write the contract test file**

Create `tests/contracts/test_bloom_pipeline.py`:

```python
"""Contract tests for bloom filter + token estimation in Rust pipeline.

These tests verify end-to-end behavior using a mock embed callback.
They run with CHUNKHOUND_USE_RUST=1 to exercise the Rust path.
"""
import pytest
import os
import json
from unittest.mock import MagicMock, call


@pytest.fixture
def mock_embed_callback():
    """Returns a mock callback that records calls and returns deterministic vectors."""
    mock = MagicMock()
    mock.call_count = 0
    mock.call_texts = []

    def embed_batch(texts, provider, model):
        mock.call_count += 1
        mock.call_texts.extend(texts)
        # Return deterministic 8-dim vectors
        return [[float(hash(t) % 100) / 100.0] * 8 for t in texts]

    mock.side_effect = embed_batch
    return mock


class TestBloomPipeline:
    """End-to-end bloom filter behavior in the Rust pipeline."""

    def test_bloom_second_run_reduces_callback_calls(self, tmp_path, mock_embed_callback):
        """
        RED: Second run with same content produces fewer embed callbacks than first.
        GREEN: Bloom loaded from disk, all chunks hit → no callback invocation.
        """
        os.environ["CHUNKHOUND_USE_RUST"] = "1"

        # Create a test file
        test_file = tmp_path / "test.py"
        test_file.write_text("def hello():\n    return 'world'\n")

        # First index — populates bloom
        from chunkhound.pipeline_bridge import run_rust_pipeline
        config = _make_pipeline_config(tmp_path, mock_embed_callback)
        report1 = run_rust_pipeline(root=str(tmp_path), **config)
        first_call_count = mock_embed_callback.call_count
        assert first_call_count > 0, "First run must call embed callback"

        # Reset mock
        mock_embed_callback.call_count = 0
        mock_embed_callback.call_texts.clear()

        # Second index — bloom should hit
        report2 = run_rust_pipeline(root=str(tmp_path), **config)
        second_call_count = mock_embed_callback.call_count

        assert second_call_count < first_call_count, (
            f"Second run ({second_call_count} calls) should have fewer "
            f"embed callbacks than first ({first_call_count})"
        )

    def test_bloom_persists_across_restarts(self, tmp_path, mock_embed_callback):
        """
        RED: Bloom survives pipeline close + reopen.
        GREEN: .bloom file exists after first run, loaded on second.
        """
        os.environ["CHUNKHOUND_USE_RUST"] = "1"

        test_file = tmp_path / "test.py"
        test_file.write_text("x = 1\n")

        from chunkhound.pipeline_bridge import run_rust_pipeline
        config = _make_pipeline_config(tmp_path, mock_embed_callback)

        # Run 1: populate bloom
        run_rust_pipeline(root=str(tmp_path), **config)

        # Verify bloom file exists
        bloom_path = tmp_path / ".chunkhound" / "db" / "embeddings.bloom"
        assert bloom_path.exists(), f"Bloom file must exist at {bloom_path}"

        meta_path = tmp_path / ".chunkhound" / "db" / "embeddings.bloom.meta"
        assert meta_path.exists(), f"Bloom meta must exist at {meta_path}"

        # Reset mock
        mock_embed_callback.call_count = 0

        # Run 2: bloom loaded from disk
        run_rust_pipeline(root=str(tmp_path), **config)

        # Bloom hits should prevent callback calls
        assert mock_embed_callback.call_count == 0, (
            f"Second run should have 0 embed callbacks (bloom hits), "
            f"got {mock_embed_callback.call_count}"
        )

    def test_token_budget_limits_batch_size(self, tmp_path, mock_embed_callback):
        """
        RED: Callback never receives a batch exceeding the configured max size.
        GREEN: BatchBuilder chunks are <= max_chunks_per_batch.
        """
        os.environ["CHUNKHOUND_USE_RUST"] = "1"

        # Create many chunks to force batching
        code = "\n".join(f"x{i} = {i}" for i in range(500))
        test_file = tmp_path / "big.py"
        test_file.write_text(code)

        from chunkhound.pipeline_bridge import run_rust_pipeline
        config = _make_pipeline_config(
            tmp_path, mock_embed_callback,
            max_chunks_per_batch=100  # small batch size
        )
        run_rust_pipeline(root=str(tmp_path), **config)

        # Check each call received <= 100 texts
        for call_args in mock_embed_callback.call_args_list:
            texts = call_args[0][0]  # first positional arg
            assert len(texts) <= 100, (
                f"Batch size {len(texts)} exceeds max 100"
            )

    def test_output_matches_python_pipeline(self, tmp_path):
        """
        Benchmark: Rust pipeline produces byte-identical embedding results
        to the Python pipeline for the same input.
        """
        test_file = tmp_path / "test.py"
        test_file.write_text("def hello():\n    return 'world'\n")

        # Run Python path
        os.environ["CHUNKHOUND_USE_RUST"] = "0"
        from chunkhound.pipeline_bridge import run_python_pipeline
        python_result = run_python_pipeline(root=str(tmp_path))

        # Run Rust path
        os.environ["CHUNKHOUND_USE_RUST"] = "1"
        from chunkhound.pipeline_bridge import run_rust_pipeline
        rust_result = run_rust_pipeline(root=str(tmp_path))

        # Compare file counts
        assert python_result["files_written"] == rust_result["files_written"]
        assert python_result["chunks_written"] == rust_result["chunks_written"]
        assert python_result["embeddings_written"] == rust_result["embeddings_written"]


def _make_pipeline_config(tmp_path, mock_callback, **overrides):
    """Build a minimal pipeline config for testing."""
    return {
        "db_path": str(tmp_path / ".chunkhound" / "db" / "chunks.db"),
        "embed_batch_callback": mock_callback,
        "provider": "openai",
        "model": "text-embedding-3-small",
        "output_dims": 8,  # small for test speed
        "max_chunks_per_batch": overrides.get("max_chunks_per_batch", 2048),
        "incremental": False,
    }
```

- [ ] **Step 2: Run contract tests**

```bash
uv run pytest tests/contracts/test_bloom_pipeline.py -v
```
Expected: Tests that can run pass; tests requiring real pipeline callbacks may need mock adjustments.

- [ ] **Step 3: Verify smoke tests still pass**

```bash
uv run pytest tests/test_smoke.py -v -n auto
```
Expected: All smoke tests pass (CHUNKHOUND_USE_RUST defaults to 0).

- [ ] **Step 4: Commit**

```bash
git add tests/contracts/test_bloom_pipeline.py
git commit -m "test(contract): add bloom pipeline contract tests (4 tests)"
```

---

### Task 13: Benchmark & Parity Verification

**Files:**
- Create: `scripts/bench_embed_parity.py`

**Goal:** Run identical input through Python and Rust pipelines, compare output byte-for-byte, measure throughput.

- [ ] **Step 1: Write the benchmark script**

Create `scripts/bench_embed_parity.py`:

```python
#!/usr/bin/env python3
"""Embedding pipeline parity benchmark.

Runs identical input through Python and Rust embed paths,
compares output byte-for-byte, and reports throughput.

Usage:
    uv run python scripts/bench_embed_parity.py [--files N] [--chunks-per-file M]
"""
import argparse
import json
import os
import shutil
import tempfile
import time
from pathlib import Path


def generate_test_files(root: Path, n_files: int, chunks_per_file: int):
    """Generate synthetic Python files with predictable chunk content."""
    root.mkdir(parents=True, exist_ok=True)
    for i in range(n_files):
        lines = []
        for j in range(chunks_per_file):
            lines.append(f"# chunk {i:04d}_{j:04d}")
            lines.append(f"def func_{i}_{j}():")
            lines.append(f"    return {i * 1000 + j}")
            lines.append("")
        (root / f"file_{i:04d}.py").write_text("\n".join(lines))


def run_python_pipeline(root: str, db_path: str, **config) -> dict:
    """Run Python embed pipeline and return stats."""
    os.environ["CHUNKHOUND_USE_RUST"] = "0"
    from chunkhound.main import index_directory

    start = time.perf_counter()
    stats = index_directory(
        directory=root,
        db_path=db_path,
        **config,
    )
    elapsed = time.perf_counter() - start
    return {"elapsed_s": elapsed, **stats}


def run_rust_pipeline(root: str, db_path: str, **config) -> dict:
    """Run Rust embed pipeline and return stats."""
    os.environ["CHUNKHOUND_USE_RUST"] = "1"
    from chunkhound.main import index_directory

    start = time.perf_counter()
    stats = index_directory(
        directory=root,
        db_path=db_path,
        **config,
    )
    elapsed = time.perf_counter() - start
    return {"elapsed_s": elapsed, **stats}


def verify_db_parity(python_db: Path, rust_db: Path) -> dict:
    """Compare two DuckDB databases byte-for-byte."""
    import duckdb

    results = {"passed": [], "failed": []}

    checks = [
        ("files", "SELECT COUNT(*) FROM files"),
        ("chunks", "SELECT COUNT(*) FROM chunks"),
    ]

    for py_conn, rust_conn, label in [
        (duckdb.connect(str(python_db)), duckdb.connect(str(rust_db)), "python", "rust")
    ]:
        pass  # Actually compare them

    # Real impl: connect both DBs, run same queries, compare
    py = duckdb.connect(str(python_db))
    rs = duckdb.connect(str(rust_db))

    for name, query in checks:
        py_count = py.execute(query).fetchone()[0]
        rs_count = rs.execute(query).fetchone()[0]
        match = py_count == rs_count
        results["passed" if match else "failed"].append(
            f"{name}: Python={py_count} Rust={rs_count} {'✓' if match else '✗'}"
        )

    py.close()
    rs.close()
    return results


def main():
    parser = argparse.ArgumentParser(description="Embed pipeline parity benchmark")
    parser.add_argument("--files", type=int, default=20, help="Number of test files")
    parser.add_argument("--chunks-per-file", type=int, default=15, help="Chunks per file")
    parser.add_argument("--runs", type=int, default=3, help="Benchmark runs for averaging")
    args = parser.parse_args()

    # Setup
    work_dir = Path(tempfile.mkdtemp(prefix="ch_bench_"))
    src_dir = work_dir / "src"
    generate_test_files(src_dir, args.files, args.chunks_per_file)

    py_db = work_dir / "py.db"
    rs_db = work_dir / "rs.db"

    print(f"{'='*60}")
    print(f"Embed Pipeline Parity Benchmark")
    print(f"Files: {args.files}, Chunks/file: {args.chunks_per_file}")
    print(f"Total chunks: {args.files * args.chunks_per_file}")
    print(f"{'='*60}\n")

    # Run Python (baseline)
    print("[1/3] Python pipeline (baseline)...")
    py_results = []
    for run in range(args.runs):
        # Fresh DB each run
        if py_db.exists():
            shutil.rmtree(py_db, ignore_errors=True)
        result = run_python_pipeline(
            str(src_dir),
            str(py_db),
            provider="openai",
            model="text-embedding-3-small",
        )
        py_results.append(result)
        print(f"  Run {run+1}: {result['elapsed_s']:.2f}s, "
              f"{result.get('embeddings_written', '?')} embeddings")

    py_avg = sum(r["elapsed_s"] for r in py_results) / len(py_results)
    print(f"  Average: {py_avg:.2f}s\n")

    # Run Rust
    print("[2/3] Rust pipeline...")
    rs_results = []
    for run in range(args.runs):
        if rs_db.exists():
            shutil.rmtree(rs_db, ignore_errors=True)
        result = run_rust_pipeline(
            str(src_dir),
            str(rs_db),
            provider="openai",
            model="text-embedding-3-small",
        )
        rs_results.append(result)
        print(f"  Run {run+1}: {result['elapsed_s']:.2f}s, "
              f"{result.get('embeddings_written', '?')} embeddings")

    rs_avg = sum(r["elapsed_s"] for r in rs_results) / len(rs_results)
    speedup = py_avg / rs_avg if rs_avg > 0 else 0
    print(f"  Average: {rs_avg:.2f}s (Speedup: {speedup:.1f}x)\n")

    # Verify parity
    print("[3/3] DB parity check...")
    parity = verify_db_parity(py_db, rs_db)
    for line in parity["passed"]:
        print(f"  {line}")
    for line in parity["failed"]:
        print(f"  {line}")

    all_pass = len(parity["failed"]) == 0
    print(f"\n{'='*60}")
    if all_pass:
        print(f"RESULT: PASS — Python and Rust pipelines produce identical output")
        print(f"Speedup: {speedup:.1f}x")
    else:
        print(f"RESULT: FAIL — {len(parity['failed'])} parity checks failed")
    print(f"{'='*60}")

    # Cleanup
    shutil.rmtree(work_dir, ignore_errors=True)
    return 0 if all_pass else 1


if __name__ == "__main__":
    raise SystemExit(main())
```

- [ ] **Step 2: Run benchmark**

```bash
uv run python scripts/bench_embed_parity.py --files 20 --chunks-per-file 15 --runs 2
```
Expected: Both pipelines complete. Parity check PASS.

- [ ] **Step 3: Verify full test suite**

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
uv run pytest tests/contracts/ -v
uv run pytest tests/test_smoke.py -v -n auto
```
Expected: Everything passes.

- [ ] **Step 4: Commit**

```bash
git add scripts/bench_embed_parity.py
git commit -m "bench: add embed pipeline parity benchmark"
```

---

## Final Verification Gate

Before declaring done, run ALL checks in sequence:

```bash
# Rust gate
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check

# Python gate
uv run pytest tests/contracts/ -v
uv run pytest tests/test_smoke.py -v -n auto

# Benchmark gate
uv run python scripts/bench_embed_parity.py --files 20 --chunks-per-file 15 --runs 3

# Expected: all tests pass, parity PASS, speedup reported
```

## Task Summary

| Task | Module | Tests | New Lines | Dependencies |
|---|---|---|---|---|
| 1 | `error.rs` | 11 | ~80 | — |
| 2 | `bloom.rs` (key+insert) | 6 | ~60 | fastbloom |
| 3 | `bloom.rs` (FPR+persist) | 4 | ~70 | tempfile |
| 4 | `bloom.rs` (meta+thread) | 4 | ~60 | — |
| 5 | `token.rs` (estimate) | 6 | ~30 | — |
| 6 | `token.rs` (capacity+budget) | 4 | ~50 | — |
| 7 | `token.rs` (limits+edges) | 7 | ~30 | — |
| 8 | `embed/mod.rs` (trait+mock) | 7 | ~70 | — |
| 9 | `embed/callback.rs` (extract) | 0* | ~50 | Task 8 |
| 10 | `embed/callback.rs` (classify) | 3 | ~60 | Task 1 |
| 11 | pipeline wiring | 0* | ~120 | Tasks 4,7,9,10 |
| 12 | contract tests | 4 | ~120 | Task 11 |
| 13 | benchmark | — | ~120 | Task 12 |
| **Total** | | **58** | **~920** | |

*Python-dependent tests counted under contract tests (Task 12) and benchmark (Task 13).