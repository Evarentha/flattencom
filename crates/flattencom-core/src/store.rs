/*
 * flattencom - Core Store
 *
 * Maintains byte- and count-bounded frame buffers with inclusive read cursors and eviction accounting.
 *
 * Authors:
 * worryzu <worryzu@gmail.com> @LinearTeam
 *
 * Copyright (C) 2026 Evarentha
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Session frame ring buffer.
//!
//! ## Buffer semantics
//!
//! - Sequence allocation is independent of retention: `next_seq` increases despite eviction;
//!   clients use inclusive `since_seq` cursors to avoid duplicates and omissions.
//! - Exceeding `max_frames` or `max_bytes` evicts oldest frames and increments `dropped`;
//!   statistics, events and views expose this loss.
//! - Frames are stored in increasing `seq` order; `read_since` uses binary search.

use std::collections::VecDeque;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::BufferPolicy;
use crate::frame::Frame;

/// Internal session frame buffer; external callers access it through `session`.
#[derive(Debug)]
pub struct FrameStore {
    frames: VecDeque<Frame>,
    bytes: u64,
    next_seq: u64,
    dropped: u64,
    max_frames: usize,
    max_bytes: u64,
}

impl FrameStore {
    /// Construct from a buffer policy.
    #[must_use]
    pub fn new(policy: &BufferPolicy) -> Self {
        Self {
            frames: VecDeque::with_capacity(1024),
            bytes: 0,
            next_seq: 0,
            dropped: 0,
            max_frames: policy.max_frames,
            max_bytes: policy.max_bytes,
        }
    }

    /// Allocate a sequence while holding the session lock to preserve order.
    pub fn alloc_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    /// Next allocated sequence, one greater than the latest allocated value.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Push a frame, evicting and counting oldest frames when limits are exceeded.
    ///
    /// An oversized single frame is retained even when it exceeds `max_bytes`;
    /// subsequent insertion can evict it as the oldest frame.
    pub fn push(&mut self, frame: Frame) {
        let n = frame.storage_len() as u64;
        while !self.frames.is_empty()
            && (self.frames.len() >= self.max_frames || self.bytes + n > self.max_bytes)
        {
            match self.frames.pop_front() {
                Some(old) => {
                    self.bytes -= old.storage_len() as u64;
                    self.dropped += 1;
                }
                None => break,
            }
        }
        self.bytes += n;
        self.frames.push_back(frame);
    }

    /// Current retained bytes.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Cumulative frames evicted since session start.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Retained frame count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Earliest retained sequence, or `None` for an empty buffer.
    #[must_use]
    pub fn first_seq(&self) -> Option<u64> {
        self.frames.front().map(|f| f.seq)
    }

    /// Latest retained sequence.
    #[must_use]
    pub fn last_seq(&self) -> Option<u64> {
        self.frames.back().map(|f| f.seq)
    }

    /// Iterate in capture order while the caller holds the session-state lock.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Frame> {
        self.frames.iter()
    }
    /// Iterate from an inclusive sequence using logarithmic cursor lookup.
    pub fn iter_since(&self, seq: u64) -> impl DoubleEndedIterator<Item = &Frame> {
        self.frames
            .range(self.frames.partition_point(|f| f.seq < seq)..)
    }

    /// Incremental read: `after_seq` is inclusive; `None` or `Some(0)` starts at the beginning.
    /// Return frames with `seq >= after_seq`,
    /// subject to the `max_bytes` limit.
    ///
    /// Return frames and the next inclusive cursor at the inspected window's right boundary;
    /// frames omitted by the byte limit remain available to the next read.
    #[must_use]
    pub fn read_since(&self, after_seq: Option<u64>, max_bytes: u64) -> (Vec<Frame>, u64) {
        // Inclusive lower-bound cursor pairs with returned last_seq + 1.
        let idx = match after_seq {
            None => 0,
            Some(s) => self.frames.partition_point(|f| f.seq < s),
        };
        let mut out = Vec::new();
        let mut acc = 0u64;
        for f in self.frames.iter().skip(idx) {
            acc += f.len() as u64;
            if acc > max_bytes && !out.is_empty() {
                break;
            }
            out.push(f.clone());
        }
        let next = out
            .last()
            .map_or(self.next_seq.max(after_seq.unwrap_or(0)), |f| f.seq + 1);
        (out, next)
    }

    /// Read the half-open sequence interval `[from, to)`; `None` means start/end.
    #[must_use]
    pub fn read_range(&self, from: Option<u64>, to: Option<u64>, max_bytes: u64) -> Vec<Frame> {
        let from = from.unwrap_or(0);
        let idx = self.frames.partition_point(|f| f.seq < from);
        let mut out = Vec::new();
        let mut acc = 0u64;
        for f in self.frames.iter().skip(idx) {
            if let Some(to) = to
                && f.seq >= to
            {
                break;
            }
            acc += f.len() as u64;
            if acc > max_bytes && !out.is_empty() {
                break;
            }
            out.push(f.clone());
        }
        out
    }

    /// Clear retained frames and return the count; sequence allocation continues.
    pub fn clear(&mut self) -> usize {
        let n = self.frames.len();
        self.frames.clear();
        self.bytes = 0;
        n
    }

    /// Remove only RX frames, matching driver receive-buffer clearing.
    pub fn clear_rx(&mut self) -> usize {
        let before = self.frames.len();
        self.frames.retain(|f| f.dir == crate::frame::Direction::Tx);
        self.bytes = self.frames.iter().map(|f| f.storage_len() as u64).sum();
        before - self.frames.len()
    }
}

/// Buffer usage snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct BufferLevel {
    /// Retained frame count.
    pub frames: usize,
    /// Retained byte count.
    pub bytes: u64,
    /// Cumulative evicted frame count.
    pub dropped: u64,
    /// Earliest retained sequence, or null if empty.
    pub first_seq: Option<u64>,
    /// Latest retained sequence, or null if empty.
    pub last_seq: Option<u64>,
}

impl BufferLevel {
    /// Empty buffer-level snapshot.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BufferPolicy;
    use crate::frame::Direction;

    fn frame(seq: u64, size: usize) -> Frame {
        Frame::new(seq, Direction::Rx, vec![0x41; size], 0, 0)
    }

    #[test]
    fn attributed_frames_evict_by_bytes_and_preserve_single_frame_soft_limit() {
        let base = frame(0, 1).storage_len() as u64;
        let mut store = FrameStore::new(&BufferPolicy {
            max_frames: 100,
            max_bytes: 2 * (base + 511),
        });
        for source_bytes in [511, 511, 2 * (base as usize + 511), 0] {
            let seq = store.alloc_seq();
            let mut frame = Frame::new(seq, Direction::Tx, vec![0x41], 0, 0);
            frame.source = Some("s".repeat(source_bytes));
            store.push(frame);
            match seq {
                0 => assert_eq!(
                    (store.len(), store.bytes(), store.dropped()),
                    (1, base + 511, 0)
                ),
                1 => assert_eq!(
                    (store.len(), store.bytes(), store.dropped()),
                    (2, 2 * (base + 511), 0)
                ),
                2 => assert_eq!(
                    (store.len(), store.bytes(), store.dropped()),
                    (1, base + source_bytes as u64, 2)
                ),
                3 => assert_eq!((store.len(), store.bytes(), store.dropped()), (1, base, 3)),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn clearing_rx_keeps_tx_attribution_in_retained_bytes() {
        let mut store = FrameStore::new(&BufferPolicy::default());
        let mut tx = Frame::new(store.alloc_seq(), Direction::Tx, vec![0x41], 0, 0);
        tx.source = Some("source".into());
        let tx_size = tx.storage_len() as u64;
        store.push(tx);
        let seq = store.alloc_seq();
        let rx = frame(seq, 10);
        let rx_size = rx.storage_len() as u64;
        store.push(rx);
        assert_eq!(store.bytes(), tx_size + rx_size);
        assert_eq!(store.clear_rx(), 1);
        assert_eq!(store.bytes(), tx_size);
        assert_eq!(store.clear(), 1);
        assert_eq!(store.bytes(), 0);
    }

    #[test]
    fn empty_decoded_fields_are_charged_and_evict_oversized_frames() {
        use crate::frame::{DecodeField, DecodedInfo};

        let mut store = FrameStore::new(&BufferPolicy {
            max_frames: 100,
            max_bytes: 64,
        });
        for _ in 0..10 {
            let fields = vec![
                DecodeField {
                    name: String::new(),
                    value: String::new()
                };
                10_000
            ];
            let minimum = fields.capacity() * std::mem::size_of::<DecodeField>();
            let frame = frame(store.alloc_seq(), 1).with_decoded(Some(DecodedInfo::info(
                "",
                String::new(),
                fields,
            )));
            store.push(frame);
            // Keep the intentional single-frame soft limit, never ten oversized frames.
            assert_eq!(store.len(), 1);
            assert!(store.bytes() >= minimum as u64);
        }
        assert_eq!(store.dropped(), 9);
        assert_eq!(store.first_seq(), Some(9));
    }

    #[test]
    fn decoded_field_capacity_controls_normal_budget_eviction() {
        use crate::frame::{DecodeField, DecodedInfo};

        let make_frame = |seq| {
            frame(seq, 1).with_decoded(Some(DecodedInfo::info(
                "",
                String::new(),
                Vec::<DecodeField>::with_capacity(10_000),
            )))
        };
        let size = make_frame(0).storage_len() as u64;
        let mut store = FrameStore::new(&BufferPolicy {
            max_frames: 100,
            max_bytes: size * 2,
        });
        for _ in 0..10 {
            let seq = store.alloc_seq();
            store.push(make_frame(seq));
            assert!(store.bytes() <= size * 2);
        }
        assert_eq!(store.len(), 2);
        assert_eq!(store.bytes(), size * 2);
        assert_eq!(store.dropped(), 8);
        assert_eq!(store.first_seq(), Some(8));
    }

    #[test]
    fn sent_history_and_projected_pages_include_large_sources() {
        use std::sync::Arc;

        use crate::config::SerialConfig;
        use crate::ids::SessionId;
        use crate::session::SessionHandle;
        use crate::transport::TransportRegistry;

        let mut config = SerialConfig::new("virtual://echo");
        config.buffer.max_bytes = 16 * 1024 * 1024;
        let session =
            SessionHandle::open(SessionId::new(), config, Arc::new(TransportRegistry::new()))
                .unwrap();
        let source = "s".repeat(3 * 1024 * 1024);
        let mut seqs = Vec::new();
        for _ in 0..3 {
            seqs.push(session.send_from(vec![0x41], source.clone()).unwrap());
        }
        // The independent 8 MiB sent history must evict the first attributed TX.
        let page = session.read_sent(None, u64::MAX);
        assert_eq!(
            page.frames.iter().map(|f| f.seq).collect::<Vec<_>>(),
            seqs[1..]
        );
        assert_eq!(page.dropped_rx, 1);
        let budget = page.frames[0].wire_size_hint() as u64;
        let first = session.read_sent(None, budget);
        assert_eq!(first.frames.len(), 1);
        let second = session.read_sent(Some(first.next_seq), budget);
        assert_eq!(second.frames.len(), 1);
        assert_eq!(second.frames[0].seq, seqs[2]);

        // Projected wire windows retain the intentional oversized-first-frame rule.
        let window = session.read_window(Some(seqs[0]), None, false, 1024);
        assert_eq!(window.frames.len(), 1);
        assert_eq!(window.frames[0].seq, seqs[0]);
        assert_eq!(window.frames[0].source.as_deref(), Some(source.as_str()));
        session.close();
    }

    #[test]
    fn 序号单调分配() {
        let p = BufferPolicy {
            max_frames: 100,
            max_bytes: 1 << 20,
        };
        let mut s = FrameStore::new(&p);
        assert_eq!(s.alloc_seq(), 0);
        assert_eq!(s.alloc_seq(), 1);
        assert_eq!(s.next_seq(), 2);
    }

    #[test]
    fn 增量拉取不重不漏() {
        let p = BufferPolicy {
            max_frames: 1000,
            max_bytes: 1 << 20,
        };
        let mut s = FrameStore::new(&p);
        for i in 0..10 {
            let seq = s.alloc_seq();
            s.push(frame(seq, 4));
            assert_eq!(seq, i);
        }
        // None reads all 10 frames from the beginning.
        let (a, cur) = s.read_since(None, 1 << 20);
        assert_eq!(a.len(), 10);
        assert_eq!(cur, 10);
        // Some(0) is equivalent to None and includes the first frame.
        let (b, cur1) = s.read_since(Some(0), 1 << 20);
        assert_eq!(b.len(), 10);
        assert_eq!(cur1, 10);
        // No frames remain after the cursor.
        let (c, _) = s.read_since(Some(cur), 1 << 20);
        assert!(c.is_empty(), "{:?}", c.is_empty());
    }

    #[test]
    fn 字节上限分页() {
        let p = BufferPolicy {
            max_frames: 1000,
            max_bytes: 1 << 20,
        };
        let mut s = FrameStore::new(&p);
        for _ in 0..10 {
            let seq = s.alloc_seq();
            s.push(frame(seq, 64));
        }
        let mut cursor = None;
        let mut seen = Vec::new();
        loop {
            let (page, next) = s.read_since(cursor, 128);
            if page.is_empty() {
                break;
            }
            assert_eq!(page.len(), 2);
            seen.extend(page.iter().map(|f| f.seq));
            cursor = Some(next);
        }
        assert_eq!(seen, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn 游标之后到达的帧不会被跳过() {
        let mut store = FrameStore::new(&BufferPolicy::default());
        let seq = store.alloc_seq();
        store.push(frame(seq, 4));
        let (first, cursor) = store.read_since(None, 64);
        assert_eq!(first[0].seq, 0);
        assert_eq!(cursor, 1);
        let seq = store.alloc_seq();
        store.push(frame(seq, 4));
        let (later, next) = store.read_since(Some(cursor), 64);
        assert_eq!(later.iter().map(|f| f.seq).collect::<Vec<_>>(), vec![1]);
        assert_eq!(next, 2);
    }

    #[test]
    fn 溢出丢最旧并计数() {
        let p = BufferPolicy {
            max_frames: 3,
            max_bytes: 1 << 20,
        };
        let mut s = FrameStore::new(&p);
        for i in 0..10 {
            let seq = s.alloc_seq();
            s.push(frame(seq, 8));
            let _ = i;
        }
        assert_eq!(s.len(), 3);
        assert_eq!(s.dropped(), 7);
        assert_eq!(s.first_seq(), Some(7));
        assert_eq!(s.last_seq(), Some(9));
        let (frames, _) = s.read_since(None, 1 << 20);
        let seqs: Vec<u64> = frames.iter().map(|f| f.seq).collect();
        assert_eq!(seqs, vec![7, 8, 9]);
    }

    #[test]
    fn 区间拉取() {
        let p = BufferPolicy {
            max_frames: 100,
            max_bytes: 1 << 20,
        };
        let mut s = FrameStore::new(&p);
        for _ in 0..10 {
            let seq = s.alloc_seq();
            s.push(frame(seq, 4));
        }
        let r = s.read_range(Some(3), Some(6), 1 << 20);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].seq, 3);
        let r = s.read_range(None, Some(2), 1 << 20);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn 清空保留序号() {
        let p = BufferPolicy {
            max_frames: 100,
            max_bytes: 1 << 20,
        };
        let mut s = FrameStore::new(&p);
        for _ in 0..3 {
            let seq = s.alloc_seq();
            s.push(frame(seq, 4));
        }
        assert_eq!(s.clear(), 3);
        assert!(s.is_empty());
        assert_eq!(s.alloc_seq(), 3);
    }
}
