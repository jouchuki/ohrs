//! Accumulates token usage across turns.
//!
//! ENG-7: the conversation loop re-sends the *entire* history every turn, so a
//! turn's `input_tokens` already includes everything earlier turns sent. Summing
//! per-turn `input_tokens` therefore double-counts re-sent context and grows
//! O(turns²). Instead we model input as a **high-water mark**: the most recent
//! turn's effective input tokens *are* the current context size, so we track the
//! last turn rather than summing. Output tokens are genuinely additive (each
//! turn emits new output), so those we sum.
//!
//! Input is measured with [`UsageSnapshot::effective_input_tokens`] so cache
//! reads (0.1x) and cache creation (1.25x) are folded in rather than ignored.

use oh_types::api::UsageSnapshot;

/// Tracks token usage across the turns of a single query run.
#[derive(Debug, Clone, Default)]
pub struct CostTracker {
    /// Effective input tokens of the most recent turn — i.e. the current context
    /// window size, NOT a running sum (the loop re-sends history each turn).
    pub last_turn_input_tokens: u64,
    /// Cumulative output tokens (genuinely additive across turns).
    pub total_output_tokens: u64,
    /// Number of turns observed.
    pub turns: u32,
}

impl CostTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one turn's usage.
    ///
    /// Output tokens accumulate; input is set to this turn's effective input
    /// (the current context size), replacing the previous turn's value rather
    /// than summing it.
    pub fn add(&mut self, usage: &UsageSnapshot) {
        // `effective_input_tokens` is an f64 (cache discounts/premiums); round to
        // the nearest whole token for the integer accounting surface.
        self.last_turn_input_tokens = usage.effective_input_tokens().round() as u64;
        self.total_output_tokens += usage.output_tokens;
        self.turns += 1;
    }

    /// Current context-window input size (last turn's effective input tokens).
    ///
    /// Kept under the historical name for callers that read the field as the
    /// "input tokens" surface; semantically this is the current context size,
    /// not a cumulative sum (see module docs).
    pub fn total_input_tokens(&self) -> u64 {
        self.last_turn_input_tokens
    }

    /// Effective total token surface: current input context + cumulative output.
    pub fn total_tokens(&self) -> u64 {
        self.last_turn_input_tokens + self.total_output_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_starts_at_zero() {
        let tracker = CostTracker::new();
        assert_eq!(tracker.total_input_tokens(), 0);
        assert_eq!(tracker.total_output_tokens, 0);
        assert_eq!(tracker.turns, 0);
        assert_eq!(tracker.total_tokens(), 0);
    }

    #[test]
    fn test_add_records_first_turn() {
        let mut tracker = CostTracker::new();
        let usage = UsageSnapshot {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        };
        tracker.add(&usage);
        assert_eq!(tracker.total_input_tokens(), 100);
        assert_eq!(tracker.total_output_tokens, 50);
        assert_eq!(tracker.turns, 1);
    }

    #[test]
    fn test_total_tokens_returns_sum() {
        let mut tracker = CostTracker::new();
        tracker.add(&UsageSnapshot {
            input_tokens: 30,
            output_tokens: 20,
            ..Default::default()
        });
        assert_eq!(tracker.total_tokens(), 50);
    }

    /// ENG-7: input is a high-water mark (current context size), not a running
    /// sum. Output still accumulates.
    #[test]
    fn test_input_tracks_last_turn_not_sum() {
        let mut tracker = CostTracker::new();
        // Each turn re-sends a growing context: 10, then 20, then 30 input tokens.
        tracker.add(&UsageSnapshot {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        });
        tracker.add(&UsageSnapshot {
            input_tokens: 20,
            output_tokens: 15,
            ..Default::default()
        });
        tracker.add(&UsageSnapshot {
            input_tokens: 30,
            output_tokens: 25,
            ..Default::default()
        });
        // Input is the LAST turn's value (30), not 10+20+30=60.
        assert_eq!(tracker.total_input_tokens(), 30);
        // Output is the cumulative sum.
        assert_eq!(tracker.total_output_tokens, 45);
        assert_eq!(tracker.turns, 3);
        // total = current context (30) + cumulative output (45) = 75.
        assert_eq!(tracker.total_tokens(), 75);
    }

    /// ENG-7: cache fields fold into the effective input via
    /// `effective_input_tokens` (cache read 0.1x, cache creation 1.25x).
    #[test]
    fn test_add_folds_in_cache_fields() {
        let mut tracker = CostTracker::new();
        tracker.add(&UsageSnapshot {
            input_tokens: 100,
            output_tokens: 10,
            cache_creation_input_tokens: 200, // *1.25 = 250
            cache_read_input_tokens: 1000,     // *0.1  = 100
        });
        // effective input = 100 + 250 + 100 = 450
        assert_eq!(tracker.total_input_tokens(), 450);
        assert_eq!(tracker.total_output_tokens, 10);
    }

    #[test]
    fn test_add_zero_usage() {
        let mut tracker = CostTracker::new();
        tracker.add(&UsageSnapshot::default());
        assert_eq!(tracker.total_tokens(), 0);
        assert_eq!(tracker.turns, 1);
    }
}
