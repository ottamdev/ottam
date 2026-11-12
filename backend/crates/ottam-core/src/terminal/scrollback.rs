use crate::core_error::CoreError;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ottam_protocol::{TerminalID, TerminalReplayChunk, TerminalReplayResult};
use std::collections::VecDeque;

/// Per-terminal in-memory ring.
pub(super) const SCROLLBACK_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Scrollback kept across exited sessions, all terminals together.
pub(super) const COMPLETED_SCROLLBACK_MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug)]
struct TerminalScrollbackChunk {
    sequence_start: u64,
    sequence_end: u64,
    byte_start: u64,
    byte_end: u64,
    data: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct TerminalScrollback {
    chunks: VecDeque<TerminalScrollbackChunk>,
    pub(super) byte_count: usize,
    max_bytes: usize,
    retention_truncated: bool,
}

impl TerminalScrollback {
    pub(super) fn new(max_bytes: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            byte_count: 0,
            max_bytes: max_bytes.max(1),
            retention_truncated: false,
        }
    }

    pub(super) fn append(&mut self, sequence: u64, byte_start: u64, data: &[u8]) {
        let byte_end = byte_start.saturating_add(data.len() as u64);
        self.chunks.push_back(TerminalScrollbackChunk {
            sequence_start: sequence,
            sequence_end: sequence,
            byte_start,
            byte_end,
            data: data.to_vec(),
        });
        self.byte_count = self.byte_count.saturating_add(data.len());
        self.evict_to_limit();
    }

    /// Evicts whole chunks, oldest first, so every retained chunk still covers
    /// its sequence completely and `retained_*` bounds stay exact. The newest
    /// chunk is never evicted, so a single chunk larger than `max_bytes` is held
    /// rather than dropping the buffer to empty; resident bytes are therefore
    /// bounded by `max_bytes` plus one PTY read.
    fn evict_to_limit(&mut self) {
        while self.byte_count > self.max_bytes && self.chunks.len() > 1 {
            let Some(front) = self.chunks.pop_front() else {
                break;
            };
            self.byte_count = self.byte_count.saturating_sub(front.data.len());
            self.retention_truncated = true;
        }
    }

    pub(super) fn clear_for_completed_budget(&mut self) -> usize {
        let removed = self.byte_count;
        // Replacing the deque releases its backing allocation as well as each
        // chunk's payload. `clear()` alone retains capacity, which would let
        // many tiny reads keep substantial metadata memory after eviction.
        self.chunks = VecDeque::new();
        self.byte_count = 0;
        if removed > 0 {
            self.retention_truncated = true;
        }
        removed
    }

    pub(super) fn replay(
        &self,
        terminal_id: TerminalID,
        max_bytes: u64,
    ) -> Result<TerminalReplayResult, CoreError> {
        // Whole chunks only, newest first: a partially returned chunk would
        // carry a sequence it no longer fully covers, and the client treats
        // `sequenceEnd` as proof it holds everything up to that sequence.
        let max_bytes = usize::try_from(max_bytes).unwrap_or(usize::MAX);
        let response_truncated = self.byte_count > max_bytes;
        let mut remaining = max_bytes;
        let mut selected = Vec::new();
        for chunk in self.chunks.iter().rev() {
            if chunk.data.len() > remaining {
                break;
            }
            selected.push(TerminalReplayChunk {
                sequence_start: chunk.sequence_start,
                sequence_end: chunk.sequence_end,
                byte_start: chunk.byte_start,
                byte_end: chunk.byte_end,
                data_base64: STANDARD.encode(&chunk.data),
            });
            remaining -= chunk.data.len();
        }
        selected.reverse();
        // `retained_*` describes what the buffer still holds, which is what the
        // field names promise; `response_truncated` is what narrows the payload.
        let first = self.chunks.front();
        let last = self.chunks.back();
        Ok(TerminalReplayResult {
            terminal_id,
            chunks: selected,
            truncated: response_truncated || self.retention_truncated,
            response_truncated,
            retention_truncated: self.retention_truncated,
            retained_byte_start: first.map(|chunk| chunk.byte_start),
            retained_byte_end: last.map(|chunk| chunk.byte_end),
            retained_sequence_start: first.map(|chunk| chunk.sequence_start),
            retained_sequence_end: last.map(|chunk| chunk.sequence_end),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_scrollback_appends_and_reports_bounds() {
        let mut scrollback = TerminalScrollback::new(64);
        scrollback.append(1, 0, b"hello");
        scrollback.append(2, 5, b" world");

        let replay = scrollback
            .replay(TerminalID("terminal-test".to_string()), 64)
            .expect("replay succeeds");
        assert_eq!(replay.chunks.len(), 2);
        assert_eq!(replay.retained_byte_start, Some(0));
        assert_eq!(replay.retained_byte_end, Some(11));
        assert_eq!(replay.retained_sequence_start, Some(1));
        assert_eq!(replay.retained_sequence_end, Some(2));
        assert!(!replay.truncated);
        let bytes = replay
            .chunks
            .iter()
            .flat_map(|chunk| STANDARD.decode(&chunk.data_base64).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(bytes, b"hello world");
    }

    #[test]
    fn terminal_scrollback_evicts_whole_chunks_and_marks_retention_truncated() {
        let mut scrollback = TerminalScrollback::new(8);
        scrollback.append(1, 0, b"abcdef");
        scrollback.append(2, 6, b"ghijkl");

        let replay = scrollback
            .replay(TerminalID("terminal-test".to_string()), 64)
            .expect("replay succeeds");
        assert!(replay.retention_truncated);
        assert!(replay.truncated);
        assert!(!replay.response_truncated);
        // The evicted chunk goes whole, so the retained bounds land on a
        // sequence boundary instead of mid-chunk.
        assert_eq!(replay.retained_byte_start, Some(6));
        assert_eq!(replay.retained_byte_end, Some(12));
        assert_eq!(replay.retained_sequence_start, Some(2));
        let bytes = replay
            .chunks
            .iter()
            .flat_map(|chunk| STANDARD.decode(&chunk.data_base64).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(bytes, b"ghijkl");
    }

    #[test]
    fn terminal_scrollback_keeps_the_newest_chunk_past_the_budget() {
        let mut scrollback = TerminalScrollback::new(4);
        scrollback.append(1, 0, b"abcdef");

        let replay = scrollback
            .replay(TerminalID("terminal-test".to_string()), 64)
            .expect("replay succeeds");
        assert_eq!(replay.chunks.len(), 1);
        assert!(!replay.retention_truncated);
        let bytes = STANDARD.decode(&replay.chunks[0].data_base64).unwrap();
        assert_eq!(bytes, b"abcdef");
    }

    #[test]
    fn terminal_scrollback_replay_caps_response_to_whole_tail_chunks() {
        let mut scrollback = TerminalScrollback::new(64);
        scrollback.append(1, 0, b"abcdef");
        scrollback.append(2, 6, b"ghijkl");

        let replay = scrollback
            .replay(TerminalID("terminal-test".to_string()), 8)
            .expect("replay succeeds");
        assert!(replay.response_truncated);
        assert!(replay.truncated);
        assert!(!replay.retention_truncated);
        // Only the newest chunk fits whole; a split would have claimed
        // sequence 1 while carrying part of its bytes.
        assert_eq!(replay.chunks.len(), 1);
        assert_eq!(replay.chunks[0].sequence_start, 2);
        assert_eq!(replay.chunks[0].byte_start, 6);
        assert_eq!(replay.chunks[0].byte_end, 12);
        let bytes = STANDARD.decode(&replay.chunks[0].data_base64).unwrap();
        assert_eq!(bytes, b"ghijkl");
        // The buffer still holds everything, so retention bounds are unchanged.
        assert_eq!(replay.retained_byte_start, Some(0));
        assert_eq!(replay.retained_byte_end, Some(12));
    }

    #[test]
    fn completed_scrollback_budget_clear_preserves_truncation_evidence() {
        let mut scrollback = TerminalScrollback::new(64);
        for sequence in 1..=16 {
            scrollback.append(sequence, sequence - 1, b"x");
        }
        assert!(scrollback.chunks.capacity() >= 16);
        assert_eq!(scrollback.clear_for_completed_budget(), 16);
        assert_eq!(scrollback.chunks.capacity(), 0);

        let replay = scrollback
            .replay(TerminalID("terminal-test".to_string()), 64)
            .expect("replay succeeds");
        assert!(replay.chunks.is_empty());
        assert!(replay.truncated);
        assert!(replay.retention_truncated);
        assert_eq!(replay.retained_byte_start, None);
        assert_eq!(replay.retained_byte_end, None);
    }
}
