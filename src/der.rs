//! Diarization error rate: how far a speaker segmentation is from the truth.
//!
//! Missed speech, false alarm and confusion stay separate because each points
//! at a different fix: the segmenter dropping audio, hearing speech in noise,
//! or the clustering putting speech under the wrong speaker.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

/// The NIST md-eval resolution: a boundary costs at most 10 ms, and an hour
/// is 360,000 frames.
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
    /// Not capped at 1.0: false alarm over silence can exceed it. A zero
    /// reference gives infinity or NaN rather than 0.0, which would win every
    /// comparison in a sweep.
    pub fn rate(&self) -> f64 {
        (self.missed_s + self.false_alarm_s + self.confusion_s) / self.reference_s
    }
}

/// Parse NIST RTTM. Any line other than `SPEAKER`, a blank or a `;;` comment
/// is an error, so a 404 page saved to disk cannot score a perfect zero.
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

/// Score a hypothesis against a reference with the md-eval cardinality
/// formula, so an unmapped label over a reference speaker is one confusion
/// rather than a miss plus a false alarm. `collar_s` forgives reference
/// boundaries, where annotators disagree more than systems do.
pub fn score(reference: &[Turn], hypothesis: &[Turn], collar_s: f64) -> Result<Der> {
    if reference.is_empty() {
        bail!("empty reference: nothing to score against");
    }

    let end_s = reference
        .iter()
        .chain(hypothesis)
        .fold(0.0f64, |longest, turn| longest.max(turn.end_s));
    let frames = (end_s / FRAME_S).ceil() as usize;

    let reference_labels = labels(reference);
    let hypothesis_labels = labels(hypothesis);

    let reference_frames = occupancy(reference, &reference_labels, frames);
    let hypothesis_frames = occupancy(hypothesis, &hypothesis_labels, frames);

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

    // Frame counts, not seconds, so the assignment is exact integer arithmetic.
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

/// Sorted, so two runs over the same input map the same way.
fn labels(turns: &[Turn]) -> Vec<String> {
    let unique: BTreeMap<&str, ()> = turns.iter().map(|t| (t.speaker.as_str(), ())).collect();
    unique.into_keys().map(String::from).collect()
}

/// A frame belongs to a turn when its midpoint does, so back-to-back turns
/// never share a frame.
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

/// Solved exactly, not greedily: greedy takes the larger piece of a split
/// speaker first and can strand another reference speaker, inflating
/// confusion.
fn best_mapping(overlap: &[Vec<i64>], n_ref: usize, n_hyp: usize) -> Vec<Option<usize>> {
    let n = n_ref.max(n_hyp);
    if n == 0 {
        return Vec::new();
    }
    // Negated because the solver minimises.
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
            // A zero-overlap match is not evidence of anything.
            (h < n_hyp && overlap[r][h] > 0).then_some(h)
        })
        .collect()
}

/// Hungarian algorithm, O(n³) with dual potentials, on a square `cost`.
/// Column 0 is a sentinel holding the row being added, hence 1-indexing.
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

    /// A boundary off a frame edge is off by up to one 10 ms frame.
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
        // Optimal is A->Y, B->X (16 s correct); greedy takes A->X first.
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
