#![allow(dead_code)]

pub const CHARS_PER_TOKEN: usize = 3;

pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count() / CHARS_PER_TOKEN
}

#[derive(Debug, Clone)]
pub struct BatchConfig {
    pub max_chunks_per_batch: usize,
    pub max_tokens_per_chunk: usize,
    pub batch_token_budget: Option<usize>,
}

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
        Self {
            chunks: Vec::new(),
            current_tokens: 0,
            config,
        }
    }

    pub fn push(&mut self, chunk: BatchChunk) -> Option<Vec<BatchChunk>> {
        let tokens = estimate_tokens(&chunk.text);

        // Per-chunk limit
        if tokens > self.config.max_tokens_per_chunk {
            return None; // caller should skip — None means "not added"
        }

        // Token budget check (pre-add: flush old batch, start new with this chunk)
        if let Some(budget) = self.config.batch_token_budget {
            if self.current_tokens + tokens > budget && !self.chunks.is_empty() {
                let flushed = std::mem::take(&mut self.chunks);
                self.current_tokens = 0;
                self.chunks.push(chunk);
                self.current_tokens += tokens;
                return Some(flushed);
            }
        }

        self.chunks.push(chunk);
        self.current_tokens += tokens;

        // Capacity check (post-add: flush when batch reaches capacity)
        if self.chunks.len() >= self.config.max_chunks_per_batch {
            self.current_tokens = 0;
            return Some(std::mem::take(&mut self.chunks));
        }

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

    fn make_chunk(hash: &str, text: &str) -> BatchChunk {
        BatchChunk {
            content_hash: hash.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn batch_builder_flushes_on_capacity() {
        let config = BatchConfig {
            max_chunks_per_batch: 3,
            max_tokens_per_chunk: 1000,
            batch_token_budget: None,
        };
        let mut builder = BatchBuilder::new(config);

        assert!(builder.push(make_chunk("c1", "text1")).is_none());
        assert!(builder.push(make_chunk("c2", "text2")).is_none());
        let flushed = builder.push(make_chunk("c3", "text3"));
        assert!(flushed.is_some());
        assert_eq!(flushed.unwrap().len(), 3);
    }

    #[test]
    fn batch_builder_multiple_capacity_flushes() {
        let config = BatchConfig {
            max_chunks_per_batch: 2,
            max_tokens_per_chunk: 1000,
            batch_token_budget: None,
        };
        let mut builder = BatchBuilder::new(config);

        assert!(builder.push(make_chunk("c1", "t1")).is_none());
        assert!(builder.push(make_chunk("c2", "t2")).is_some());
        assert!(builder.push(make_chunk("c3", "t3")).is_none());
        assert!(builder.push(make_chunk("c4", "t4")).is_some());
    }

    #[test]
    fn batch_builder_flushes_on_token_budget() {
        let config = BatchConfig {
            max_chunks_per_batch: 1000,
            max_tokens_per_chunk: 1000,
            batch_token_budget: Some(10),
        };
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
        let config = BatchConfig {
            max_chunks_per_batch: 1000,
            max_tokens_per_chunk: 1000,
            batch_token_budget: None,
        };
        let mut builder = BatchBuilder::new(config);

        for i in 0..100 {
            assert!(builder
                .push(make_chunk(&format!("c{}", i), &"x".repeat(90)))
                .is_none());
        }
        assert_eq!(builder.chunks.len(), 100);
    }

    #[test]
    fn batch_builder_oversized_chunk_skipped() {
        let config = BatchConfig {
            max_chunks_per_batch: 1000,
            max_tokens_per_chunk: 100,
            batch_token_budget: None,
        };
        let mut builder = BatchBuilder::new(config);

        let huge_text = "x".repeat(3000); // 3000/3 = 1000 tokens > 100 max
        let result = builder.push(make_chunk("huge", &huge_text));
        assert!(result.is_none(), "oversized chunk should return None");
        assert!(builder.chunks.is_empty(), "oversized chunk not added");
    }

    #[test]
    fn batch_builder_accepts_chunk_under_limit() {
        let config = BatchConfig {
            max_chunks_per_batch: 1000,
            max_tokens_per_chunk: 100,
            batch_token_budget: None,
        };
        let mut builder = BatchBuilder::new(config);

        let text = "x".repeat(300); // 300/3 = 100 tokens = at limit
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

        let text = "x".repeat(300); // exactly at limit
        assert!(builder.push(make_chunk("boundary", &text)).is_none());
    }

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

    #[test]
    fn batch_builder_single_chunk_exceeds_budget() {
        let config = BatchConfig {
            max_chunks_per_batch: 100,
            max_tokens_per_chunk: 10_000,
            batch_token_budget: Some(5),
        };
        let mut builder = BatchBuilder::new(config);

        // 30 chars = 10 tokens > 5 budget, but pushed into empty builder
        let result = builder.push(make_chunk("big", "123456789012345678901234567890"));
        assert!(
            result.is_none(),
            "single chunk into empty builder is not flushed"
        );
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

        builder.push(make_chunk("c1", "123456789")); // 3 tokens
        builder.push(make_chunk("c2", "123456789")); // 6 tokens
        builder.push(make_chunk("c3", "123456789")); // flush
        assert_eq!(builder.current_tokens, 0);
        assert!(builder.chunks.is_empty());
    }
}
