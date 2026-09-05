//! Diarization error rate: how far a speaker segmentation is from the truth.
//!
//! One number per recording, so a change to the clustering can be judged
//! rather than argued about. The three components are kept apart because they
//! do not mean the same thing to fix: missed speech is the segmenter throwing
//! audio away, false alarm is it hearing speech in noise, and confusion is the
//! clustering putting the right speech under the wrong speaker. A single total
//! would hide which of the three moved.
//!
//! Speaker labels are arbitrary strings on both sides — `FEE013` and `X` name
//! the same person only by coincidence — so the two label sets are matched by
//! maximum-weight assignment before anything is counted. The match is exact
//! for any number of labels on either side, so an over-clustered candidate in
//! a sweep gets a number rather than an abort.
//!
//! Pure arithmetic — no I/O, no models — so it is cheap to test exhaustively
//! and gives the same answer on every machine.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

/// The scoring resolution. Ten milliseconds is the NIST md-eval convention:
/// fine enough that a frame boundary never costs more than 10 ms of error,
/// coarse enough that a one-hour meeting is 360,000 frames rather than
/// something that has to be scored by interval arithmetic.
const FRAME_S: f64 = 0.01;

/// One speaker speaking over one stretch of time.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub start_s: f64,
    pub end_s: f64,
    pub speaker: String,
}

/// The error seconds behind one score, and the reference duration they are
/// measured against.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Der {
    pub missed_s: f64,
    pub false_alarm_s: f64,
    pub confusion_s: f64,
    pub reference_s: f64,
}

impl Der {
    /// `(missed + false alarm + confusion) / reference`, the conventional
    /// diarization error rate. Not capped at 1.0: a system that invents
    /// speakers over silence can legitimately score above it, and clamping
    /// would hide exactly the failure worth seeing.
    pub fn rate(&self) -> f64 {
        if self.reference_s == 0.0 {
            return 0.0;
        }
        (self.missed_s + self.false_alarm_s + self.confusion_s) / self.reference_s
    }
}

/// Parse NIST RTTM, the format every diarization corpus ships its truth in.
///
/// Only `SPEAKER` lines carry turns; the fields that matter are the third and
/// fourth numbers (onset and duration, in seconds) and the eighth field (the
/// speaker label). Blank lines and `;;` comments are skipped.
///
/// Anything else is an error naming the line, deliberately: a download that
/// failed to a 404 page is still a file on disk, and silently parsing it to
/// zero turns would score a run against nothing and report a perfect zero.
pub fn parse_rttm(text: &str) -> Result<Vec<Turn>> {
    let mut turns = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with(";;") {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields[0] != "SPEAKER" {
            bail!("line {number}: not a SPEAKER line: {line}");
        }
        if fields.len() < 8 {
            bail!(
                "line {number}: expected at least 8 fields, found {}",
                fields.len()
            );
        }
        let start_s: f64 = fields[3]
            .parse()
            .with_context(|| format!("line {number}: onset {:?} is not a number", fields[3]))?;
        let duration_s: f64 = fields[4]
            .parse()
            .with_context(|| format!("line {number}: duration {:?} is not a number", fields[4]))?;
        turns.push(Turn {
            start_s,
            end_s: start_s + duration_s,
            speaker: fields[7].to_string(),
        });
    }
    Ok(turns)
}

/// Score a hypothesis segmentation against a reference one.
///
/// Both sides are discretised into [`FRAME_S`] frames covering the longer of
/// the two, a frame within `collar_s` of any reference turn boundary is
/// dropped (annotators disagree about where a turn starts by more than a
/// system does), and the remaining frames are counted with the md-eval
/// cardinality formula: per frame, missed is the reference speakers the
/// hypothesis has no room for, false alarm is the hypothesis speakers the
/// reference has no room for, and everything else that fails to match is
/// confusion. Counting cardinalities rather than pairs is what makes an
/// unmapped label speaking over a reference speaker one error (confusion)
/// rather than two (a miss plus a false alarm).
///
/// Errors only when the reference is empty: there is nothing to be a
/// proportion of, and returning 0.0 would report a perfect score for a run
/// with no truth behind it.
pub fn score(reference: &[Turn], hypothesis: &[Turn], collar_s: f64) -> Result<Der> {
    if reference.is_empty() {
        bail!("empty reference: nothing to score against");
    }

    let end_s = reference
        .iter()
        .chain(hypothesis)
        .fold(0.0f64, |longest, turn| longest.max(turn.end_s));
    let frames = (end_s / FRAME_S).ceil() as usize;

    // Labels get dense indices so a frame's speaker set is a bitmap-ish Vec
    // rather than a set of strings.
    let reference_labels = labels(reference);
    let hypothesis_labels = labels(hypothesis);

    let reference_frames = occupancy(reference, &reference_labels, frames);
    let hypothesis_frames = occupancy(hypothesis, &hypothesis_labels, frames);

    // A frame is scored unless its midpoint sits within the collar of a
    // reference turn boundary — start or end, both are uncertain.
    let mut included = vec![true; frames];
    if collar_s > 0.0 {
        for turn in reference {
            for (frame, keep) in included.iter_mut().enumerate() {
                let mid_s = (frame as f64 + 0.5) * FRAME_S;
                if (mid_s - turn.start_s).abs() < collar_s || (mid_s - turn.end_s).abs() < collar_s
                {
                    *keep = false;
                }
            }
        }
    }

    // Overlap matrix: how many included frames each reference label shares
    // with each hypothesis label. Frame counts, not seconds, so the assignment
    // below is exact integer arithmetic.
    let mut overlap = vec![vec![0i64; hypothesis_labels.len()]; reference_labels.len()];
    for frame in 0..frames {
        if !included[frame] {
            continue;
        }
        for r in 0..reference_labels.len() {
            if !reference_frames[r][frame] {
                continue;
            }
            for h in 0..hypothesis_labels.len() {
                if hypothesis_frames[h][frame] {
                    overlap[r][h] += 1;
                }
            }
        }
    }
    let mapping = best_mapping(&overlap, reference_labels.len(), hypothesis_labels.len());

    let mut der = Der::default();
    for frame in 0..frames {
        if !included[frame] {
            continue;
        }
        let mut n_ref = 0usize;
        let mut n_correct = 0usize;
        for r in 0..reference_labels.len() {
            if !reference_frames[r][frame] {
                continue;
            }
            n_ref += 1;
            if let Some(h) = mapping[r] {
                if hypothesis_frames[h][frame] {
                    n_correct += 1;
                }
            }
        }
        let n_hyp = (0..hypothesis_labels.len())
            .filter(|h| hypothesis_frames[*h][frame])
            .count();

        der.missed_s += n_ref.saturating_sub(n_hyp) as f64 * FRAME_S;
        der.false_alarm_s += n_hyp.saturating_sub(n_ref) as f64 * FRAME_S;
        der.confusion_s += (n_ref.min(n_hyp) - n_correct) as f64 * FRAME_S;
        der.reference_s += n_ref as f64 * FRAME_S;
    }
    Ok(der)
}

/// The distinct speaker labels of a turn list, in a fixed order so that two
/// runs over the same input map the same way.
fn labels(turns: &[Turn]) -> Vec<String> {
    let unique: BTreeMap<&str, ()> = turns.iter().map(|t| (t.speaker.as_str(), ())).collect();
    unique.into_keys().map(String::from).collect()
}

/// `occupancy[label][frame]` — whether that label is speaking in that frame.
/// A frame belongs to a turn when its midpoint does, which keeps a turn that
/// ends where the next begins from counting in both.
fn occupancy(turns: &[Turn], labels: &[String], frames: usize) -> Vec<Vec<bool>> {
    let mut occupancy = vec![vec![false; frames]; labels.len()];
    for turn in turns {
        let Some(index) = labels.iter().position(|label| *label == turn.speaker) else {
            continue;
        };
        let first = ((turn.start_s / FRAME_S - 0.5).ceil().max(0.0)) as usize;
        for (frame, occupied) in occupancy[index].iter_mut().enumerate().skip(first) {
            let mid_s = (frame as f64 + 0.5) * FRAME_S;
            if mid_s >= turn.end_s {
                break;
            }
            *occupied = true;
        }
    }
    occupancy
}

/// The reference label -> hypothesis label map that puts the most speech on
/// the right speaker, `None` for a reference label left unmatched.
///
/// Greedy matching is wrong here and the failure is not exotic: given a
/// reference speaker split across two clusters, greedy takes the larger piece
/// first and can strand a second reference speaker with nothing, inflating
/// confusion. So the overlap matrix is padded to square with zeros and solved
/// exactly.
fn best_mapping(overlap: &[Vec<i64>], n_ref: usize, n_hyp: usize) -> Vec<Option<usize>> {
    let n = n_ref.max(n_hyp);
    if n == 0 {
        return Vec::new();
    }
    // Padded to square, and negated because the solver minimises.
    let mut cost = vec![vec![0i64; n]; n];
    for (r, row) in overlap.iter().enumerate() {
        for (h, weight) in row.iter().enumerate() {
            cost[r][h] = -weight;
        }
    }
    let assignment = hungarian(&cost);
    (0..n_ref)
        .map(|r| {
            let h = assignment[r];
            // A padded column is not a real label, and a zero-overlap match is
            // not evidence of anything.
            (h < n_hyp && overlap[r][h] > 0).then_some(h)
        })
        .collect()
}

/// The Hungarian algorithm (Kuhn–Munkres), in the O(n³) shortest-augmenting-
/// path form with dual potentials: for each row in turn, grow a Dijkstra-like
/// tree over unvisited columns, raising the potentials by the slack of the
/// cheapest edge until it reaches an unassigned column, then flip the
/// alternating path. `cost` must be square; the return is the column chosen
/// for each row, minimising the total.
///
/// Column 0 of the internal arrays is a sentinel holding the row currently
/// being added, which is why everything below is 1-indexed.
fn hungarian(cost: &[Vec<i64>]) -> Vec<usize> {
    const INFINITY: i64 = i64::MAX / 4;
    let n = cost.len();
    let mut row_potential = vec![0i64; n + 1];
    let mut column_potential = vec![0i64; n + 1];
    let mut row_of_column = vec![0usize; n + 1];
    let mut previous_column = vec![0usize; n + 1];

    for row in 1..=n {
        row_of_column[0] = row;
        let mut column = 0usize;
        let mut slack = vec![INFINITY; n + 1];
        let mut visited = vec![false; n + 1];
        loop {
            visited[column] = true;
            let current_row = row_of_column[column];
            let mut delta = INFINITY;
            let mut next_column = 0usize;
            for candidate in 1..=n {
                if visited[candidate] {
                    continue;
                }
                let reduced = cost[current_row - 1][candidate - 1]
                    - row_potential[current_row]
                    - column_potential[candidate];
                if reduced < slack[candidate] {
                    slack[candidate] = reduced;
                    previous_column[candidate] = column;
                }
                if slack[candidate] < delta {
                    delta = slack[candidate];
                    next_column = candidate;
                }
            }
            for candidate in 0..=n {
                if visited[candidate] {
                    row_potential[row_of_column[candidate]] += delta;
                    column_potential[candidate] -= delta;
                } else {
                    slack[candidate] -= delta;
                }
            }
            column = next_column;
            if row_of_column[column] == 0 {
                break;
            }
        }
        // Walk the augmenting path back, shifting each column to the row that
        // reached it.
        while column != 0 {
            let source = previous_column[column];
            row_of_column[column] = row_of_column[source];
            column = source;
        }
    }

    let mut assignment = vec![0usize; n];
    for column in 1..=n {
        assignment[row_of_column[column] - 1] = column - 1;
    }
    assignment
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(speaker: &str, start_s: f64, end_s: f64) -> Turn {
        Turn {
            start_s,
            end_s,
            speaker: speaker.to_string(),
        }
    }

    /// Seconds compare loosely: the scorer counts 10 ms frames, so a boundary
    /// that does not land on a frame edge is off by at most one frame.
    fn close(actual: f64, expected: f64) -> bool {
        (actual - expected).abs() < 0.05
    }

    #[test]
    fn parse_rttm_reads_a_speaker_line() {
        let turns = parse_rttm("SPEAKER ES2004a 1 12.34 5.00 <NA> <NA> FEE013 <NA> <NA>").unwrap();
        assert_eq!(turns.len(), 1);
        assert!(close(turns[0].start_s, 12.34));
        assert!(close(turns[0].end_s, 17.34));
        assert_eq!(turns[0].speaker, "FEE013");
    }

    #[test]
    fn parse_rttm_skips_blanks_and_comments() {
        let text = ";; a comment\n\nSPEAKER ES2004a 1 0.00 1.00 <NA> <NA> A <NA> <NA>\n\n";
        let turns = parse_rttm(text).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].speaker, "A");
    }

    #[test]
    fn parse_rttm_rejects_a_non_speaker_line() {
        // An HTML error page saved as an RTTM must fail, not parse to nothing.
        let text = "<!DOCTYPE html>\n<html><body>404</body></html>\n";
        let err = parse_rttm(text).unwrap_err().to_string();
        assert!(err.contains("line 1"), "{err}");
    }

    #[test]
    fn parse_rttm_rejects_a_short_line() {
        let text = "SPEAKER ES2004a 1 0.00 1.00 <NA> <NA> A <NA> <NA>\nSPEAKER ES2004a 1 2.00\n";
        let err = parse_rttm(text).unwrap_err().to_string();
        assert!(err.contains("line 2"), "{err}");
    }

    #[test]
    fn identical_reference_and_hypothesis_score_zero() {
        let reference = vec![turn("A", 0.0, 10.0), turn("B", 10.0, 20.0)];
        let der = score(&reference, &reference, 0.0).unwrap();
        assert_eq!(der.rate(), 0.0);
    }

    #[test]
    fn empty_hypothesis_is_all_missed() {
        let der = score(&[turn("A", 0.0, 10.0)], &[], 0.0).unwrap();
        assert!(close(der.missed_s, 10.0), "{:?}", der);
    }

    #[test]
    fn empty_reference_is_an_error() {
        let err = score(&[], &[turn("A", 0.0, 4.0)], 0.0)
            .unwrap_err()
            .to_string();
        assert!(err.contains("empty reference"), "{err}");
    }

    #[test]
    fn labels_are_mapped_not_compared() {
        let reference = vec![turn("A", 0.0, 10.0), turn("B", 10.0, 20.0)];
        let hypothesis = vec![turn("X", 0.0, 10.0), turn("Y", 10.0, 20.0)];
        let der = score(&reference, &hypothesis, 0.0).unwrap();
        assert_eq!(der.rate(), 0.0);
    }

    #[test]
    fn merged_speakers_are_confusion() {
        let reference = vec![turn("A", 0.0, 10.0), turn("B", 10.0, 20.0)];
        let hypothesis = vec![turn("X", 0.0, 20.0)];
        let der = score(&reference, &hypothesis, 0.0).unwrap();
        assert!(close(der.confusion_s, 10.0), "{:?}", der);
    }

    #[test]
    fn over_clustering_uses_the_optimal_map() {
        // The optimal map is A->Y, B->X for 16 s correct. A greedy assignment
        // takes A->X first (9 s) and leaves B nothing, scoring 16 s confusion.
        let reference = vec![turn("A", 0.0, 17.0), turn("B", 17.0, 25.0)];
        let hypothesis = vec![
            turn("X", 0.0, 9.0),
            turn("Y", 9.0, 17.0),
            turn("X", 17.0, 25.0),
        ];
        let der = score(&reference, &hypothesis, 0.0).unwrap();
        assert!(close(der.confusion_s, 9.0), "{:?}", der);
    }

    #[test]
    fn many_hypothesis_labels_still_score() {
        let reference = vec![turn("A", 0.0, 10.0), turn("B", 10.0, 20.0)];
        let hypothesis: Vec<Turn> = (0..12)
            .map(|i| turn(&format!("H{i}"), f64::from(i), f64::from(i) + 1.0))
            .collect();
        let der = score(&reference, &hypothesis, 0.0).unwrap();
        // One label maps to A and one to B, so 2 s is correct.
        assert!(close(der.confusion_s, 10.0), "{:?}", der);
        assert!(close(der.missed_s, 8.0), "{:?}", der);
        assert!(close(der.false_alarm_s, 0.0), "{:?}", der);
    }

    #[test]
    fn collar_forgives_a_boundary_shift() {
        let reference = vec![turn("A", 0.0, 10.0)];
        let hypothesis = vec![turn("A", 0.2, 10.0)];
        assert_eq!(score(&reference, &hypothesis, 0.25).unwrap().rate(), 0.0);
        let uncollared = score(&reference, &hypothesis, 0.0).unwrap();
        assert!(close(uncollared.missed_s, 0.2), "{:?}", uncollared);
    }
}
