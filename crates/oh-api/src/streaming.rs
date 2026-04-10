//! SSE stream parsing utilities.
//!
//! The actual SSE parsing is done inline in the client module.
//! This module provides helper types for future incremental streaming support.

use oh_types::api::ApiStreamEvent;

/// A buffered SSE parser that can be fed chunks of bytes.
pub struct SseParser {
    buffer: String,
    events: Vec<ApiStreamEvent>,
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            events: Vec::new(),
        }
    }

    /// Feed a chunk of SSE data.
    pub fn feed(&mut self, chunk: &str) {
        self.buffer.push_str(chunk);
        // TODO: Implement incremental SSE parsing for true streaming
    }

    /// Drain any complete events.
    pub fn drain(&mut self) -> Vec<ApiStreamEvent> {
        std::mem::take(&mut self.events)
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}
