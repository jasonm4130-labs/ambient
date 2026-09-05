//! Cost accounting for a live transcription pass, and the backlog it implies.
//!
//! `src/bin/asrbench.rs` measures one block at a time — resample, VAD, decode —
//! and this is where those timings turn into an answer to the only question
//! that matters for live work: does a transcriber keep up, or does it fall
//! further behind with every block? The simulation is deliberately pessimistic
//! about concurrency (one worker, arrivals in order) because that is what
//! `queue::Queue` is.

use serde::Serialize;

/// One block of a live pass: what it held, when it was complete, and what it
/// cost. `prep_s` is resample plus VAD, `decode_s` the sum of its turns'
/// decodes. `audio_s` is the block's *new* audio, so the column sums to the
/// track's length; speech carried forward across a block edge is counted once,
/// where it is decoded, while the cost of re-preparing it lands in `prep_s`.
#[derive(Debug, Clone, Serialize)]
pub struct BlockRow {
    pub index: usize,
    pub audio_s: f64,
    pub end_s: f64,
    pub prep_s: f64,
    pub decode_s: f64,
    pub turns: usize,
}

impl BlockRow {
    /// Everything the block cost: what the queue's worker is busy with.
    pub fn work_s(&self) -> f64 {
        self.prep_s + self.decode_s
    }
}

/// Realtime factor: seconds of work per second of audio, so below 1 is faster
/// than the audio arrives. `None` when there is no audio to divide by — a
/// missing number rather than an infinity that would print as a verdict.
pub fn rtf(work_s: f64, audio_s: f64) -> Option<f64> {
    (audio_s != 0.0).then(|| work_s / audio_s)
}

/// How far behind the audio a transcriber runs.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Lag {
    pub max_lag_s: f64,
    pub final_lag_s: f64,
}

/// The workload played `times` over, end to end, as a longer recording of the
/// same character. Whether the lag of the doubled workload matches the single
/// one is the whole test for a bounded backlog: a queue that keeps up has the
/// same lag on a track of any length, one that does not adds to it forever.
pub fn repeat(rows: &[BlockRow], times: usize) -> Vec<BlockRow> {
    let span = rows.last().map_or(0.0, |r| r.end_s);
    let mut out = Vec::with_capacity(rows.len() * times);
    for copy in 0..times {
        let offset = span * copy as f64;
        for r in rows {
            out.push(BlockRow {
                index: out.len(),
                end_s: r.end_s + offset,
                ..r.clone()
            });
        }
    }
    out
}

/// Play the blocks back as arrivals and see how far behind a single worker
/// gets. A block cannot be started before its audio exists, so it arrives at
/// `end_s`; `tracks` copies arrive at that same instant, because a room and a
/// call track are recorded at once; and one worker takes them in arrival order,
/// each for `prep_s + decode_s`. A block's lag is when the worker finishes it
/// minus when its audio was complete — what a user waits to see the line.
pub fn simulate_queue(rows: &[BlockRow], tracks: usize) -> Lag {
    let (mut free, mut max_lag_s, mut final_lag_s) = (0.0f64, 0.0f64, 0.0f64);
    for r in rows {
        for _ in 0..tracks {
            free = free.max(r.end_s) + r.work_s();
            final_lag_s = free - r.end_s;
            max_lag_s = max_lag_s.max(final_lag_s);
        }
    }
    Lag {
        max_lag_s,
        final_lag_s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtf_is_work_over_audio() {
        assert_eq!(rtf(1.5, 30.0), Some(0.05));
        assert_eq!(rtf(1.0, 0.0), None);
    }

    /// Two 10 s blocks costing 2 s of work each: the worker is idle when each
    /// one lands, so the lag is the work and nothing accumulates.
    #[test]
    fn a_worker_that_keeps_up_lags_by_one_block_of_work() {
        let rows = [row(0, 10.0, 10.0, 1.0, 1.0), row(1, 10.0, 20.0, 1.0, 1.0)];
        assert_eq!(simulate_queue(&rows, 1).max_lag_s, 2.0);
        // Two tracks arrive together; the second waits out the first.
        assert_eq!(simulate_queue(&rows, 2).max_lag_s, 4.0);
    }

    /// The same blocks at 12 s of work each: the first block's overrun is still
    /// being paid off when the second lands.
    #[test]
    fn a_worker_that_falls_behind_carries_the_overrun_forward() {
        let rows = [row(0, 10.0, 10.0, 2.0, 10.0), row(1, 10.0, 20.0, 2.0, 10.0)];
        let lag = simulate_queue(&rows, 1);
        assert_eq!(lag.max_lag_s, 14.0);
        assert_eq!(lag.final_lag_s, 14.0);
    }

    /// Doubling the workload is the test for a bounded backlog: 12 s of work
    /// per 10 s block adds 2 s every block and never stops adding it, while a
    /// 2 s block costs the same lag however long the track runs.
    #[test]
    fn repeat_shows_whether_the_backlog_is_bounded() {
        let slow = [row(0, 10.0, 10.0, 2.0, 10.0), row(1, 10.0, 20.0, 2.0, 10.0)];
        let doubled = repeat(&slow, 2);
        assert_eq!(doubled.len(), 4);
        assert_eq!(
            doubled.iter().map(|r| r.end_s).collect::<Vec<_>>(),
            vec![10.0, 20.0, 30.0, 40.0]
        );
        assert_eq!(
            doubled.iter().map(|r| r.index).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        // Lags 12, 14, 16, 18 — the backlog is still growing at the end.
        assert_eq!(simulate_queue(&doubled, 1).max_lag_s, 18.0);

        let fast = [row(0, 10.0, 10.0, 1.0, 1.0), row(1, 10.0, 20.0, 1.0, 1.0)];
        assert_eq!(simulate_queue(&repeat(&fast, 2), 1).max_lag_s, 2.0);
    }

    #[test]
    fn no_blocks_is_no_lag() {
        let lag = simulate_queue(&[], 1);
        assert_eq!(lag.max_lag_s, 0.0);
        assert_eq!(lag.final_lag_s, 0.0);
        assert!(repeat(&[], 2).is_empty());
    }

    fn row(index: usize, audio_s: f64, end_s: f64, prep_s: f64, decode_s: f64) -> BlockRow {
        BlockRow {
            index,
            audio_s,
            end_s,
            prep_s,
            decode_s,
            turns: 1,
        }
    }
}
