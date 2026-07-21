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
        assert_eq!(estimate_tokens("héllo wörld"), 4); // 13 bytes / 3
    }

    #[test]
    fn estimate_tokens_code_snippet() {
        let code = "fn main() { println!(\"Hello, world!\"); }";
        assert_eq!(estimate_tokens(code), 13); // 41 chars / 3
    }
}