//! Per-utterance 16 kHz mono PCM ring buffer (SPEC.md §3.1, §3.2; V1.1
//! acceptance: "ring buffer replays a full utterance").
//!
//! Bounded and allocation-stable: capacity is fixed at construction and the
//! backing store never reallocates on push. Pushing past capacity overwrites
//! the oldest samples (sliding-window semantics), which is what the hot path
//! needs — Opus-encode-on-escalation replays "the utterance so far" without
//! ever growing memory unboundedly on a runaway/very long utterance.

/// A fixed-capacity ring buffer of 16-bit PCM samples (mono, 16 kHz per
/// `LocalAsr::feed_pcm`'s contract — this type is sample-rate-agnostic and
/// just stores whatever `i16` frames it's given).
#[derive(Debug, Clone)]
pub struct PcmRingBuffer {
    storage: Vec<i16>,
    capacity: usize,
    /// Index in `storage` where the next sample will be written.
    write_pos: usize,
    /// Total samples ever written; used to know whether the buffer has
    /// wrapped and how many valid samples it currently holds.
    total_written: u64,
}

impl PcmRingBuffer {
    /// Create a new ring buffer holding at most `capacity_samples` samples.
    /// `capacity_samples == 0` is coerced to 1 so the buffer is never
    /// degenerate (a zero-capacity ring buffer can't hold "the most recent
    /// sample," which every caller implicitly expects).
    #[must_use]
    pub fn new(capacity_samples: usize) -> Self {
        let capacity = capacity_samples.max(1);
        Self {
            storage: vec![0; capacity],
            capacity,
            write_pos: 0,
            total_written: 0,
        }
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of valid samples currently held (== capacity once the buffer
    /// has wrapped at least once).
    #[must_use]
    pub fn len(&self) -> usize {
        (self.total_written as usize).min(self.capacity)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total_written == 0
    }

    /// Push one frame of samples. Allocation-stable: never resizes
    /// `storage`. Oldest samples are silently overwritten once `capacity` is
    /// exceeded.
    pub fn push(&mut self, frames: &[i16]) {
        for &sample in frames {
            self.storage[self.write_pos] = sample;
            self.write_pos = (self.write_pos + 1) % self.capacity;
            self.total_written += 1;
        }
    }

    /// Reset to empty without deallocating the backing store — used between
    /// utterances so the same buffer instance can be reused for the next
    /// key-down without a fresh allocation.
    pub fn clear(&mut self) {
        self.write_pos = 0;
        self.total_written = 0;
        // Contents are left as-is; `len()`/`replay()` are governed by
        // `total_written`, not by zeroing storage, so this stays O(1).
    }

    /// Replay the currently held samples in chronological order (oldest
    /// first). This is the "ring buffer replays a full utterance" contract
    /// from V1.1's acceptance criteria.
    #[must_use]
    pub fn replay(&self) -> Vec<i16> {
        let held = self.len();
        if held == 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(held);
        if (self.total_written as usize) < self.capacity {
            // Genuinely never wrapped (strictly fewer samples written than
            // capacity): samples are storage[0..write_pos] in order.
            out.extend_from_slice(&self.storage[..self.write_pos]);
        } else if self.write_pos == 0 {
            // Wrapped exactly to the boundary (total_written is a nonzero
            // multiple of capacity): write_pos has wrapped back to 0, but
            // that does NOT mean "nothing to replay" — the whole buffer is
            // full and in order starting at index 0. Using the `else`
            // branch below here would slice storage[0..] twice with an
            // empty first slice, which is harmless, but is expressed
            // explicitly to make the exactly-full case impossible to
            // confuse with the truly-empty case (total_written == 0, which
            // already returned early above via `held == 0`).
            out.extend_from_slice(&self.storage[..]);
        } else {
            // Wrapped mid-buffer: oldest sample is at write_pos (about to
            // be overwritten next), newest is at write_pos - 1.
            out.extend_from_slice(&self.storage[self.write_pos..]);
            out.extend_from_slice(&self.storage[..self.write_pos]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replays_a_full_utterance_within_capacity() {
        let mut buf = PcmRingBuffer::new(10);
        buf.push(&[1, 2, 3]);
        buf.push(&[4, 5]);
        assert_eq!(buf.replay(), vec![1, 2, 3, 4, 5]);
        assert_eq!(buf.len(), 5);
    }

    #[test]
    fn wrap_around_keeps_most_recent_samples_in_order() {
        let mut buf = PcmRingBuffer::new(4);
        // Push 6 samples into a 4-slot buffer: 1,2,3,4,5,6 -> keep 3,4,5,6.
        buf.push(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.replay(), vec![3, 4, 5, 6]);
    }

    #[test]
    fn wrap_around_across_multiple_pushes_not_aligned_to_capacity() {
        let mut buf = PcmRingBuffer::new(5);
        buf.push(&[1, 2, 3]); // storage: 1 2 3 . . , write_pos=3
        buf.push(&[4, 5, 6]); // wraps: overwrite index0 with 6 -> 6 2 3 4 5, write_pos=1
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.replay(), vec![2, 3, 4, 5, 6]);
        buf.push(&[7]); // overwrite index1(2) -> write_pos=2
        assert_eq!(buf.replay(), vec![3, 4, 5, 6, 7]);
    }

    #[test]
    fn allocation_is_stable_across_pushes() {
        let mut buf = PcmRingBuffer::new(8);
        let ptr_before = buf.storage.as_ptr();
        for _ in 0..100 {
            buf.push(&[0; 3]);
        }
        assert_eq!(
            buf.storage.as_ptr(),
            ptr_before,
            "backing store reallocated"
        );
        assert_eq!(buf.capacity(), 8);
    }

    #[test]
    fn clear_resets_without_growing() {
        let mut buf = PcmRingBuffer::new(4);
        buf.push(&[1, 2, 3, 4, 5]);
        buf.clear();
        assert!(buf.is_empty());
        assert_eq!(buf.replay(), Vec::<i16>::new());
        buf.push(&[9]);
        assert_eq!(buf.replay(), vec![9]);
    }

    #[test]
    fn replay_at_capacity_minus_one_is_not_full_but_correct() {
        let mut buf = PcmRingBuffer::new(4);
        buf.push(&[1, 2, 3]);
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.replay(), vec![1, 2, 3]);
    }

    #[test]
    fn replay_at_exactly_capacity_is_not_empty() {
        // Regression for the MAJOR finding: total_written == capacity wraps
        // write_pos back to 0, which must NOT be confused with "nothing
        // written." Pushing exactly `capacity` samples must replay all of
        // them, in order, not an empty Vec.
        let mut buf = PcmRingBuffer::new(4);
        buf.push(&[1, 2, 3, 4]);
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.replay(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn replay_at_capacity_plus_one_drops_oldest_sample() {
        let mut buf = PcmRingBuffer::new(4);
        buf.push(&[1, 2, 3, 4, 5]);
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.replay(), vec![2, 3, 4, 5]);
    }

    #[test]
    fn replay_at_exactly_capacity_after_multiple_pushes() {
        // Same boundary, but total_written reaches capacity across several
        // push() calls rather than one, and after a prior wrap — exercises
        // the write_pos == 0 branch when it's reached via modular
        // arithmetic rather than starting fresh.
        let mut buf = PcmRingBuffer::new(4);
        buf.push(&[1, 2]);
        buf.push(&[3, 4]);
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.replay(), vec![1, 2, 3, 4]);

        // Push exactly one more full capacity's worth (4 samples): total
        // written is now 8, a multiple of capacity 4, so write_pos wraps to
        // 0 again — same boundary, second time around.
        buf.push(&[5, 6, 7, 8]);
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.replay(), vec![5, 6, 7, 8]);
    }

    #[test]
    fn zero_capacity_is_coerced_to_one() {
        let mut buf = PcmRingBuffer::new(0);
        assert_eq!(buf.capacity(), 1);
        buf.push(&[1, 2, 3]);
        assert_eq!(buf.replay(), vec![3]);
    }
}
