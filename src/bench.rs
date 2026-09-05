//! Queue arithmetic for the live-transcription benchmark.
//!
//! Pure arithmetic — no I/O, no models, no `std::time` — so it gives the same
//! answer on every machine, can be tested exhaustively, and a go/no-go
//! verdict rests on numbers a reader can recompute by hand rather than take
//! on faith.

/// Timings for one 30-second block of the benchmark replay.
///
/// `prep_s` is resample plus VAD for the block; `decode_s` is the sum of its
/// turns' decodes. Unit 2 (`src/bin/asrbench.rs`) fills these fields by
/// timing the real pipeline; this module only knows how to add them up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockRow {
    pub index: usize,
    pub audio_s: f64,
    pub end_s: f64,
    pub prep_s: f64,
    pub decode_s: f64,
    pub turns: usize,
}

/// Real-time factor: how much wall-clock work a block cost relative to the
/// audio it covers. `None` when `audio_s == 0.0`, since the ratio is
/// undefined rather than infinite.
pub fn rtf(work_s: f64, audio_s: f64) -> Option<f64> {
    if audio_s == 0.0 {
        None
    } else {
        Some(work_s / audio_s)
    }
}

/// The two numbers a queue simulation reports: how far behind the worker
/// ever fell, and how far behind it was when it finished.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lag {
    /// The largest `clock - arrival` seen over all jobs.
    pub max_lag_s: f64,
    /// The lag of the **last job processed** — with `tracks > 1` that is the
    /// last of the `tracks` copies of the last row, not the last row itself.
    /// Acceptance pins this value only at one track; the doc comment says so
    /// because unit 2 prints `tracks=2` pairs and must not guess which copy
    /// this field means.
    pub final_lag_s: f64,
}

/// Repeat a workload `times` times, back to back in simulated time.
///
/// Copy `k` (0-based) offsets every `end_s` by `k * rows.last().end_s`;
/// `audio_s`, `prep_s`, `decode_s` and `turns` are copied unchanged, because
/// the workload is being repeated in time, not scaled. `index` is renumbered
/// 0-based to the position in the returned `Vec`. `times == 0` or an empty
/// `rows` both give an empty `Vec`.
pub fn repeat(rows: &[BlockRow], times: usize) -> Vec<BlockRow> {
    let Some(last) = rows.last() else {
        return Vec::new();
    };
    let period = last.end_s;
    let mut out = Vec::with_capacity(rows.len() * times);
    for k in 0..times {
        let offset = k as f64 * period;
        for row in rows {
            out.push(BlockRow {
                index: out.len(),
                audio_s: row.audio_s,
                end_s: row.end_s + offset,
                prep_s: row.prep_s,
                decode_s: row.decode_s,
                turns: row.turns,
            });
        }
    }
    out
}

/// Simulate a single worker draining `tracks` copies of every block.
///
/// Each block arrives at its `end_s`; `tracks` copies of every block arrive
/// at that same instant. A single worker takes arrivals in arrival order —
/// rows in order, then each row's `tracks` copies together — and each job
/// costs `prep_s + decode_s`. So `clock = max(clock, arrival) + prep_s +
/// decode_s` and `lag = clock - arrival`. `max_lag_s` is the largest lag
/// over all jobs; `final_lag_s` is the lag of the last job processed, which
/// with `tracks > 1` is the last copy of the last row. An empty slice, or
/// `tracks == 0`, gives `Lag { max_lag_s: 0.0, final_lag_s: 0.0 }`.
pub fn simulate_queue(rows: &[BlockRow], tracks: usize) -> Lag {
    let mut clock = 0.0_f64;
    let mut max_lag_s = 0.0_f64;
    let mut final_lag_s = 0.0_f64;
    for row in rows {
        for _ in 0..tracks {
            let arrival = row.end_s;
            clock = clock.max(arrival) + row.prep_s + row.decode_s;
            let lag = clock - arrival;
            if lag > max_lag_s {
                max_lag_s = lag;
            }
            final_lag_s = lag;
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

    fn row(index: usize, audio_s: f64, end_s: f64, prep_s: f64, decode_s: f64) -> BlockRow {
        BlockRow {
            index,
            audio_s,
            end_s,
            prep_s,
            decode_s,
            turns: 0,
        }
    }

    #[test]
    fn rtf_divides() {
        assert_eq!(rtf(1.5, 30.0), Some(0.05));
    }

    #[test]
    fn rtf_none_when_no_audio() {
        assert_eq!(rtf(1.0, 0.0), None);
    }

    #[test]
    fn queue_light_load_one_track() {
        let rows = [row(0, 10.0, 10.0, 1.0, 1.0), row(1, 10.0, 20.0, 1.0, 1.0)];
        let lag = simulate_queue(&rows, 1);
        assert_eq!(lag.max_lag_s, 2.0);
    }

    #[test]
    fn queue_light_load_two_tracks() {
        let rows = [row(0, 10.0, 10.0, 1.0, 1.0), row(1, 10.0, 20.0, 1.0, 1.0)];
        let lag = simulate_queue(&rows, 2);
        assert_eq!(lag.max_lag_s, 4.0);
    }

    #[test]
    fn queue_heavy_load_one_track() {
        let rows = [row(0, 10.0, 10.0, 2.0, 10.0), row(1, 10.0, 20.0, 2.0, 10.0)];
        let lag = simulate_queue(&rows, 1);
        assert_eq!(lag.max_lag_s, 14.0);
        assert_eq!(lag.final_lag_s, 14.0);
    }

    #[test]
    fn repeat_heavy_load_grows_backlog() {
        let rows = [row(0, 10.0, 10.0, 2.0, 10.0), row(1, 10.0, 20.0, 2.0, 10.0)];
        let doubled = repeat(&rows, 2);
        let ends: Vec<f64> = doubled.iter().map(|r| r.end_s).collect();
        assert_eq!(ends, vec![10.0, 20.0, 30.0, 40.0]);
        let lag = simulate_queue(&doubled, 1);
        assert_eq!(lag.max_lag_s, 18.0);
    }

    #[test]
    fn repeat_light_load_stays_bounded() {
        let rows = [row(0, 10.0, 10.0, 1.0, 1.0), row(1, 10.0, 20.0, 1.0, 1.0)];
        let doubled = repeat(&rows, 2);
        let lag = simulate_queue(&doubled, 1);
        assert_eq!(lag.max_lag_s, 2.0);
    }

    #[test]
    fn empty_slice_is_zero() {
        let lag = simulate_queue(&[], 1);
        assert_eq!(
            lag,
            Lag {
                max_lag_s: 0.0,
                final_lag_s: 0.0
            }
        );
        assert!(repeat(&[], 2).is_empty());
    }
}
