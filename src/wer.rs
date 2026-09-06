//! Word error rate: how far a transcript is from what was said.
//!
//! One number per recording, so an experiment on the pipeline can be judged
//! rather than argued about. The three edit counts are kept apart because they
//! do not mean the same thing to fix: deletions are usually the segmenter
//! throwing speech away, insertions are usually the recogniser inventing words
//! from noise, and substitutions are the acoustic model being wrong. A single
//! total would hide which of the three moved.
//!
//! Pure arithmetic — no I/O, no models — so it is cheap to test exhaustively
//! and gives the same answer on every machine.

/// The edit counts behind one score, and the reference length they are
/// measured against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Wer {
    pub substitutions: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub reference_words: usize,
}

impl Wer {
    /// `(S + I + D) / N`, the conventional word error rate. Not capped at 1.0:
    /// a recogniser that hallucinates can legitimately score above it, and
    /// clamping would hide exactly the failure worth seeing.
    pub fn rate(&self) -> f64 {
        if self.reference_words == 0 {
            // Nothing to divide by. Two empty sequences agree perfectly; a
            // hypothesis with no reference behind it is wholly wrong.
            return if self.insertions == 0 { 0.0 } else { 1.0 };
        }
        (self.substitutions + self.insertions + self.deletions) as f64 / self.reference_words as f64
    }
}

/// Lowercase, split on whitespace, and keep only letters, digits and
/// apostrophes within each word.
///
/// A hyphen splits nothing: `"well-known"` stays a single word and becomes
/// `"wellknown"`. Splitting it would count one reference word as two and
/// charge the recogniser an insertion for writing the compound the other way
/// round, which is a spelling convention, not a transcription error.
///
/// The typographic apostrophe `’` is folded to `'` first so that a recogniser
/// that punctuates prettily still matches a reference that does not; without
/// that, `"world’s"` would strip to `"worlds"` and score a substitution
/// against `"world's"`.
pub fn normalise(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .map(|c| if c == '\u{2019}' { '\'' } else { c })
                .filter(|c| c.is_alphanumeric() || *c == '\'')
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// Align the two word sequences and count the edits that separate them.
///
/// Word-level Levenshtein with the standard traceback. The distance alone
/// would give `S + I + D`; walking the table back recovers which of the three
/// each step was, so the counts are exact rather than inferred. Where several
/// alignments are equally short the traceback prefers a substitution, then a
/// deletion, then an insertion — an arbitrary but fixed order, so the same
/// pair of inputs always reports the same breakdown.
pub fn score(reference: &[String], hypothesis: &[String]) -> Wer {
    let (n, m) = (reference.len(), hypothesis.len());

    // d[i][j] is the edit distance between reference[..i] and hypothesis[..j].
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        d[i][0] = i;
        for j in 1..=m {
            let sub = d[i - 1][j - 1] + usize::from(reference[i - 1] != hypothesis[j - 1]);
            let del = d[i - 1][j] + 1;
            let ins = d[i][j - 1] + 1;
            d[i][j] = sub.min(del).min(ins);
        }
    }

    let mut wer = Wer {
        reference_words: n,
        ..Default::default()
    };
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let cost = usize::from(reference[i - 1] != hypothesis[j - 1]);
            if d[i][j] == d[i - 1][j - 1] + cost {
                wer.substitutions += cost;
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && d[i][j] == d[i - 1][j] + 1 {
            wer.deletions += 1;
            i -= 1;
            continue;
        }
        // Only reachable with j > 0: at j == 0 the row is d[i][0] == i, which
        // the deletion arm above always matches.
        wer.insertions += 1;
        j -= 1;
    }
    wer
}

/// Score `hypothesis` against the prefix of `reference` that best matches it,
/// rather than the whole reference.
///
/// For a reference that runs on past where the audio was cut — Earnings-21's
/// call transcripts cover the full call, but the fixture is only the first
/// 300 s of it — scoring against the whole reference would count every word
/// spoken after the cut as a deletion, drowning out what the recogniser
/// actually got wrong. This finds the prefix length `k` that minimises the
/// edit distance to the whole hypothesis, then scores normally against
/// `reference[..k]`.
///
/// The search is a rolling two-row Levenshtein DP: it needs `d[i][m]`, the
/// distance from each reference prefix to the full hypothesis, but never the
/// whole `n × m` table those come from — O(m) memory rather than O(n·m),
/// which matters when the reference is a hand-transcribed hour and the
/// hypothesis is a few hundred words. `score` is then called once, on the
/// winning prefix, to recover the exact substitution/insertion/deletion
/// breakdown from its own traceback.
///
/// Ties choose the smallest `k`: a longer prefix at the same distance only
/// enlarges the denominator without explaining any more of the hypothesis,
/// so the smaller one is the conservative reading.
pub fn score_prefix(reference: &[String], hypothesis: &[String]) -> Wer {
    let (n, m) = (reference.len(), hypothesis.len());

    let mut prev: Vec<usize> = (0..=m).collect();
    let mut best_k = 0;
    let mut best_dist = prev[m];

    let mut curr = vec![0usize; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let sub = prev[j - 1] + usize::from(reference[i - 1] != hypothesis[j - 1]);
            let del = prev[j] + 1;
            let ins = curr[j - 1] + 1;
            curr[j] = sub.min(del).min(ins);
        }
        if curr[m] < best_dist {
            best_dist = curr[m];
            best_k = i;
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    score(&reference[..best_k], hypothesis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_sequences_score_zero() {
        let words = normalise("the migration is scheduled for thursday morning");
        let wer = score(&words, &words);
        assert_eq!(wer.substitutions, 0);
        assert_eq!(wer.insertions, 0);
        assert_eq!(wer.deletions, 0);
        assert_eq!(wer.reference_words, 7);
        assert_eq!(wer.rate(), 0.0);
    }

    #[test]
    fn one_substitution() {
        let wer = score(&normalise("a b c d"), &normalise("a x c d"));
        assert_eq!(wer.substitutions, 1);
        assert_eq!(wer.insertions, 0);
        assert_eq!(wer.deletions, 0);
        assert_eq!(wer.rate(), 0.25);
    }

    #[test]
    fn one_insertion() {
        let wer = score(&normalise("a b c d"), &normalise("a b x c d"));
        assert_eq!(wer.substitutions, 0);
        assert_eq!(wer.insertions, 1);
        assert_eq!(wer.deletions, 0);
        assert_eq!(wer.rate(), 0.25);
    }

    #[test]
    fn one_deletion() {
        let wer = score(&normalise("a b c d"), &normalise("a c d"));
        assert_eq!(wer.substitutions, 0);
        assert_eq!(wer.insertions, 0);
        assert_eq!(wer.deletions, 1);
        assert_eq!(wer.rate(), 0.25);
    }

    /// The counts must be a breakdown of one alignment, not three independent
    /// guesses: a scorer that reports 2/1/1 for this would still sum to the
    /// right distance while pointing at the wrong problem.
    #[test]
    fn the_three_counts_are_exact_together() {
        let wer = score(
            &normalise("the quick brown fox jumps over"),
            &normalise("the quick red fox leaps right over"),
        );
        assert_eq!(wer.substitutions, 2);
        assert_eq!(wer.insertions, 1);
        assert_eq!(wer.deletions, 0);
        assert_eq!(wer.reference_words, 6);
        assert_eq!(wer.rate(), 0.5);
    }

    #[test]
    fn both_empty_is_perfect() {
        let wer = score(&[], &[]);
        assert_eq!(wer.reference_words, 0);
        assert_eq!(wer.rate(), 0.0);
    }

    #[test]
    fn hypothesis_without_a_reference_is_wholly_wrong() {
        let wer = score(&[], &normalise("words from nowhere"));
        assert_eq!(wer.insertions, 3);
        assert_eq!(wer.reference_words, 0);
        assert_eq!(wer.rate(), 1.0);
    }

    #[test]
    fn empty_hypothesis_loses_every_reference_word() {
        let wer = score(&normalise("a b c d"), &[]);
        assert_eq!(wer.deletions, 4);
        assert_eq!(wer.rate(), 1.0);
    }

    #[test]
    fn normalise_strips_punctuation_and_case() {
        assert_eq!(
            normalise("Hello, world's END."),
            ["hello", "world's", "end"]
        );
    }

    #[test]
    fn normalise_drops_words_that_are_only_punctuation() {
        assert_eq!(normalise("  yes -- no  "), ["yes", "no"]);
    }

    /// A hyphen is removed, not treated as a boundary: one word in, one out.
    #[test]
    fn normalise_keeps_a_hyphenated_word_whole() {
        assert_eq!(normalise("A WELL-KNOWN case"), ["a", "wellknown", "case"]);
    }

    #[test]
    fn normalise_folds_the_typographic_apostrophe() {
        assert_eq!(normalise("world\u{2019}s"), normalise("world's"));
    }

    /// A reference whose tail runs on past the hypothesis — the Earnings-21
    /// case, where the transcript covers the whole call and the audio is cut
    /// short — should not charge the tail as deletions once truncated.
    #[test]
    fn score_prefix_ignores_a_reference_tail_past_the_hypothesis() {
        let reference = normalise(
            "the quick brown fox jumps over the lazy dog and then a lot more that was never said",
        );
        let hypothesis = normalise("the quick brown fox jumps over the lazy dog");
        let wer = score_prefix(&reference, &hypothesis);
        assert_eq!(wer.substitutions, 0);
        assert_eq!(wer.insertions, 0);
        assert_eq!(wer.deletions, 0);
        assert_eq!(wer.reference_words, 9);
        assert_eq!(wer.rate(), 0.0);
    }

    /// An empty hypothesis against a non-empty reference: the best prefix is
    /// the empty one (`k == 0`), so `reference_words` is 0 and `rate()` takes
    /// the "nothing to divide by, no insertions" branch and reads 0.0 — not
    /// 1.0, which is only for a non-empty hypothesis matching no prefix.
    #[test]
    fn score_prefix_of_empty_hypothesis_is_the_empty_prefix() {
        let wer = score_prefix(&normalise("a b c d"), &[]);
        assert_eq!(wer.reference_words, 0);
        assert_eq!(wer.rate(), 0.0);
    }

    /// A non-empty hypothesis that matches nothing in the reference: every
    /// prefix length ties at the same distance (m insertions), so the
    /// smallest, `k == 0`, wins, and the hypothesis is wholly unexplained.
    #[test]
    fn score_prefix_of_an_unmatched_hypothesis_is_the_empty_prefix() {
        let wer = score_prefix(&normalise("a b c"), &normalise("x y z"));
        assert_eq!(wer.insertions, 3);
        assert_eq!(wer.reference_words, 0);
        assert_eq!(wer.rate(), 1.0);
    }

    /// When the hypothesis explains the whole reference best by using all of
    /// it, the full reference is itself the best prefix, so truncated and
    /// full scoring agree exactly.
    #[test]
    fn score_prefix_agrees_with_score_when_the_full_reference_is_best() {
        let reference = normalise("the quick brown fox jumps over");
        let hypothesis = normalise("the quick red fox leaps right over");
        assert_eq!(
            score_prefix(&reference, &hypothesis),
            score(&reference, &hypothesis)
        );
    }
}
