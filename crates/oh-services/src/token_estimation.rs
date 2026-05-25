//! Token count estimation utilities.

/// Rough estimate of tokens in a string (~4 chars per token for English).
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}
