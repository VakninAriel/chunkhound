# Embedding Provider Integration — Detailed Design

**Date:** 2026-07-19 · **Revision:** 2 (post-review)
**Scope:** Embed stage only (Phase 1 of Unified Rust + PyO3 Indexing Pipeline)
**Depends on:** PR #375 (RustDbWriter, Phase 0 DB Writer) merged
**Wiki reference:** [Unified Rust + PyO3 Indexing Pipeline Design §11](https://github.com/chunkhound/chunkhound/wiki/Unified-Rust-PyO3-Indexing-Pipeline-Design#11-embedding-provider-integration)

## Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Concurrency model | Single `std::thread`, sequential | Simplicity; backpressure from DB Writer throttles naturally |
| Providers | OpenAI (incl. Azure) + VoyageAI, extensible | Matches Python provider support; trait designed so adding a provider is self-contained |
| Rate limiting | Retry-only (reactive) | Honor `Retry-After` header + exponential backoff; no proactive token bucket |
| Rate limit semantics | `Retry-After` does not compound; exponential backoff only for non-429 | 429 is a provider ceiling, not transient error |
| Batch boundary | Per-provider max capacity, capped at 300 chunks | Matches Python `MAX_CHUNKS_PER_BATCH = 300` empirical limit; time-window deferred |
| Bloom key | `(content_hash, provider, model, output_dimensions)` | Distinguishes same-model-different-dims (matryoshka) |
| Dimension truncation | Provider-internal; embed stage sees only output dim | Preserves existing matryoshka flow unchanged |
| Error isolation | Batch-level failure → retry; if exhausted, mark `vector=None`, pipeline shuts down | Embed APIs reliable; retries handle transients |
| Retry attempts | Default 3 | Matches Python `retry_attempts=3` |
| Token handling | Token-aware batching with 6000-token safe ceiling; "context length" `BadRequest` is splittable | Prevents one large chunk from crashing pipeline |
| Stable file key | Monotonically incrementing `FileKey` (u64), not Vec index | Vec indices invalidated by `retain()` |

---

## 1. Embed Stage Architecture

### 1.1 Pipeline Position

```
Parser (rayon)          Embed (1 thread)         DB Writer (1 thread)
    │                        │                        │
    │  ParsedFile            │  EmbeddedFile          │
    ├───────────────────────>├───────────────────────>│
    │  crossbeam bounded     │  crossbeam bounded     │
    │  (max(50, N×16))      │  (100)                 │
    │                        │                        │
    │              CacheSnapshot                      │
    │              ┌──────────────────┐               │
    │              │ bloom (lock-free)│ ← Embed reads │
    │              └──────────────────┘               │
```

The embed thread owns its `Box<dyn EmbeddingProvider>` (wrapping `reqwest::blocking::Client`). It holds no DB connection and no Python references.

### 1.2 Stable File Keys

`BatchChunk` references files by a stable `FileKey`, not a Vec index:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FileKey(u64);

struct EmbedBuffer {
    pending_files: HashMap<FileKey, PendingFile>,
    batch:         Vec<BatchChunk>,
    stats:         EmbedStats,
    next_key:      u64,
}
```

`next_key` increments monotonically. `HashMap` survives `retain()`-style removal without index shifting.

### 1.3 Main Loop

```rust
fn embed_stage(
    rx: Receiver<ParsedFile>,
    tx: Sender<EmbeddedFile>,
    provider: Box<dyn EmbeddingProvider>,
    bloom: Arc<AtomicBloomFilter>,
    retry: RetryPolicy,
    cancelled: Arc<AtomicBool>,
) -> Result<EmbedStats, EmbedError> {
    let mut buffer = EmbedBuffer::new();
    let effective_batch_size = provider.max_batch_size().min(MAX_CHUNKS_PER_BATCH);

    for parsed in rx.iter() {
        // 1. Pass-through for Deleted/Error
        if matches!(parsed.kind, FileEventKind::Deleted | FileEventKind::Error) {
            tx.send(EmbeddedFile::from_parsed(&parsed, &provider))?;
            continue;
        }

        // 2. Build intermediate file with mutable vector slots
        let mut file = EmbeddedFile::from_parsed(&parsed, &provider);
        let mut new_chunks: usize = 0;

        for (chunk_idx, chunk) in parsed.chunks.iter().enumerate() {
            if chunk.text.trim().is_empty() || chunk.content_hash.is_empty() {
                continue;  // skip empty / legacy-hashless chunks
            }

            // Token check: skip chunks exceeding provider context window
            let token_estimate = estimate_tokens(&chunk.text);
            if token_estimate > provider.max_tokens_per_chunk() {
                continue;  // oversized chunk — logged, skipped, counted
            }

            let key = bloom_key(&chunk.content_hash, provider.name(), provider.model(), provider.dimensions());
            if bloom.contains(&key) {
                buffer.stats.chunks_skipped += 1;
                continue;
            }

            let file_key = buffer.next_key;
            buffer.batch.push(BatchChunk {
                file_key,
                chunk_idx,
                text: chunk.text.clone(),
            });
            buffer.stats.chunks_checked += 1;
            new_chunks += 1;
        }

        if new_chunks == 0 {
            tx.send(file)?;
        } else {
            buffer.next_key += 1;
            buffer.pending_files.insert(file_key, PendingFile { file, remaining: new_chunks });
        }

        // 3. Flush when batch reaches capacity
        if buffer.batch.len() >= effective_batch_size {
            flush_batch(&mut buffer, &tx, &*provider, &retry, &cancelled)?;
        }

        if cancelled.load(Ordering::Relaxed) { break; }
    }

    // 4. Final flush + error-safe drain (see §5.3)
    if !buffer.batch.is_empty() {
        flush_batch(&mut buffer, &tx, &*provider, &retry, &cancelled)?;
    }
    // Safety net: any remaining pending files with partial vectors
    for (_, pf) in buffer.pending_files.drain() {
        tx.send(pf.file)?;
    }
    drop(tx);
    Ok(buffer.stats)
}
```

### 1.4 Thread Lifecycle

1. Created in `run_pipeline_inner` via `std::thread::scope`
2. Runs until Parser drops its Sender → `rx.iter()` exits
3. Final flush + safety-net drain
4. Drops output Sender → DB Writer drains
5. Returns `EmbedStats`

### 1.5 Shutdown on Error

On unrecoverable error (retries exhausted or fatal):
1. Sets `cancelled.store(true, Ordering::Relaxed)`
2. Marks un-flushed batch chunks as `vector=None`
3. Emits all pending files with partial vectors
4. Drops output Sender
5. Propagates error up through `std::thread::scope`

---

## 2. `EmbeddingProvider` Trait

```rust
/// Embedding provider — object-safe, no generics, no associated types.
/// All methods are synchronous; the consumer runs in a `std::thread`.
pub trait EmbeddingProvider: Send {
    /// Send a batch of texts. Returns exactly `texts.len()` vectors, in order.
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>;

    /// Max texts per API call (provider-documented limit).
    fn max_batch_size(&self) -> usize;

    /// Stable provider identifier for bloom keys/stats/logging.
    fn name(&self) -> &str;

    /// Model identifier for bloom keys and API requests.
    fn model(&self) -> &str;

    /// Output dimension AFTER truncation. Must equal vector length from embed_batch.
    fn dimensions(&self) -> usize;

    /// Maximum tokens a single chunk text can contain for this provider.
    /// The embed stage uses this for pre-flight token estimation to avoid
    /// sending chunks that would trigger a "context length exceeded" error.
    fn max_tokens_per_chunk(&self) -> usize;

    /// Recommended number of concurrent in-flight batches. Defaults to 1
    /// (sequential). Providers that support higher concurrency (e.g. OpenAI
    /// allows many parallel requests) can override. Kept for future use;
    /// the current embed stage is single-threaded.
    fn recommended_concurrency(&self) -> usize { 1 }
}
```

**`Send` only, not `Sync`**: provider is exclusively owned by one thread. `Sync` prevents valid `RefCell`-backed implementations.

### 2.1 `EmbedError`

```rust
#[derive(thiserror::Error, Debug)]
pub enum EmbedError {
    #[error("HTTP request failed: {0}")]
    Http(String),                                         // retryable

    #[error("rate limited (HTTP 429): retry after {retry_after:?}")]
    RateLimited { retry_after: Option<Duration> },        // retryable

    #[error("authentication failed (HTTP 401/403): {0}")]
    Auth(String),                                         // fatal

    #[error("invalid request (HTTP 4xx): {0}")]
    BadRequest(String),                                   // see note below

    #[error("context length exceeded: {0}")]
    ContextLengthExceeded(String),                        // retryable (split batch)

    #[error("provider error (HTTP 5xx): {0}")]
    Provider(String),                                     // retryable

    #[error("unexpected response format: {0}")]
    ResponseFormat(String),                               // fatal

    #[error("embedding cancelled")]
    Cancelled,                                            // pipeline exit
}
```

`BadRequest` is fatal unless the error message contains "context length" or "maximum context length" → then it maps to `ContextLengthExceeded`, which is retryable with batch splitting.

### 2.2 HTTP Status → `EmbedError` Mapping

Provider impls inspect the HTTP response **before** calling `error_for_status()`, extracting headers:

```rust
fn classify_response(response: &reqwest::blocking::Response) -> Result<(), EmbedError> {
    match response.status().as_u16() {
        200..=299 => Ok(()),
        401 | 403 => Err(EmbedError::Auth(response.text().unwrap_or_default())),
        429 => {
            let retry_after = response.headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs);
            Err(EmbedError::RateLimited { retry_after })
        }
        400..=499 => {
            let body = response.text().unwrap_or_default();
            if body.contains("context length") || body.contains("maximum context length") {
                Err(EmbedError::ContextLengthExceeded(body))
            } else {
                Err(EmbedError::BadRequest(body))
            }
        }
        500..=599 => Err(EmbedError::Provider(response.text().unwrap_or_default())),
        _ => Err(EmbedError::Http(format!("HTTP {}", response.status()))),
    }
}
```

> **Intentional simplification vs Python:** The Python `_classify_*_error` functions parse provider-specific JSON error bodies for granular retry categories (rate-limit types, billing errors, etc.). The Rust version uses HTTP status codes and `Retry-After` headers — sufficient for the embedding stage because the retry loop handles all retryable errors uniformly with exponential backoff. The Python async path needs finer categories for `asyncio.Semaphore` fairness; the sequential Rust thread does not.

### 2.3 Factory

```rust
pub fn create_provider(cfg: &EmbedConfig) -> Result<Box<dyn EmbeddingProvider>, EmbedError> {
    match cfg.provider.as_str() {
        "openai" | "azure_openai" => Ok(Box::new(OpenAiProvider::new(cfg)?)),
        "voyageai" => Ok(Box::new(VoyageAiProvider::new(cfg)?)),
        other => Err(EmbedError::BadRequest(format!("unknown provider: {other}"))),
    }
}
```

### 2.4 `EmbedConfig` (PyO3 boundary)

```rust
use pyo3::prelude::*;
use pyo3::types::PyDict;

#[derive(Debug, Clone)]
pub struct EmbedConfig {
    pub provider:               String,
    pub api_key:                String,
    pub model:                  String,
    pub base_url:               Option<String>,
    pub output_dims:            Option<usize>,
    pub client_side_truncation: bool,
    pub batch_size:             Option<usize>,
    pub timeout_seconds:        u64,
    pub retry_max_attempts:     u32,
    pub ssl_verify:             bool,
    pub api_version:            Option<String>,   // Azure OpenAI
    pub azure_endpoint:         Option<String>,   // Azure OpenAI
}

impl EmbedConfig {
    /// Construct from a Python dict (PyO3 boundary). Follows the pattern
    /// established by DbConfig::from_pydict in PR #375.
    pub fn from_pydict(py: Python<'_>, dict: &PyDict) -> PyResult<Self> {
        Ok(Self {
            provider:       dict.get_item("provider")?.extract()?,
            api_key:        dict.get_item("api_key")?.extract()?,
            model:          dict.get_item("model")?.extract()?,
            base_url:       dict.get_item("base_url")?.extract::<Option<String>>()?,
            output_dims:    dict.get_item("output_dims")?.extract::<Option<usize>>()?,
            client_side_truncation: dict.get_item("client_side_truncation")?
                .extract::<Option<bool>>()?.unwrap_or(false),
            batch_size:     dict.get_item("batch_size")?.extract::<Option<usize>>()?,
            timeout_seconds: dict.get_item("timeout")?.extract::<Option<u64>>()?.unwrap_or(60),
            retry_max_attempts: dict.get_item("retry_attempts")?.extract::<Option<u32>>()?.unwrap_or(3),
            ssl_verify:     dict.get_item("ssl_verify")?.extract::<Option<bool>>()?.unwrap_or(true),
            api_version:    dict.get_item("api_version")?.extract::<Option<String>>()?,
            azure_endpoint: dict.get_item("azure_endpoint")?.extract::<Option<String>>()?,
        })
    }
}
```

Effective batch size: `min(config.batch_size.unwrap_or(usize::MAX), provider.max_batch_size(), MAX_CHUNKS_PER_BATCH)`.

---

## 3. Provider Implementations

### 3.1 OpenAI Provider (incl. Azure)

#### Model Config Table

```rust
struct OpenAiModelInfo {
    native_dims: usize,
    max_tokens:  usize,
    matryoshka:  bool,
}

const OPENAI_MODEL_CONFIG: phf::Map<&'static str, OpenAiModelInfo> = phf_map! {
    "text-embedding-3-small"  => OpenAiModelInfo { native_dims: 1536, max_tokens: 8191, matryoshka: true },
    "text-embedding-3-large"  => OpenAiModelInfo { native_dims: 3072, max_tokens: 8191, matryoshka: true },
    "text-embedding-ada-002"  => OpenAiModelInfo { native_dims: 1536, max_tokens: 8191, matryoshka: false },
};
```

#### Struct

```rust
pub struct OpenAiProvider {
    client:                 reqwest::blocking::Client,
    api_key:                String,
    model:                  String,
    base_url:               String,
    output_dims:            Option<usize>,
    client_side_truncation: bool,
    model_info:             OpenAiModelInfo,
    is_azure:               bool,
    api_version:            Option<String>,
}

impl OpenAiProvider {
    const MAX_BATCH: usize = 2048;

    pub fn new(cfg: &EmbedConfig) -> Result<Self, EmbedError> {
        let is_azure = cfg.azure_endpoint.is_some() ||
            cfg.base_url.as_deref().map_or(false, |u| u.contains("openai.azure.com"));
        let model_info = OPENAI_MODEL_CONFIG
            .get(&cfg.model)
            .copied()
            .unwrap_or(OpenAiModelInfo { native_dims: 1536, max_tokens: 8191, matryoshka: false });

        let mut client_builder = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_seconds));
        if !cfg.ssl_verify {
            client_builder = client_builder.danger_accept_invalid_certs(true);
        }

        Ok(Self {
            client: client_builder.build().map_err(|e| EmbedError::Http(e.to_string()))?,
            api_key: cfg.api_key.clone(),
            model: cfg.model.clone(),
            base_url: cfg.base_url.clone().unwrap_or_else(|| {
                if is_azure { cfg.azure_endpoint.clone().unwrap() }
                else { "https://api.openai.com/v1".into() }
            }),
            output_dims: cfg.output_dims,
            client_side_truncation: cfg.client_side_truncation,
            model_info,
            is_azure,
            api_version: cfg.api_version.clone(),
        })
    }
}

impl EmbeddingProvider for OpenAiProvider {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut body = json!({ "model": self.model, "input": texts });
        if let Some(dims) = self.output_dims {
            if !self.client_side_truncation && self.model_info.matryoshka {
                body["dimensions"] = json!(dims);
            }
        }

        let mut url = format!("{}/embeddings", self.base_url);
        if self.is_azure {
            if let Some(ref ver) = self.api_version {
                url.push_str(&format!("?api-version={}", ver));
            }
        }

        let response = self.client
            .post(&url)
            .header("api-key", &self.api_key)
            .json(&body)
            .send()
            .map_err(|e| EmbedError::Http(e.to_string()))?;

        // Classify BEFORE error_for_status to capture Retry-After header
        classify_response(&response)?;

        let raw: OpenAIResponse = response.json()
            .map_err(|e| EmbedError::ResponseFormat(e.to_string()))?;

        let mut vectors: Vec<Vec<f32>> = raw.data.into_iter().map(|d| d.embedding).collect();
        if self.client_side_truncation {
            let out = self.output_dims.unwrap();
            vectors = vectors.into_iter().map(|v| l2_normalize(&v[..out])).collect();
        }
        Ok(vectors)
    }

    fn max_batch_size(&self) -> usize { Self::MAX_BATCH }
    fn name(&self) -> &str {
        if self.is_azure { "azure_openai" } else { "openai" }
    }
    fn model(&self) -> &str { &self.model }
    fn dimensions(&self) -> usize { self.output_dims.unwrap_or(self.model_info.native_dims) }
    fn max_tokens_per_chunk(&self) -> usize { self.model_info.max_tokens }
}

#[derive(Deserialize)]
struct OpenAIResponse {
    data: Vec<OpenAIEmbeddingData>,
}

#[derive(Deserialize)]
struct OpenAIEmbeddingData {
    embedding: Vec<f32>,
    index:     usize,
}
```

### 3.2 VoyageAI Provider

#### Model Config Table

```rust
struct VoyageModelInfo {
    native_dims:          usize,
    max_tokens_per_chunk: usize,   // context_length per individual text
    max_tokens_per_batch: usize,   // total token budget per API call (120k or 320k)
    supported_dimensions: &'static [usize],  // valid output_dims for this model
}

const VOYAGE_MODEL_CONFIG: phf::Map<&'static str, VoyageModelInfo> = phf_map! {
    "voyage-3-large"         => VoyageModelInfo { native_dims: 1024, max_tokens_per_chunk: 32000, max_tokens_per_batch: 120000, supported_dimensions: &[256, 512, 1024, 2048] },
    "voyage-3"               => VoyageModelInfo { native_dims: 1024, max_tokens_per_chunk: 32000, max_tokens_per_batch: 120000, supported_dimensions: &[256, 512, 1024, 2048] },
    "voyage-3-lite"          => VoyageModelInfo { native_dims:  512, max_tokens_per_chunk: 32000, max_tokens_per_batch: 120000, supported_dimensions: &[256, 512] },
    "voyage-3.5"             => VoyageModelInfo { native_dims: 1024, max_tokens_per_chunk: 32000, max_tokens_per_batch: 320000, supported_dimensions: &[256, 512, 1024, 2048] },
    "voyage-3.5-lite"        => VoyageModelInfo { native_dims: 1024, max_tokens_per_chunk: 32000, max_tokens_per_batch: 1000000, supported_dimensions: &[256, 512, 1024, 2048] },
    "voyage-code-3"          => VoyageModelInfo { native_dims: 1024, max_tokens_per_chunk: 32000, max_tokens_per_batch: 120000, supported_dimensions: &[256, 512, 1024, 2048] },
    "voyage-finance-2"       => VoyageModelInfo { native_dims: 1024, max_tokens_per_chunk: 32000, max_tokens_per_batch: 120000, supported_dimensions: &[1024] },
    "voyage-law-2"           => VoyageModelInfo { native_dims: 1024, max_tokens_per_chunk: 16000, max_tokens_per_batch: 120000, supported_dimensions: &[1024] },
    "voyage-multilingual-2"  => VoyageModelInfo { native_dims: 1024, max_tokens_per_chunk: 32000, max_tokens_per_batch: 120000, supported_dimensions: &[1024] },
    "voyage-large-2-instruct"=> VoyageModelInfo { native_dims: 1024, max_tokens_per_chunk: 16000, max_tokens_per_batch: 120000, supported_dimensions: &[1024] },
    "voyage-2"               => VoyageModelInfo { native_dims: 1024, max_tokens_per_chunk:  4000, max_tokens_per_batch: 320000, supported_dimensions: &[1024] },
};
```

#### Struct

```rust
pub struct VoyageAiProvider {
    client:                 reqwest::blocking::Client,
    api_key:                String,
    model:                  String,
    base_url:               String,
    output_dims:            Option<usize>,
    client_side_truncation: bool,
    model_info:             VoyageModelInfo,
}

impl VoyageAiProvider {
    const MAX_BATCH: usize = 1000;  // matches Python max_texts_per_batch

    pub fn new(cfg: &EmbedConfig) -> Result<Self, EmbedError> {
        let model_info = VOYAGE_MODEL_CONFIG
            .get(&cfg.model)
            .copied()
            .unwrap_or(VoyageModelInfo { native_dims: 1024, max_tokens_per_chunk: 32000, max_tokens_per_batch: 320000, supported_dimensions: &[] });

        let mut client_builder = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_seconds));
        if !cfg.ssl_verify {
            client_builder = client_builder.danger_accept_invalid_certs(true);
        }

        Ok(Self {
            client: client_builder.build().map_err(|e| EmbedError::Http(e.to_string()))?,
            api_key: cfg.api_key.clone(),
            model: cfg.model.clone(),
            base_url: cfg.base_url.clone().unwrap_or_else(|| "https://api.voyageai.com/v1".into()),
            output_dims: cfg.output_dims,
            client_side_truncation: cfg.client_side_truncation,
            model_info,
        })
    }
}

impl EmbeddingProvider for VoyageAiProvider {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut body = json!({
            "model": self.model,
            "input": texts,
            "input_type": "document",
            "truncation": true,      // always enabled — matches Python behavior
        });
        if let Some(dims) = self.output_dims {
            if !self.client_side_truncation {
                body["output_dimension"] = json!(dims);
            }
        }

        let response = self.client
            .post(format!("{}/embeddings", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .map_err(|e| EmbedError::Http(e.to_string()))?;

        classify_response(&response)?;

        let raw: VoyageResponse = response.json()
            .map_err(|e| EmbedError::ResponseFormat(e.to_string()))?;

        let mut vectors: Vec<Vec<f32>> = raw.data.into_iter().map(|d| d.embedding).collect();
        if self.client_side_truncation {
            let out = self.output_dims.unwrap();
            vectors = vectors.into_iter().map(|v| l2_normalize(&v[..out])).collect();
        }
        Ok(vectors)
    }

    fn max_batch_size(&self) -> usize { Self::MAX_BATCH }
    fn name(&self) -> &str { "voyageai" }
    fn model(&self) -> &str { &self.model }
    fn dimensions(&self) -> usize { self.output_dims.unwrap_or(self.model_info.native_dims) }
    fn max_tokens_per_chunk(&self) -> usize { self.model_info.max_tokens_per_chunk }
}

#[derive(Deserialize)]
struct VoyageResponse {
    data: Vec<VoyageEmbeddingData>,
}

#[derive(Deserialize)]
struct VoyageEmbeddingData {
    embedding: Vec<f32>,
    index:     usize,
}
```

### 3.3 Concern Ownership

| Concern | Location |
|---|---|
| HTTP client (reqwest, auth, timeout, SSL) | Provider struct |
| Request body + response parsing | `embed_batch()` |
| Native dimensions | Provider constants |
| Truncation (server-side param or client-side slice+normalize) | `embed_batch()` |
| HTTP status → `EmbedError` | Shared `classify_response()` |
| Per-model config (dims, max_tokens) | Static provider table |

---

## 4. Bloom Filter Integration

### 4.1 Ownership & Sharing

The bloom filter is created in `load_cache()` during pipeline setup, wrapped in `Arc<AtomicBloomFilter>`, and passed to both the Embed stage (read-only) and the DB Writer (write-only post-commit) via `run_pipeline_inner`:

```rust
fn run_pipeline_inner(
    root: &str,
    embed_cfg: EmbedConfig,
    db_cfg: DbConfig,
    pipeline_cfg: PipelineConfig,
) -> Result<PyObject, PipelineError> {
    let db = create_backend(&db_cfg);
    let cache = db.load_cache()?;
    let provider = create_provider(&embed_cfg)?;

    let bloom = Arc::clone(&cache.embeddings);
    let cancelled = Arc::new(AtomicBool::new(false));

    std::thread::scope(|s| {
        let (scanner_tx, scanner_rx) = crossbeam::channel::bounded(500);
        let (parser_tx, parser_rx) = crossbeam::channel::bounded(max(50, num_cpus * 16));
        let (embed_tx, embed_rx) = crossbeam::channel::bounded(100);

        s.spawn(|| file_scanner(root, &cache, scanner_tx, &cancelled));
        s.spawn(|| parser_stage(scanner_rx, parser_tx, &cancelled));
        s.spawn(|| {
            let stats = embed_stage(parser_rx, embed_tx, provider, bloom, retry, cancelled)?;
            Ok(stats)
        });
        // DB Writer receives bloom via Arc clone in spawn closure:
        s.spawn(|| db_writer_stage(embed_rx, db, Arc::clone(&cache.embeddings), &cancelled));
    })
}
```

### 4.2 Bloom Key Construction

```rust
fn bloom_key(content_hash: &str, provider: &str, model: &str, dims: usize) -> String {
    format!("{content_hash}:{provider}:{model}:{dims}")
}
```

Separator `:` is safe — `content_hash` is hex (xxhash), provider/model use `[a-z0-9_-]`.

### 4.3 Embed Stage Interaction

```
chunk arrives
    ├─ kind Delete/Error?              → pass through, no bloom check
    ├─ text empty or no content_hash?   → skip (no embedding possible)
    ├─ estimated tokens > max?          → skip (would exceed context window)
    ├─ bloom.contains(key)?             → skip (stats.chunks_skipped++)
    └─ no                               → add to batch
```

False positive rate: 1% (~1 in 100 un-embedded chunks skipped). Recovered on next run.

### 4.4 Bloom Initialization

```rust
fn load_cache(db: &dyn DbBackend) -> Result<CacheSnapshot, PipelineError> {
    let (files, embedding_count) = db.load_cache_data()?;
    let capacity = (embedding_count as f64 * 1.5) as usize;
    let capacity = capacity.max(1_000_000);
    let bloom = AtomicBloomFilter::with_false_pos(0.01, capacity); // ← 1% FPR
    db.populate_bloom(&bloom)?;  // JOIN chunks + embeddings tables on (content_hash, provider, model, dims)
    Ok(CacheSnapshot { files: Arc::new(RwLock::new(files)), embeddings: Arc::new(bloom) })
}
```

Persisted to `.chunkhound/db/embeddings.bloom` via `fastbloom` serde. Fallback: rebuild from DB if absent or corrupt.

### 4.5 Model/Provider Change Validation

On pipeline startup, compare the stored bloom's `(provider, model)` metadata (persisted alongside the bloom) against the current config. If they differ, discard the persisted bloom and rebuild from DB. This prevents stale bloom entries from the wrong provider/model combination.

### 4.6 Edge Cases

| Case | Behavior |
|---|---|
| Bloom overflow (load factor > 0.9) | Deferred: log warning, continue with stale filter. Full rebuild handled by compaction cycle. |
| `content_hash` empty (legacy) | Always embed; these chunks don't participate in dedup |
| First run on empty DB | Bloom created with 1M minimum; all keys miss |
| Re-index with different model | Validation discards old bloom → full re-embed |

---

## 5. Batch Accumulation & Flush

### 5.1 Internal State

```rust
const MAX_CHUNKS_PER_BATCH: usize = 300;   // matches Python embedding_service.py
const TOKEN_SAFE_LIMIT: usize = 6000;      // pre-flight token ceiling

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FileKey(u64);

struct EmbedBuffer {
    pending_files: HashMap<FileKey, PendingFile>,
    batch:         Vec<BatchChunk>,
    stats:         EmbedStats,
    next_key:      u64,
}

struct PendingFile {
    file:      EmbeddedFile,   // chunks already have vector slots (initially None)
    remaining: usize,          // chunks still in batch
}

struct BatchChunk {
    file_key:  FileKey,        // stable — not invalidated by removal
    chunk_idx: usize,          // index into file.chunks
    text:      String,
}
```

### 5.2 Flush Triggers

| Trigger | Condition | Batch size |
|---|---|---|
| Capacity full | `batch.len() >= effective_batch_size` | exactly `effective_batch_size` |
| Channel drain | `rx.iter()` exits | any size ≥ 1 |
| Cancellation | `cancelled.load(Relaxed)` → drain with `None` vectors | any size ≥ 1 |

### 5.3 `flush_batch()`

```rust
fn flush_batch(
    buffer: &mut EmbedBuffer,
    tx: &Sender<EmbeddedFile>,
    provider: &dyn EmbeddingProvider,
    retry: &RetryPolicy,
    cancelled: &AtomicBool,
) -> Result<(), EmbedError> {
    if buffer.batch.is_empty() { return Ok(()); }

    // 1. Extract text, call provider with retry
    let texts: Vec<String> = buffer.batch.iter().map(|c| c.text.clone()).collect();
    let vectors = match embed_with_retry(provider, &texts, retry, cancelled) {
        Ok(v) => v,
        Err(EmbedError::ContextLengthExceeded(_)) => {
            // Split batch in half and retry each half individually.
            // If a single chunk exceeds the provider's limit, it was already
            // filtered by token estimation — this path handles cumulative
            // token overflow when batching many medium-length chunks.
            return flush_split_batch(buffer, tx, provider, retry, cancelled);
        }
        Err(e) => return Err(e),
    };

    // 2. Scatter vectors back into pending_files
    let chunks: Vec<BatchChunk> = buffer.batch.drain(..).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let pf = buffer.pending_files.get_mut(&chunk.file_key)
            .expect("file_key must exist in pending_files");
        pf.file.chunks[chunk.chunk_idx].vector = Some(vectors[i].clone());
        pf.remaining -= 1;
    }

    // 3. Emit completed files via manual drain (not retain(), to propagate errors)
    let completed: Vec<FileKey> = buffer.pending_files.iter()
        .filter(|(_, pf)| pf.remaining == 0)
        .map(|(k, _)| *k)
        .collect();
    for key in completed {
        let pf = buffer.pending_files.remove(&key)
            .expect("key just collected from iter");
        tx.send(pf.file)?;
    }

    buffer.stats.embeddings_sent += texts.len() as u64;
    buffer.stats.batches_sent += 1;
    Ok(())
}
```

### 5.4 All-Bloom-Hit Files

A file where every chunk hits the bloom or is skipped: `new_chunks == 0` → emitted immediately, never enters `pending_files`.

### 5.5 Multi-Batch Files

A file with more new chunks than `effective_batch_size` spans multiple flushes. `FileKey` + `remaining` counter ensures it stays in `pending_files` across flushes until `remaining == 0`.

### 5.6 Error-Safe Drain

On cancellation or final drain:
1. All un-flushed batch chunks get `vector=None` scattered into their files
2. All `pending_files` are emitted (some with partial vectors)
3. `tx` is dropped

The safety-net drain runs BEFORE error propagation to ensure DB Writer always receives the drop signal:

```rust
// Final flush (may error)
let result = flush_batch(&mut buffer, &tx, &*provider, &retry, &cancelled);

// Safety net ALWAYS runs, regardless of error
for (_, pf) in buffer.pending_files.drain() {
    let _ = tx.send(pf.file);  // ignore error — channel may be dead
}
drop(tx);

result?; // propagate error after drain
```

---

## 6. Retry & Error Handling

### 6.1 `RetryPolicy`

```rust
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,       // default: 3 (matches Python retry_attempts)
    pub base_delay:   Duration,  // default: 1 second
    pub max_delay:    Duration,  // default: 60 seconds
    pub jitter:       bool,      // default: true
}
```

### 6.2 Retry Loop

```rust
fn embed_with_retry(
    provider: &dyn EmbeddingProvider,
    texts: &[String],
    policy: &RetryPolicy,
    cancelled: &AtomicBool,
    stats: &mut EmbedStats,          // caller-owned; mutated for retries/failures
) -> Result<Vec<Vec<f32>>, EmbedError> {
    let mut attempt: u32 = 0;
    let mut delay = policy.base_delay;

    loop {
        if cancelled.load(Ordering::Relaxed) { return Err(EmbedError::Cancelled); }
        attempt += 1;

        match provider.embed_batch(texts) {
            Ok(vectors) => {
                if vectors.len() != texts.len() {
                    return Err(EmbedError::ResponseFormat(format!(
                        "expected {} vectors, got {}", texts.len(), vectors.len()
                    )));
                }
                return Ok(vectors);
            }
            Err(e) => {
                stats.retries += 1;

                if matches!(&e, EmbedError::ContextLengthExceeded(_)) {
                    return Err(e);  // propagate for split-and-retry
                }
                if !is_retryable(&e) || attempt >= policy.max_attempts {
                    stats.chunks_failed += texts.len() as u64;
                    return Err(e);
                }
                let wait = if let EmbedError::RateLimited { retry_after: Some(ra) } = &e {
                    ra.to_owned()
                } else {
                    delay
                };
                sleep_with_jitter(wait, policy.jitter);
                if !matches!(&e, EmbedError::RateLimited { .. }) {
                    delay = (delay * 2).min(policy.max_delay);
                }
            }
        }
    }
}

fn is_retryable(e: &EmbedError) -> bool {
    matches!(e,
        EmbedError::Http(_)
        | EmbedError::RateLimited { .. }
        | EmbedError::Provider(_)
        | EmbedError::ContextLengthExceeded(_)
    )
}
```

### 6.3 Batch Splitting on `ContextLengthExceeded`

```rust
fn flush_split_batch(
    buffer: &mut EmbedBuffer,
    tx: &Sender<EmbeddedFile>,
    provider: &dyn EmbeddingProvider,
    retry: &RetryPolicy,
    cancelled: &AtomicBool,
) -> Result<(), EmbedError> {
    // Drain current batch, split into two halves, re-queue
    let mut all = std::mem::take(&mut buffer.batch);
    let mid = all.len() / 2;

    if mid == 0 {
        // Single chunk exceeded limit — skip it
        buffer.stats.chunks_failed += 1;
        return Ok(());
    }

    // Brief backoff before retry — avoids hammering the provider
    sleep_with_jitter(Duration::from_millis(500), true);

    buffer.batch = all.split_off(mid);
    let first_half = all;

    // Flush first half
    buffer.batch = first_half;
    flush_batch(buffer, tx, provider, retry, cancelled)?;

    // Second half will be flushed on next iteration or capacity trigger
    // (it's already in buffer.batch from split_off)
    Ok(())
}
```

### 6.4 Error Propagation

| Failure | Embed Stage Response |
|---|---|
| Single batch, retry succeeds | Log warning, continue. `stats.retries` incremented. |
| Batch, retries exhausted | Set `cancelled=true`. Mark batch chunks `vector=None`. Emit pending. Drop tx. |
| Batch, fatal error (Auth) | Same drain-and-exit. |
| `ContextLengthExceeded` | Split batch in half, retry each. Single oversized chunk → skip. |
| Cancellation | Safety-net drain → drop tx → exit. |

### 6.5 Token Estimation

```rust
/// Rough token estimate: ~3 chars per token for code (matches Python's
/// EMBEDDING_CHARS_PER_TOKEN = 3.0, measured empirically for both OpenAI
/// and VoyageAI). Slightly underestimates (3.0 vs actual 3.0–3.5), producing
/// smaller batches which is the safe direction.
/// Actual token counting (tiktoken) is deferred to provider impl if needed.
const EMBEDDING_CHARS_PER_TOKEN: usize = 3;

fn estimate_tokens(text: &str) -> usize {
    text.len() / EMBEDDING_CHARS_PER_TOKEN
}
```

Chunks whose `estimate_tokens() > provider.max_tokens_per_chunk()` are skipped with a warning.

### 6.6 Utilities

```rust
use std::thread::sleep;
use rand::Rng;

fn sleep_with_jitter(duration: Duration, jitter: bool) {
    let d = if jitter {
        let j: f64 = rand::thread_rng().gen_range(0.0..0.25);
        duration + Duration::from_secs_f64(duration.as_secs_f64() * j)
    } else {
        duration
    };
    sleep(d);
}

/// L2-normalize a slice of f32 values. Returns a new Vec<f32>.
fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 { return v.to_vec(); }
    v.iter().map(|x| x / norm).collect()
}
```

---

## 7. Extensibility — Adding a New Provider

### 7.1 Contract

Implement these methods:

```rust
impl EmbeddingProvider for MyProvider {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> { ... }
    fn max_batch_size(&self) -> usize { ... }
    fn name(&self) -> &str { ... }
    fn model(&self) -> &str { ... }
    fn dimensions(&self) -> usize { ... }
    fn max_tokens_per_chunk(&self) -> usize { ... }
}
```

### 7.2 Steps (example: Qwen via OpenAI-compatible endpoint)

Qwen models use the OpenAI API format. Add to `OPENAI_MODEL_CONFIG` and use `base_url` override:

```rust
// Static config addition:
"qwen3-embedding-8b" => OpenAiModelInfo {
    native_dims: 4096, max_tokens: 32768, matryoshka: false
},
// User config: { "provider": "openai", "model": "qwen3-embedding-8b",
//                 "base_url": "https://dashscope.aliyuncs.com/compatible-mode/v1" }
```

For non-OpenAI-compatible providers, create a new struct + impl + register in factory. The trait is the sole interface contract.

### 7.3 What NOT to Do

- Don't add provider-specific fields to `EmbedConfig` — use the provider's `new()` for defaults
- Don't change the trait — provider-specific behavior stays in the impl
- Don't branch on `provider.name()` in the embed stage — the stage is provider-agnostic

---

## 8. Types

### 8.1 `EmbeddedFile` and `EmbeddedChunk`

```rust
#[derive(Debug, Clone)]
pub struct EmbeddedFile {
    pub path: PathBuf,
    pub file_id: Option<u64>,
    pub kind: FileEventKind,
    pub mtime: f64,
    pub size_bytes: u64,
    pub content_hash: String,
    pub language: String,
    pub chunks: Vec<EmbeddedChunk>,
}

#[derive(Debug, Clone)]
pub struct EmbeddedChunk {
    pub text: String,
    pub content_hash: String,
    pub chunk_type: String,
    pub symbol: String,
    pub language: String,
    pub code: String,
    pub start_line: u32,
    pub end_line: u32,
    /// Filled by embed stage; None if embedding failed.
    pub vector: Option<Vec<f32>>,
    pub provider: String,
    pub model: String,
}

impl EmbeddedFile {
    /// Build from Parser output. All vectors start as None; the embed stage
    /// fills them in. provider/model are populated from the provider trait.
    /// For Deleted/Error files, chunks is empty and vectors stay None.
    pub fn from_parsed(p: &ParsedFile, provider: &dyn EmbeddingProvider) -> Self {
        Self {
            path: p.path.clone(),
            file_id: p.file_id,
            kind: p.kind,
            mtime: p.mtime,
            size_bytes: p.size_bytes,
            content_hash: p.content_hash.clone(),
            language: p.language.clone(),
            chunks: p.chunks.iter().map(|c| EmbeddedChunk {
                text: c.text.clone(),
                content_hash: c.content_hash.clone(),
                chunk_type: c.chunk_type.clone(),
                symbol: c.symbol.clone(),
                language: c.language.clone(),
                code: c.code.clone(),
                start_line: c.start_line,
                end_line: c.end_line,
                vector: None,
                provider: provider.name().to_string(),
                model: provider.model().to_string(),
            }).collect(),
        }
    }
}
```

### 8.2 `EmbedStats`

```rust
#[derive(Debug, Default)]
pub struct EmbedStats {
    pub chunks_checked: u64,    // incremented when chunk passes bloom + token check
    pub chunks_skipped: u64,    // bloom-hit skips
    pub embeddings_sent: u64,   // chunks sent to API
    pub batches_sent: u64,      // API calls made
    pub retries: u64,           // incremented in retry loop (each attempt after first)
    pub chunks_failed: u64,     // chunks where embedding failed after all retries
}
```

### 8.3 Module Layout

```
src/embed/
├── mod.rs          # embed_stage(), EmbedBuffer, flush_batch(), estimate_tokens()
├── provider.rs     # EmbeddingProvider trait, create_provider(), EmbedConfig, classify_response()
├── openai.rs       # OpenAiProvider (incl. Azure)
├── voyageai.rs     # VoyageAiProvider
├── retry.rs        # RetryPolicy, embed_with_retry(), is_retryable(), sleep_with_jitter()
├── types.rs        # EmbeddedFile, EmbeddedChunk, EmbedStats, FileKey, BatchChunk, etc.
└── utils.rs        # l2_normalize(), bloom_key()
```

---

## 9. Dependencies

### 9.1 Cargo.toml additions (on top of PR #375 + existing)

```toml
[dependencies]
reqwest      = { version = "0.12", features = ["blocking", "json", "rustls-tls"] }
serde        = { version = "1", features = ["derive"] }
serde_json   = "1"
thiserror    = "1"                # matches PR #375
crossbeam    = "0.8"             # already in wiki design
fastbloom    = { version = "0.7", features = ["serde"] }  # bloom filter
phf          = { version = "0.11", features = ["macros"] }  # static model config
rand         = "0.8"             # jitter random

[dev-dependencies]
mockito      = "1"               # HTTP mock server for provider tests
```

---

## 10. Python Call Site

The embed stage is instantiated inside `run_pipeline` (Phase 1 PyO3 boundary). The Python call site follows the wiki design §5.1:

```python
# chunkhound/services/pipeline_bridge.py (Phase 1)
import chunkhound_native

stats = chunkhound_native.run_pipeline(
    root="/path/to/repo",
    crash_recovery_done=True,
    embed_config={
        "provider": "openai",
        "api_key": "...",
        "model": "text-embedding-3-small",
        "output_dims": None,
        "batch_size": None,
        "timeout": 60,
        "retry_attempts": 3,
        "ssl_verify": True,
    },
    db_config=db.config_dict(),
    pipeline_config=cfg.to_dict(),
)
```

Python validation (Pydantic) owns config correctness. Rust trusts the dict structure. The GIL is released on `run_pipeline` entry and re-acquired only to build the return dict.

---

## 11. Test Strategy

### 11.1 Unit Tests (Rust)

```rust
#[cfg(test)]
mod tests {
    #[test] fn bloom_key_includes_all_components();
    #[test] fn estimate_tokens_rough();
    #[test] fn l2_normalize_unit_vector();
    #[test] fn classify_response_maps_status_codes();
    #[test] fn retry_policy_defaults_match_python();
    #[test] fn classify_context_length_as_context_length_exceeded();
    #[test] fn azure_endpoint_detected();
    #[test] fn voyageai_truncation_in_body();
    #[test] fn batch_split_halves_on_context_length();
}
```

### 11.2 Integration Tests (Python)

| Test | Coverage |
|---|---|
| `test_embed_provider_parity.py` | Rust EmbeddingProvider output matches Python provider on same input (mock server) |
| `test_embed_pipeline.py` | Embed stage connected to mock Parser + DB Writer via channels |
| `test_embed_retry.py` | Retry loop behavior — 429 with Retry-After, 5xx with exponential backoff, Auth is fatal |
| `test_embed_token_limit.py` | Oversized chunk skipped, batch split on context-length error |

### 11.3 Property Tests

- **Batch ordering invariant**: Regardless of bloom hits/skips, files are emitted in the order they were received from Parser
- **Vector count invariant**: Every chunk in the batch after flush has `vector.len() == provider.dimensions()` or `vector.is_none()`

### 11.4 Mandatory Checks

```bash
cargo test embed::                           # Rust embed unit tests
cargo clippy -- -D warnings                  # Rust lint
uv run pytest tests/test_smoke.py -v -n auto # existing smoke
uv run pytest tests/test_embed_*.py -v       # embed-specific
```

---

## 12. Future Work

| Item | When |
|---|---|
| Time-window flush (`embed_flush_timeout_seconds`) | If sparse repos become a problem |
| Concurrent HTTP requests via thread pool | If single-thread throughput is bottleneck |
| Bloom overflow auto-rebuild with 2× capacity | When load factor tracking is added to fastbloom |
| Qwen/Cohere/Ollama native providers | When user demand justifies dedicated structs |
| Actual tiktoken integration for precise token counting | If estimate_tokens produces too many false skips |
| Rerank support in Rust | Separate from embed stage; search-path concern |