//! Parakeet TDT transducer: encoder -> greedy decode over decoder + joiner.
//!
//! TDT ("token-and-duration transducer") differs from a plain RNN-T in that the
//! joiner emits both a token distribution and a duration, and the duration says
//! how many encoder frames to skip. That is why decoding is far cheaper here
//! than a frame-by-frame RNN-T, and why an incorrect duration table shows up as
//! text that is fluent but wrongly paced rather than as an error.

use anyhow::{Context, Result};
use ort::session::Session;
use ort::value::Tensor;

/// `ort::Error` is neither `Send` nor `Sync`, so it cannot cross into `anyhow`
/// through `?`. Flatten it to a message at the boundary.
trait OrtExt<T> {
    fn a(self) -> Result<T>;
}
impl<T> OrtExt<T> for ort::Result<T> {
    fn a(self) -> Result<T> {
        self.map_err(|e| anyhow::anyhow!(e.to_string()))
    }
}

/// NeMo's default TDT duration bins for the 0.6b models.
const DURATIONS: [usize; 5] = [0, 1, 2, 3, 4];
const PRED_HIDDEN: usize = 640;
const PRED_LAYERS: usize = 2;
/// Guard against a duration-0 loop that never advances.
const MAX_SYMBOLS_PER_STEP: usize = 10;

pub struct Recognizer {
    encoder: Session,
    decoder: Session,
    joiner: Session,
    last_confidence: f32,
    tokens: Vec<String>,
    blank: usize,
}

struct DecoderState {
    out: Vec<f32>, // [PRED_HIDDEN]
    h: Vec<f32>,   // [PRED_LAYERS, 1, PRED_HIDDEN]
    c: Vec<f32>,
}

impl Recognizer {
    pub fn load(dir: &str) -> Result<Self> {
        let find = |prefix: &str| -> Result<String> {
            let mut hits: Vec<_> = std::fs::read_dir(dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(prefix) && n.ends_with(".onnx"))
                .collect();
            hits.sort();
            let name = hits
                .into_iter()
                .next()
                .with_context(|| format!("no {prefix}*.onnx in {dir}"))?;
            Ok(format!("{dir}/{name}"))
        };

        let build =
            |p: String| -> Result<Session> { Session::builder().a()?.commit_from_file(p).a() };

        let encoder = build(find("encoder")?)?;
        let decoder = build(find("decoder")?)?;
        let joiner = build(find("joiner")?)?;

        let tokens_path = format!("{dir}/tokens.txt");
        let tokens_txt =
            std::fs::read_to_string(&tokens_path).with_context(|| tokens_path.clone())?;
        let tokens = parse_tokens(&tokens_txt, &tokens_path)?;
        let blank = tokens.len() - 1;

        Ok(Self {
            encoder,
            decoder,
            joiner,
            tokens,
            blank,
            last_confidence: 0.0,
        })
    }

    /// One decoder step. `token == None` means the initial blank priming step.
    fn decode_step(&mut self, token: usize, prev: Option<&DecoderState>) -> Result<DecoderState> {
        let zeros = vec![0.0f32; PRED_LAYERS * PRED_HIDDEN];
        let (h_in, c_in) = match prev {
            Some(s) => (s.h.clone(), s.c.clone()),
            None => (zeros.clone(), zeros),
        };

        let targets = Tensor::from_array((vec![1_i64, 1], vec![token as i32])).a()?;
        let target_len = Tensor::from_array((vec![1_i64], vec![1_i32])).a()?;
        let h = Tensor::from_array((vec![PRED_LAYERS as i64, 1, PRED_HIDDEN as i64], h_in)).a()?;
        let c = Tensor::from_array((vec![PRED_LAYERS as i64, 1, PRED_HIDDEN as i64], c_in)).a()?;

        let out = self
            .decoder
            .run(ort::inputs![
                "targets" => &targets,
                "target_length" => &target_len,
                "states.1" => &h,
                "onnx::Slice_3" => &c,
            ])
            .a()?;

        let (_, o) = out["outputs"].try_extract_tensor::<f32>().a()?;
        let (_, h_new) = out["states"].try_extract_tensor::<f32>().a()?;
        let (_, c_new) = out["162"].try_extract_tensor::<f32>().a()?;

        Ok(DecoderState {
            out: o.to_vec(),
            h: h_new.to_vec(),
            c: c_new.to_vec(),
        })
    }

    /// Split on the quietest point near each target boundary so a cut lands
    /// between words rather than through one. Cheaper and steadier than
    /// overlap-and-merge, and good enough while chunks are this long.
    fn split_points(samples: &[f32], chunk: usize, search: usize) -> Vec<usize> {
        let mut cuts = vec![0usize];
        let mut pos = chunk;
        while pos < samples.len() {
            let lo = pos.saturating_sub(search);
            let hi = (pos + search).min(samples.len());
            let win = 1600; // 100 ms
            let mut best = pos;
            let mut best_energy = f32::MAX;
            let mut i = lo;
            while i + win < hi {
                let e: f32 = samples[i..i + win].iter().map(|v| v * v).sum();
                if e < best_energy {
                    best_energy = e;
                    best = i + win / 2;
                }
                i += 800;
            }
            cuts.push(best);
            pos = best + chunk;
        }
        cuts.push(samples.len());
        cuts
    }

    /// Transcribe using caller-supplied chunk boundaries — VAD segments, so
    /// cuts land in silence and silence itself is never sent to the encoder.
    pub fn transcribe_chunked(
        &mut self,
        samples: &[f32],
        chunks: &[crate::vad::Segment],
    ) -> Result<String> {
        let parts = self.transcribe_segments(samples, chunks)?;
        Ok(parts
            .into_iter()
            .map(|(_, text, _)| text)
            .collect::<Vec<_>>()
            .join(" "))
    }

    /// As [`transcribe_chunked`](Self::transcribe_chunked), but keeping each
    /// segment's boundaries alongside its text. A session transcript needs to
    /// know *when* something was said — to interleave two tracks, to seek the
    /// audio, and to give later layers a stable thing to point at — and joining
    /// into one string throws exactly that away.
    pub fn transcribe_segments(
        &mut self,
        samples: &[f32],
        chunks: &[crate::vad::Segment],
    ) -> Result<Vec<(crate::vad::Segment, String, f32)>> {
        let mut out = Vec::new();
        for c in chunks {
            let seg = &samples[c.start.min(samples.len())..c.end.min(samples.len())];
            if seg.len() < 1600 {
                continue;
            }
            let text = self.transcribe(seg)?;
            if !text.is_empty() {
                out.push((*c, text, self.last_confidence()));
            }
        }
        Ok(out)
    }

    /// Transcribe audio of any length, chunked so peak memory stays flat.
    pub fn transcribe_long(&mut self, samples: &[f32]) -> Result<String> {
        const CHUNK: usize = 30 * 16_000;
        const SEARCH: usize = 2 * 16_000;

        if samples.len() <= CHUNK + SEARCH {
            return self.transcribe(samples);
        }

        let cuts = Self::split_points(samples, CHUNK, SEARCH);
        let mut parts = Vec::new();
        for w in cuts.windows(2) {
            let seg = &samples[w[0]..w[1]];
            if seg.len() < 1600 {
                continue;
            }
            let text = self.transcribe(seg)?;
            if !text.is_empty() {
                parts.push(text);
            }
        }
        Ok(parts.join(" "))
    }

    /// Mean probability of the tokens emitted by the last `transcribe` call.
    /// Parakeet will invent fluent sentences from room noise, and no level or
    /// VAD threshold separates that from genuinely quiet speech — measured 2 dB
    /// apart on real recordings. How sure the decoder was is the signal that
    /// does discriminate.
    pub fn last_confidence(&self) -> f32 {
        self.last_confidence
    }

    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String> {
        let (feats, frames) = crate::features::log_mel(samples);
        // Nothing to encode. The encoder rejects a zero-length time axis, and
        // an empty segment is silence, which transcribes to nothing.
        if frames == 0 {
            self.last_confidence = 0.0;
            return Ok(String::new());
        }

        let signal = Tensor::from_array((
            vec![1_i64, crate::features::N_MELS as i64, frames as i64],
            feats,
        ))
        .a()?;
        let length = Tensor::from_array((vec![1_i64], vec![frames as i64])).a()?;

        let enc = self
            .encoder
            .run(ort::inputs!["audio_signal" => &signal, "length" => &length])
            .a()?;
        let (enc_shape, enc_data) = enc["outputs"].try_extract_tensor::<f32>().a()?;
        let dim = enc_shape[1] as usize; // 1024
        let t_max = enc_shape[2] as usize;
        let enc_data = enc_data.to_vec();
        drop(enc);

        // Prime the prediction network with a blank.
        let mut state = self.decode_step(self.blank, None)?;
        let mut emitted: Vec<usize> = Vec::new();
        let (mut conf_sum, mut conf_n) = (0.0f32, 0usize);

        let mut t = 0usize;
        while t < t_max {
            let mut symbols = 0;
            loop {
                // Encoder frame t as [1, 1024, 1].
                let frame: Vec<f32> = (0..dim).map(|d| enc_data[d * t_max + t]).collect();
                let ef = Tensor::from_array((vec![1_i64, dim as i64, 1], frame)).a()?;
                let df =
                    Tensor::from_array((vec![1_i64, PRED_HIDDEN as i64, 1], state.out.clone()))
                        .a()?;

                let j = self
                    .joiner
                    .run(ort::inputs![
                        "encoder_outputs" => &ef,
                        "decoder_outputs" => &df,
                    ])
                    .a()?;
                let (_, logits) = j["outputs"].try_extract_tensor::<f32>().a()?;

                let n_tok = self.tokens.len(); // 8193 incl. blank
                check_joiner(logits, n_tok, DURATIONS.len())?;
                let tok = crate::features::argmax(&logits[..n_tok]).context("empty logits")?;
                // Softmax only where it is needed: the winning token's share.
                let max = logits[tok];
                let denom: f32 = logits[..n_tok].iter().map(|l| (l - max).exp()).sum();
                let prob = 1.0 / denom;
                let dur_idx = crate::features::argmax(&logits[n_tok..n_tok + DURATIONS.len()])
                    .context("empty logits")?;
                let dur = DURATIONS[dur_idx];
                drop(j);

                if tok != self.blank {
                    conf_sum += prob;
                    conf_n += 1;
                    emitted.push(tok);
                    state = self.decode_step(tok, Some(&state))?;
                    symbols += 1;
                }

                // Advance. A zero duration with no emission would spin forever.
                if dur > 0 {
                    t += dur;
                    break;
                }
                if tok == self.blank || symbols >= MAX_SYMBOLS_PER_STEP {
                    t += 1;
                    break;
                }
            }
        }

        self.last_confidence = if conf_n > 0 {
            conf_sum / conf_n as f32
        } else {
            0.0
        };

        // SentencePiece: U+2581 marks a word boundary.
        let text: String = emitted
            .iter()
            .map(|&i| self.tokens[i].replace('\u{2581}', " "))
            .collect();
        Ok(text.trim().to_string())
    }
}

/// Split `tokens.txt` into its pieces, one per line.
///
/// The last token is the blank, so an empty file would make `tokens.len() - 1`
/// underflow and every later index land out of bounds. Fail here, naming the
/// file, rather than several thousand frames later.
pub fn parse_tokens(text: &str, source: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // "<piece> <id>", and the piece itself may be a space.
        let idx = line.rfind(' ').unwrap_or(line.len());
        tokens.push(line[..idx].to_string());
    }
    anyhow::ensure!(!tokens.is_empty(), "{source} is empty");
    Ok(tokens)
}

/// Check one joiner output before it is indexed.
///
/// The decode loop slices the token half and the duration half by position, so
/// a short tensor would read durations as tokens or panic; and one NaN turns
/// the softmax denominator — and with it every confidence — into NaN, which
/// propagates silently instead of stopping.
pub fn check_joiner(logits: &[f32], n_tok: usize, n_dur: usize) -> Result<()> {
    let want = n_tok + n_dur;
    anyhow::ensure!(
        logits.len() >= want,
        "joiner output has {} values, expected at least {want}",
        logits.len()
    );
    anyhow::ensure!(
        logits[..want].iter().all(|v| v.is_finite()),
        "joiner output is not finite"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_joiner_rejects_an_output_too_short_to_index() {
        let err = check_joiner(&[0.0; 5], 4, 2).unwrap_err().to_string();
        assert!(err.contains("expected at least 6"), "{err}");
    }

    #[test]
    fn check_joiner_rejects_a_non_finite_output() {
        let err = check_joiner(&[0.0, f32::NAN, 0.0], 2, 1)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not finite"), "{err}");
    }

    #[test]
    fn check_joiner_accepts_a_full_finite_output() {
        assert!(check_joiner(&[0.0; 6], 4, 2).is_ok());
    }

    #[test]
    fn parse_tokens_rejects_a_file_with_no_tokens() {
        for text in ["", "\n\n"] {
            let err = parse_tokens(text, "x/tokens.txt").unwrap_err().to_string();
            assert!(err.contains("x/tokens.txt is empty"), "{err}");
        }
    }

    /// The real file's layout: "<piece> <id>", where the piece may itself be a
    /// space — so the id is split off the right, never the left.
    #[test]
    fn parse_tokens_reads_a_piece_that_is_a_space() {
        let toks = parse_tokens("<unk> 0\n\u{2581}the 1\n  2\n", "x/tokens.txt").unwrap();
        assert_eq!(toks, ["<unk>", "\u{2581}the", " "]);
    }
}
