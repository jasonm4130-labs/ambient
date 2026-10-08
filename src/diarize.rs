//! Speaker diarization — who spoke when, within one track.
//!
//! Two models, in sequence. pyannote segmentation says how many people are
//! talking in each 10 s window and which parts of it belong to each of them,
//! but its speaker indices are local to that window and mean nothing across
//! windows. WeSpeaker then embeds each (window, local speaker) separately, and
//! clustering those embeddings over the whole recording is what recovers a
//! consistent identity — which is why no attempt is made to stitch windows
//! together by permutation.

use anyhow::{bail, Result};
use ort::session::Session;
use ort::value::Tensor;

const SR: usize = 16_000;
/// The segmentation model's `window_size`.
const WINDOW: usize = 160_000;
/// Its `receptive_field_shift`: samples covered by one output frame.
const SHIFT: usize = 270;
/// ~50% overlap, rounded down to a whole number of frames so window-local frame
/// indices map onto a single global grid without drift.
const HOP: usize = (WINDOW / 2 / SHIFT) * SHIFT;
const N_CLASSES: usize = 7;
const N_LOCAL: usize = 3;

/// The powerset the model was trained with: `num_speakers = 3`,
/// `powerset_max_classes = 2`, enumerated by subset size. Verified against a
/// clip with known silence rather than assumed — see `AMBIENT_DEBUG_DIAR`.
const POWERSET: [&[usize]; N_CLASSES] = [&[], &[0], &[1], &[2], &[0, 1], &[0, 2], &[1, 2]];

/// A local speaker needs at least this much audio in a window before its
/// embedding is worth clustering. Below it the vector is dominated by whatever
/// phonemes happened to land in the fragment rather than by the voice.
const MIN_EMBED: usize = SR;

/// A cluster holding less speech than this, in samples, is a candidate to be
/// folded into a bigger one — see `absorb_small`. Measured on whole AMI
/// meetings in docs/developing/measurements.md: 10 s to 15 s land on the same
/// speakers everywhere, so this sits mid-plateau rather than on an edge.
const MIN_CLUSTER: usize = 12 * SR;

/// The furthest, by cosine distance between cluster means, that a small
/// cluster may sit from the big one it is folded into. Further than this it
/// is more likely a real person who spoke briefly than a voice that drifted,
/// and it is kept. Measured alongside [`MIN_CLUSTER`].
const FOLD_DISTANCE: f32 = 0.8;

/// Cosine distance at which two clusters stop being the same person. Cheap to
/// retune because `diarize` appends rather than rewrites.
pub const DEFAULT_THRESHOLD: f32 = 0.5;

trait OrtExt<T> {
    fn a(self) -> Result<T>;
}
impl<T> OrtExt<T> for ort::Result<T> {
    fn a(self) -> Result<T> {
        self.map_err(|e| anyhow::anyhow!(e.to_string()))
    }
}

/// A stretch of one speaker's audio, in samples, labelled with a zero-based
/// speaker index local to this recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub speaker: usize,
}

struct Cand {
    /// Global frame indices this candidate is active in.
    frames: Vec<usize>,
    emb: Vec<f32>,
}

pub struct Diarizer {
    seg: Session,
    emb: Session,
}

impl Diarizer {
    pub fn load(segmentation: &str, embedding: &str) -> Result<Self> {
        Ok(Self {
            seg: Session::builder().a()?.commit_from_file(segmentation).a()?,
            emb: Session::builder().a()?.commit_from_file(embedding).a()?,
        })
    }

    /// Argmax powerset class per output frame, for exactly one window.
    fn classify(&mut self, window: &[f32]) -> Result<Vec<usize>> {
        let x = Tensor::from_array((vec![1_i64, 1, window.len() as i64], window.to_vec())).a()?;
        let out = self.seg.run(ort::inputs!["x" => &x]).a()?;
        let (shape, y) = out["y"].try_extract_tensor::<f32>().a()?;
        let frames = shape[1] as usize;
        let classes = shape[2] as usize;
        if classes != N_CLASSES {
            bail!("segmentation model emits {classes} classes, expected {N_CLASSES}");
        }
        Ok((0..frames)
            .map(|f| {
                let row = &y[f * classes..(f + 1) * classes];
                crate::features::argmax(row).unwrap_or(0)
            })
            .collect())
    }

    /// L2-normalised 256-d speaker embedding, so a dot product is the cosine.
    fn embed(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        let (feats, frames) = crate::fbank::fbank(samples);
        if frames == 0 {
            bail!("clip too short to embed");
        }
        let t = Tensor::from_array((
            vec![1_i64, frames as i64, crate::fbank::N_MELS as i64],
            feats,
        ))
        .a()?;
        let out = self.emb.run(ort::inputs!["feats" => &t]).a()?;
        let (_, e) = out["embs"].try_extract_tensor::<f32>().a()?;
        let norm = e.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-9);
        Ok(e.iter().map(|v| v / norm).collect())
    }

    /// Speaker spans over a whole 16 kHz mono track.
    pub fn diarize(&mut self, samples: &[f32], threshold: f32) -> Result<Vec<Span>> {
        if samples.len() < MIN_EMBED {
            return Ok(Vec::new());
        }
        let debug = std::env::var("AMBIENT_DEBUG_DIAR").is_ok();
        let mut cands: Vec<Cand> = Vec::new();

        let mut start = 0usize;
        loop {
            let end = (start + WINDOW).min(samples.len());
            let mut buf = vec![0.0f32; WINDOW];
            buf[..end - start].copy_from_slice(&samples[start..end]);
            let classes = self.classify(&buf)?;
            // Frames past the real audio are looking at the zero pad.
            let valid = ((end - start) / SHIFT).min(classes.len());
            let base = start / SHIFT;

            if debug {
                let mut hist = [0usize; N_CLASSES];
                for &c in &classes[..valid] {
                    hist[c] += 1;
                }
                eprintln!(
                    "  window {:.1}s–{:.1}s  frames {valid}  classes {hist:?}",
                    start as f64 / SR as f64,
                    end as f64 / SR as f64
                );
            }

            for spk in 0..N_LOCAL {
                let frames: Vec<usize> = (0..valid)
                    .filter(|&f| POWERSET[classes[f]].contains(&spk))
                    .map(|f| base + f)
                    .collect();
                if frames.len() * SHIFT < MIN_EMBED {
                    continue;
                }
                let mut audio = Vec::with_capacity(frames.len() * SHIFT);
                for &gf in &frames {
                    let a = gf * SHIFT;
                    let b = (a + SHIFT).min(samples.len());
                    if a < b {
                        audio.extend_from_slice(&samples[a..b]);
                    }
                }
                let emb = self.embed(&audio)?;
                cands.push(Cand { frames, emb });
            }

            if end == samples.len() {
                break;
            }
            start += HOP;
        }

        if cands.is_empty() {
            return Ok(Vec::new());
        }
        let embs: Vec<Vec<f32>> = cands.iter().map(|c| c.emb.clone()).collect();
        let n_frames = samples.len() / SHIFT + 2;
        let labels = cluster(&embs, threshold);
        let found = labels.iter().copied().max().unwrap_or(0) + 1;
        let held = speech_frames(&winners(&cands, &labels, n_frames), found);
        let labels = absorb_small(&embs, &labels, &held, MIN_CLUSTER / SHIFT, FOLD_DISTANCE);
        let n_clusters = labels.iter().copied().max().unwrap_or(0) + 1;
        if debug {
            let mut secs: Vec<f64> = held
                .iter()
                .map(|&h| (h * SHIFT) as f64 / SR as f64)
                .collect();
            secs.sort_by(|a, b| b.total_cmp(a));
            eprintln!(
                "  {} candidate(s) -> {found} cluster(s) holding {secs:.1?} s -> {n_clusters} speaker(s)",
                cands.len()
            );
        }

        let mut spans: Vec<Span> = Vec::new();
        for (f, k) in winners(&cands, &labels, n_frames).into_iter().enumerate() {
            let Some(k) = k else { continue };
            let a = f * SHIFT;
            let b = ((f + 1) * SHIFT).min(samples.len());
            if a >= b {
                continue;
            }
            match spans.last_mut() {
                Some(prev) if prev.speaker == k && prev.end == a => prev.end = b,
                _ => spans.push(Span {
                    start: a,
                    end: b,
                    speaker: k,
                }),
            }
        }
        Ok(spans)
    }
}

/// Which cluster owns each global frame. Windows overlap, so a frame can be
/// claimed twice; the candidates covering it vote, and a frame nobody claims
/// is `None`.
fn winners(cands: &[Cand], labels: &[usize], n_frames: usize) -> Vec<Option<usize>> {
    let n_clusters = labels.iter().copied().max().unwrap_or(0) + 1;
    let mut votes = vec![0u32; n_frames * n_clusters];
    for (cand, &k) in cands.iter().zip(labels) {
        for &f in &cand.frames {
            if f < n_frames {
                votes[f * n_clusters + k] += 1;
            }
        }
    }
    (0..n_frames)
        .map(|f| {
            let row = &votes[f * n_clusters..(f + 1) * n_clusters];
            let (k, &v) = row
                .iter()
                .enumerate()
                .max_by_key(|(_, &v)| v)
                .unwrap_or((0, &0));
            (v > 0).then_some(k)
        })
        .collect()
}

/// Frames each of `n` clusters ends up owning.
fn speech_frames(owners: &[Option<usize>], n: usize) -> Vec<usize> {
    let mut held = vec![0usize; n];
    for k in owners.iter().flatten() {
        held[*k] += 1;
    }
    held
}

/// Fold every cluster that owns fewer than `min_frames` of speech into the
/// nearest cluster that owns at least that much, by cosine distance between
/// the two clusters' mean embeddings — but only when that distance is at most
/// `max_distance`. A small cluster with no big one that close is kept.
///
/// Average linkage alone over-clusters a real meeting: across 25 minutes a
/// voice drifts with distance from the microphone, laughter and crosstalk,
/// and each excursion that clears the threshold becomes a "speaker" holding a
/// few seconds. A threshold high enough to stop that also merges real people.
/// Time held alone does not separate the two: a participant who speaks once
/// in a five-minute clip holds no more than an excursion does in half an hour.
/// Distance does the rest. An excursion is still that voice, so its mean sits
/// near the big cluster it drifted from; a brief real speaker's sits far from
/// every one of them. Both tests have to pass to fold. When no cluster
/// reaches the floor there is nothing to fold into and the clustering stands.
fn absorb_small(
    embs: &[Vec<f32>],
    labels: &[usize],
    held: &[usize],
    min_frames: usize,
    max_distance: f32,
) -> Vec<usize> {
    let big: Vec<usize> = (0..held.len()).filter(|&k| held[k] >= min_frames).collect();
    if big.is_empty() || big.len() == held.len() {
        return labels.to_vec();
    }
    let centroids: Vec<Vec<f32>> = (0..held.len()).map(|k| centroid(embs, labels, k)).collect();
    let target: Vec<usize> = (0..held.len())
        .map(|k| {
            if held[k] >= min_frames {
                return k;
            }
            let dot = |b: usize| -> f32 {
                centroids[k]
                    .iter()
                    .zip(&centroids[b])
                    .map(|(x, y)| x * y)
                    .sum()
            };
            let nearest = big
                .iter()
                .copied()
                .max_by(|&a, &b| dot(a).total_cmp(&dot(b)))
                .unwrap_or(k);
            if 1.0 - dot(nearest) <= max_distance {
                nearest
            } else {
                k
            }
        })
        .collect();
    let folded: Vec<usize> = labels.iter().map(|&l| target[l]).collect();
    first_appearance(&folded)
}

/// L2-normalised mean of cluster `k`'s embeddings.
fn centroid(embs: &[Vec<f32>], labels: &[usize], k: usize) -> Vec<f32> {
    let dim = embs.first().map_or(0, Vec::len);
    let mut c = vec![0.0f32; dim];
    for (e, _) in embs.iter().zip(labels).filter(|(_, &l)| l == k) {
        for (a, b) in c.iter_mut().zip(e) {
            *a += b;
        }
    }
    let norm = c.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-9);
    c.iter().map(|v| v / norm).collect()
}

/// Renumber labels in first-appearance order, so speaker 0 is whoever spoke
/// first rather than whichever row the matrix happened to keep.
fn first_appearance(owner: &[usize]) -> Vec<usize> {
    let mut map = std::collections::HashMap::new();
    owner
        .iter()
        .map(|&o| {
            let next = map.len();
            *map.entry(o).or_insert(next)
        })
        .collect()
}

/// Agglomerative clustering, average linkage, cosine distance. Merging stops
/// when the closest two clusters are further apart than `threshold`, so the
/// number of speakers is discovered rather than supplied.
fn cluster(embs: &[Vec<f32>], threshold: f32) -> Vec<usize> {
    let n = embs.len();
    if n == 0 {
        return Vec::new();
    }
    let mut d = vec![0.0f32; n * n];
    for i in 0..n {
        for j in 0..n {
            let dot: f32 = embs[i].iter().zip(&embs[j]).map(|(a, b)| a * b).sum();
            d[i * n + j] = 1.0 - dot;
        }
    }

    let mut active: Vec<bool> = vec![true; n];
    let mut size: Vec<f32> = vec![1.0; n];
    // Which original candidate belongs to which surviving cluster index.
    let mut owner: Vec<usize> = (0..n).collect();

    loop {
        let mut best = (0usize, 0usize, f32::MAX);
        for i in 0..n {
            if !active[i] {
                continue;
            }
            for j in (i + 1)..n {
                if active[j] && d[i * n + j] < best.2 {
                    best = (i, j, d[i * n + j]);
                }
            }
        }
        if best.2 > threshold {
            break;
        }
        let (i, j) = (best.0, best.1);
        // Lance-Williams update for average linkage, done in place.
        for k in 0..n {
            if !active[k] || k == i || k == j {
                continue;
            }
            let nd = (size[i] * d[i * n + k] + size[j] * d[j * n + k]) / (size[i] + size[j]);
            d[i * n + k] = nd;
            d[k * n + i] = nd;
        }
        size[i] += size[j];
        active[j] = false;
        for o in owner.iter_mut() {
            if *o == j {
                *o = i;
            }
        }
    }

    first_appearance(&owner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect()
    }

    #[test]
    fn powerset_covers_every_pair_once() {
        assert_eq!(POWERSET[0].len(), 0);
        let singles: Vec<_> = POWERSET.iter().filter(|s| s.len() == 1).collect();
        let pairs: Vec<_> = POWERSET.iter().filter(|s| s.len() == 2).collect();
        assert_eq!(singles.len(), N_LOCAL);
        assert_eq!(pairs.len(), 3);
    }

    #[test]
    fn hop_lands_on_a_frame_boundary() {
        assert_eq!(HOP % SHIFT, 0);
    }

    #[test]
    fn two_tight_groups_become_two_clusters() {
        let embs = vec![
            unit(&[1.0, 0.0, 0.02]),
            unit(&[0.98, 0.05, 0.0]),
            unit(&[0.0, 1.0, 0.03]),
            unit(&[0.02, 0.97, 0.0]),
        ];
        let labels = cluster(&embs, 0.5);
        assert_eq!(labels[0], labels[1]);
        assert_eq!(labels[2], labels[3]);
        assert_ne!(labels[0], labels[2]);
    }

    #[test]
    fn one_voice_stays_one_cluster() {
        let embs = vec![
            unit(&[1.0, 0.0, 0.0]),
            unit(&[0.99, 0.03, 0.0]),
            unit(&[0.98, 0.0, 0.05]),
        ];
        let labels = cluster(&embs, 0.5);
        assert_eq!(labels, vec![0, 0, 0]);
    }

    #[test]
    fn a_small_cluster_folds_into_the_nearest_big_one() {
        let embs = vec![
            unit(&[1.0, 0.0, 0.0]),
            unit(&[0.0, 1.0, 0.0]),
            unit(&[0.9, 0.0, 0.4]),
            unit(&[0.0, 1.0, 0.0]),
        ];
        // Cluster 2 holds a sliver of speech and sits nearest cluster 0.
        let labels = absorb_small(&embs, &[0, 1, 2, 1], &[500, 400, 20], 100, 0.8);
        assert_eq!(labels, vec![0, 1, 0, 1]);
    }

    #[test]
    fn a_small_cluster_far_from_every_big_one_is_kept() {
        let embs = vec![
            unit(&[1.0, 0.0, 0.0]),
            unit(&[0.0, 1.0, 0.0]),
            unit(&[0.0, 0.0, 1.0]),
        ];
        // Orthogonal to both big clusters: distance 1.0, past the limit, so
        // this is a brief speaker of its own rather than a drifted voice.
        let labels = absorb_small(&embs, &[0, 1, 2], &[500, 400, 20], 100, 0.8);
        assert_eq!(labels, vec![0, 1, 2]);
    }

    #[test]
    fn folding_renumbers_in_first_appearance_order() {
        let embs = vec![unit(&[0.0, 1.0]), unit(&[1.0, 0.0]), unit(&[0.1, 1.0])];
        // The first speaker heard is the small one, so after folding the
        // big cluster it joined has to become speaker 0.
        let labels = absorb_small(&embs, &[0, 1, 2], &[10, 900, 900], 100, 0.8);
        assert_eq!(labels, vec![0, 1, 0]);
    }

    #[test]
    fn nothing_folds_when_no_cluster_reaches_the_floor() {
        let embs = vec![unit(&[1.0, 0.0]), unit(&[1.0, 0.1])];
        assert_eq!(
            absorb_small(&embs, &[0, 1], &[10, 20], 100, 0.8),
            vec![0, 1]
        );
    }

    #[test]
    fn a_high_threshold_collapses_everything() {
        let embs = vec![unit(&[1.0, 0.0]), unit(&[0.0, 1.0])];
        assert_eq!(cluster(&embs, 1.5), vec![0, 0]);
    }
}
