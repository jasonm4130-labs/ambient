//! Word error rate. Substitutions, insertions and deletions are counted apart
//! because each points at a different fault: the acoustic model, a recogniser
//! inventing words from noise, or speech thrown away.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Wer {
    pub substitutions: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub reference_words: usize,
}

impl Wer {
    /// Not capped at 1.0: a hallucinating recogniser can exceed it, and
    /// clamping would hide exactly that.
    pub fn rate(&self) -> f64 {
        if self.reference_words == 0 {
            // Two empty sequences agree; a hypothesis with no reference is
            // wholly wrong.
            return if self.insertions == 0 { 0.0 } else { 1.0 };
        }
        (self.substitutions + self.insertions + self.deletions) as f64 / self.reference_words as f64
    }
}

/// A hyphen splits nothing (`"well-known"` becomes `"wellknown"`): writing a
/// compound the other way is spelling, not a transcription error. `’` folds to
/// `'` first so `"world’s"` still matches `"world's"`.
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

/// The traceback recovers exact S/I/D counts. Equal-length alignments prefer
/// substitution, then deletion, then insertion, so a pair of inputs always
/// reports the same breakdown.
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

/// Scores against the reference prefix that best matches `hypothesis`, so a
/// reference running past where the audio was cut (Earnings-21 covers the
/// whole call; the fixture is its first 300 s) does not count the tail as
/// deletions. The search keeps two DP rows, O(m) memory, because the reference
/// can be an hour of words. Ties take the smallest prefix: a longer one at the
/// same distance only inflates the denominator.
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

    /// The counts must come from one alignment: 2/1/1 here would sum to the
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

    #[test]
    fn normalise_keeps_a_hyphenated_word_whole() {
        assert_eq!(normalise("A WELL-KNOWN case"), ["a", "wellknown", "case"]);
    }

    #[test]
    fn normalise_folds_the_typographic_apostrophe() {
        assert_eq!(normalise("world\u{2019}s"), normalise("world's"));
    }

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

    /// `k == 0` leaves no reference words, so the rate is 0.0, not 1.0.
    #[test]
    fn score_prefix_of_empty_hypothesis_is_the_empty_prefix() {
        let wer = score_prefix(&normalise("a b c d"), &[]);
        assert_eq!(wer.reference_words, 0);
        assert_eq!(wer.rate(), 0.0);
    }

    /// Every prefix ties at m insertions, so the smallest, `k == 0`, wins.
    #[test]
    fn score_prefix_of_an_unmatched_hypothesis_is_the_empty_prefix() {
        let wer = score_prefix(&normalise("a b c"), &normalise("x y z"));
        assert_eq!(wer.insertions, 3);
        assert_eq!(wer.reference_words, 0);
        assert_eq!(wer.rate(), 1.0);
    }

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
