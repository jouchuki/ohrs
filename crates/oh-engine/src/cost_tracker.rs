//! Accumulates token usage across turns.

use oh_types::api::UsageSnapshot;

/// Tracks cumulative token usage.
#[derive(Debug, Clone, Default)]
pub struct CostTracker {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub turns: u32,
}

impl CostTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, usage: &UsageSnapshot) {
        self.total_input_tokens += usage.input_tokens;
        self.total_output_tokens += usage.output_tokens;
        self.turns += 1;
    }

    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_starts_at_zero() {
        let tracker = CostTracker::new();
        assert_eq!(tracker.total_input_tokens, 0);
        assert_eq!(tracker.total_output_tokens, 0);
        assert_eq!(tracker.turns, 0);
        assert_eq!(tracker.total_tokens(), 0);
    }

    #[test]
    fn test_add_accumulates_tokens() {
        let mut tracker = CostTracker::new();
        let usage = UsageSnapshot {
            input_tokens: 100,
            output_tokens: 50, ..Default::default() };
        tracker.add(&usage);
        assert_eq!(tracker.total_input_tokens, 100);
        assert_eq!(tracker.total_output_tokens, 50);
        assert_eq!(tracker.turns, 1);
    }

    #[test]
    fn test_total_tokens_returns_sum() {
        let mut tracker = CostTracker::new();
        tracker.add(&UsageSnapshot {
            input_tokens: 30,
            output_tokens: 20, ..Default::default() });
        assert_eq!(tracker.total_tokens(), 50);
    }

    #[test]
    fn test_multiple_adds_accumulate() {
        let mut tracker = CostTracker::new();
        tracker.add(&UsageSnapshot {
            input_tokens: 10,
            output_tokens: 5, ..Default::default() });
        tracker.add(&UsageSnapshot {
            input_tokens: 20,
            output_tokens: 15, ..Default::default() });
        tracker.add(&UsageSnapshot {
            input_tokens: 30,
            output_tokens: 25, ..Default::default() });
        assert_eq!(tracker.total_input_tokens, 60);
        assert_eq!(tracker.total_output_tokens, 45);
        assert_eq!(tracker.turns, 3);
        assert_eq!(tracker.total_tokens(), 105);
    }

    #[test]
    fn test_add_zero_usage() {
        let mut tracker = CostTracker::new();
        tracker.add(&UsageSnapshot::default());
        assert_eq!(tracker.total_tokens(), 0);
        assert_eq!(tracker.turns, 1);
    }
}
