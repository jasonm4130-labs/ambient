//! Session to transcript via `ambient-engine` (ADR 0020). Each stage's output
//! is a checkpoint under `stages/`, reused while its key (track size, mtime,
//! model) matches, so a job that dies resumes at its first unfinished stage.
//!
//! Assembly writes `raw.jsonl`, `edits.jsonl`, then `transcript.md`, each
//! atomically and `transcript.md` last: its existence is what "finished" means
//! to the launch re-queue and the audio sweep.

use anyhow::{anyhow, bail, Context, Result};
use serde::de::DeserializeOwned;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::engine::{self, SpeakersFile, Stage, TranscriptFile, Turn, Word};
use crate::session::{Edit, Meter, MeterPhase, RawRecord, Target, Track, DIARIZE_BY, STATUS_FILE};

/// Must match `engine/Sources/AmbientEngine/main.swift`; a test reads it.
pub const ASR_MODEL: &str = "parakeet-ultra/fluidaudio-e2ea498";
pub const DIAR_MODEL: &str = "nemotron3-fast32/fluidaudio-e2ea498";
/// Layout under the models root, as `ambient-engine fetch` writes it.
pub const ASR_DIR: &str = "parakeet-ultra";
pub const DIAR_DIR: &str = "nemotron-3-diarization/monolithic/v2";

pub const STAGES_DIR: &str = "stages";
/// Shared so a later labelled run can clear the warning and the audio sweep
/// can spare that session's audio.
pub const UNLABELLED_WARNING: &str = "voices were not separated";
pub const ATTEMPTS_FILE: &str = "attempts";
/// Names the `previous/` archive whose typed names this run carries.
pub const CARRY_FILE: &str = "carry-from";
pub const PREVIOUS_DIR: &str = "previous";

pub const RETRY_CAP: u32 = 2;

/// Nemotron sometimes splits a voice into a brief second speaker. Dropping
/// speakers under this moved exact speaker counts from 15/16 to 16/16 (AMI)
/// and 50/57 to 57/57 (PriMock57) on issue #168's benchmark, with word
/// attribution unchanged.
pub const PHANTOM_SECONDS: f64 = 5.0;

const PAUSE_SECONDS: f64 = 1.0;
/// Past this a line ends at the next sentence end...
const SOFT_LINE_SECONDS: f64 = 20.0;
/// ...and past this one it ends regardless.
const HARD_LINE_SECONDS: f64 = 40.0;

pub struct Engine {
    pub binary: PathBuf,
    pub models: PathBuf,
}

impl Engine {
    pub fn locate() -> Result<Engine> {
        Ok(Engine {
            binary: engine::locate()?,
            models: crate::session::models_root()?,
        })
    }

    /// Runs before any stage, so a missing model fails as the machine's fault,
    /// not the session's.
    pub fn check(&self) -> Result<()> {
        for dir in [ASR_DIR, DIAR_DIR] {
            let p = self.models.join(dir);
            if !p.is_dir() {
                bail!("no model at {} — run ./fetch-models.sh", p.display());
            }
        }
        Ok(())
    }

    /// No session and no checkpoint: for the accuracy harnesses, which must
    /// measure the models as the app runs them.
    pub fn once<T: DeserializeOwned>(&self, stage: Stage, wav: &Path, out: &Path) -> Result<T> {
        std::fs::remove_file(out).ok();
        engine::run(
            &self.binary,
            stage,
            &self.models,
            wav,
            out,
            "harness",
            |_| {},
        )?;
        let text =
            std::fs::read_to_string(out).with_context(|| format!("reading {}", out.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", out.display()))
    }
}

pub fn key(audio: &Path, model: &str) -> Result<String> {
    let m = std::fs::metadata(audio).with_context(|| format!("reading {}", audio.display()))?;
    let mtime = m
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(format!("{model}|{}|{mtime}", m.len()))
}

trait Keyed {
    fn key(&self) -> &str;
}
impl Keyed for TranscriptFile {
    fn key(&self) -> &str {
        &self.key
    }
}
impl Keyed for SpeakersFile {
    fn key(&self) -> &str {
        &self.key
    }
}

fn load<T: DeserializeOwned + Keyed>(path: &Path, key: &str) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    let t: T = serde_json::from_str(&text).ok()?;
    (t.key() == key).then_some(t)
}

fn stage_path(dir: &Path, track: Track, stage: Stage) -> PathBuf {
    dir.join(STAGES_DIR)
        .join(format!("{}.{}.json", track.as_str(), stage.as_str()))
}

fn stage<T: DeserializeOwned + Keyed>(
    engine: &Engine,
    dir: &Path,
    track: Track,
    wav: &Path,
    stage: Stage,
    model: &str,
) -> Result<T> {
    let key = key(wav, model)?;
    let out = stage_path(dir, track, stage);
    if let Some(t) = load(&out, &key) {
        eprintln!("  {} {} — from checkpoint", track.as_str(), stage.as_str());
        return Ok(t);
    }
    std::fs::create_dir_all(out.parent().expect("stage path has a parent"))?;
    engine::run(
        &engine.binary,
        stage,
        &engine.models,
        wav,
        &out,
        &key,
        |_| {},
    )?;
    load(&out, &key).ok_or_else(|| {
        anyhow!(
            "{} {} finished without a usable {}",
            engine::ENGINE_NAME,
            stage.as_str(),
            out.display()
        )
    })
}

/// If nobody reaches the threshold (a short recording), the speaker with the
/// most speech stays.
fn kept_speakers(turns: &[Turn]) -> HashSet<usize> {
    let mut held: HashMap<usize, f64> = HashMap::new();
    for t in turns {
        *held.entry(t.speaker).or_default() += (t.end - t.start).max(0.0);
    }
    let mut kept: HashSet<usize> = held
        .iter()
        .filter(|(_, &s)| s >= PHANTOM_SECONDS)
        .map(|(&k, _)| k)
        .collect();
    if kept.is_empty() {
        if let Some((&k, _)) = held
            .iter()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap().then(b.0.cmp(a.0)))
        {
            kept.insert(k);
        }
    }
    kept
}

fn speaker_for(start: f64, end: f64, turns: &[Turn]) -> Option<usize> {
    let mut overlap: HashMap<usize, f64> = HashMap::new();
    for t in turns {
        let o = end.min(t.end) - start.max(t.start);
        if o > 0.0 {
            *overlap.entry(t.speaker).or_default() += o;
        }
    }
    if let Some((&s, _)) = overlap
        .iter()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap().then(b.0.cmp(a.0)))
    {
        return Some(s);
    }
    turns
        .iter()
        .map(|t| {
            let gap = if end < t.start {
                t.start - end
            } else {
                (start - t.end).max(0.0)
            };
            (gap, t.speaker)
        })
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(a.1.cmp(&b.1)))
        .map(|(_, s)| s)
}

fn ends_sentence(text: &str) -> bool {
    text.ends_with(['.', '?', '!'])
}

/// Speaker indices count from 0 in order of first appearance. Lines also break
/// at pauses, because a call track is mostly one voice. `start_ms` is unique
/// within the track because edits address lines by it.
pub fn assemble(
    track: Track,
    words: &[Word],
    turns: Option<&[Turn]>,
) -> (Vec<RawRecord>, Vec<Option<usize>>) {
    let kept_turns: Vec<Turn> = match turns {
        Some(turns) => {
            let kept = kept_speakers(turns);
            turns
                .iter()
                .filter(|t| kept.contains(&t.speaker))
                .copied()
                .collect()
        }
        None => Vec::new(),
    };

    let mut groups: Vec<(Vec<&Word>, Option<usize>)> = Vec::new();
    for w in words.iter().filter(|w| !w.text.trim().is_empty()) {
        let spk = speaker_for(w.start, w.end, &kept_turns);
        let split = match groups.last() {
            None => true,
            Some((g, s)) => {
                let first = g[0];
                let last = g[g.len() - 1];
                let length = w.end - first.start;
                *s != spk
                    || w.start - last.end >= PAUSE_SECONDS
                    || (length >= SOFT_LINE_SECONDS && ends_sentence(&last.text))
                    || length >= HARD_LINE_SECONDS
            }
        };
        if split {
            groups.push((vec![w], spk));
        } else {
            groups.last_mut().unwrap().0.push(w);
        }
    }

    let mut order: Vec<usize> = Vec::new();
    let mut records = Vec::with_capacity(groups.len());
    let mut speakers = Vec::with_capacity(groups.len());
    let mut last_start: Option<u64> = None;
    for (g, spk) in groups {
        let mut start_ms = (g[0].start * 1000.0).round().max(0.0) as u64;
        if let Some(prev) = last_start {
            if start_ms <= prev {
                start_ms = prev + 1;
            }
        }
        last_start = Some(start_ms);
        let end_ms = ((g[g.len() - 1].end * 1000.0).round().max(0.0) as u64).max(start_ms);
        let text = g
            .iter()
            .map(|w| w.text.trim())
            .collect::<Vec<_>>()
            .join(" ");
        let confidence = g.iter().map(|w| w.confidence).sum::<f32>() / g.len() as f32;
        records.push(RawRecord {
            track,
            start_ms,
            end_ms,
            text,
            confidence,
        });
        speakers.push(spk.map(|s| match order.iter().position(|&o| o == s) {
            Some(i) => i,
            None => {
                order.push(s);
                order.len() - 1
            }
        }));
    }
    (records, speakers)
}

pub fn label(track: Track, speaker: usize) -> String {
    format!("{}-{}", track.as_str(), speaker + 1)
}

/// After reverts, the last live `Speaker` edit for a line wins.
fn current_speakers(edits: &[Edit]) -> HashMap<(Track, u64), (String, String)> {
    let reverted: HashSet<usize> = edits
        .iter()
        .filter_map(|e| match e {
            Edit::Revert { target_seq, .. } => Some(*target_seq),
            _ => None,
        })
        .collect();
    let mut out = HashMap::new();
    for (seq, e) in edits.iter().enumerate() {
        if reverted.contains(&seq) {
            continue;
        }
        if let Edit::Speaker {
            target, name, by, ..
        } = e
        {
            out.insert((target.track, target.start_ms), (name.clone(), by.clone()));
        }
    }
    out
}

fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).with_context(|| format!("parsing {}", path.display())))
        .collect()
}

/// Each typed name goes to the new speaker whose lines overlap the old named
/// lines most, one name per speaker, largest overlap first. Text repairs are
/// not carried: their words may be gone, and the archive keeps them.
pub fn carry_names(
    old_raw: &[RawRecord],
    old_edits: &[Edit],
    new_raw: &[RawRecord],
    new_speakers: &[Option<usize>],
    at: &str,
) -> Vec<Edit> {
    let current = current_speakers(old_edits);
    let mut named: HashMap<(Track, String, String), Vec<(u64, u64)>> = HashMap::new();
    for r in old_raw {
        if let Some((name, by)) = current.get(&(r.track, r.start_ms)) {
            if by != DIARIZE_BY {
                named
                    .entry((r.track, name.clone(), by.clone()))
                    .or_default()
                    .push((r.start_ms, r.end_ms));
            }
        }
    }
    let mut candidates: Vec<(u64, Track, usize, String, String)> = Vec::new();
    for ((track, name, by), spans) in &named {
        let mut overlap: HashMap<usize, u64> = HashMap::new();
        for (r, s) in new_raw.iter().zip(new_speakers) {
            let Some(s) = s else { continue };
            if r.track != *track {
                continue;
            }
            for &(a, b) in spans {
                let o = r.end_ms.min(b).saturating_sub(r.start_ms.max(a));
                *overlap.entry(*s).or_default() += o;
            }
        }
        for (s, o) in overlap {
            if o > 0 {
                candidates.push((o, *track, s, name.clone(), by.clone()));
            }
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then(a.3.cmp(&b.3)).then(a.2.cmp(&b.2)));
    let mut taken_speaker: HashSet<(Track, usize)> = HashSet::new();
    let mut taken_name: HashSet<(Track, String)> = HashSet::new();
    let mut out = Vec::new();
    for (_, track, s, name, by) in candidates {
        if taken_speaker.contains(&(track, s)) || taken_name.contains(&(track, name.clone())) {
            continue;
        }
        taken_speaker.insert((track, s));
        taken_name.insert((track, name.clone()));
        for (r, spk) in new_raw.iter().zip(new_speakers) {
            if r.track == track && *spk == Some(s) {
                out.push(Edit::Speaker {
                    target: Target {
                        track,
                        start_ms: r.start_ms,
                    },
                    name: name.clone(),
                    by: by.clone(),
                    at: at.to_string(),
                });
            }
        }
    }
    out
}

fn set_status(dir: &Path, status: &str, meter: Option<&Arc<Meter>>, phase: MeterPhase) {
    std::fs::write(dir.join(STATUS_FILE), format!("{status}\n")).ok();
    if let Some(m) = meter {
        m.set_phase(phase);
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("{} has no file name", path.display()))?;
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn jsonl<T: serde::Serialize>(items: &[T]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for i in items {
        out.extend(serde_json::to_vec(i)?);
        out.push(b'\n');
    }
    Ok(out)
}

fn is_nonempty(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() > 0)
}

/// Copies, never moves: the caller removes originals once the copy is safe.
fn archive(dir: &Path) -> Result<Option<String>> {
    let files: Vec<&str> = ["raw.jsonl", "edits.jsonl", "transcript.md"]
        .into_iter()
        .filter(|f| dir.join(f).is_file())
        .collect();
    if files.is_empty() {
        return Ok(None);
    }
    let stamp = crate::clock::now_rfc3339().replace(':', "-");
    let mut name = stamp.clone();
    let mut n = 1;
    while dir.join(PREVIOUS_DIR).join(&name).exists() {
        n += 1;
        name = format!("{stamp}-{n}");
    }
    let to = dir.join(PREVIOUS_DIR).join(&name);
    std::fs::create_dir_all(&to)?;
    for f in files {
        std::fs::copy(dir.join(f), to.join(f))
            .with_context(|| format!("archiving {}", dir.join(f).display()))?;
    }
    Ok(Some(name))
}

fn has_typed_edits(dir: &Path) -> Result<bool> {
    let edits: Vec<Edit> = read_jsonl(&dir.join("edits.jsonl"))?;
    Ok(edits.iter().any(|e| match e {
        Edit::Speaker { by, .. } | Edit::Repair { by, .. } | Edit::Revert { by, .. } => {
            by != DIARIZE_BY
        }
    }))
}

/// Skips archiving when the `carry-from` archive already holds exactly these
/// files, so nothing a person wrote is overwritten unarchived. `carry-from`
/// moves to the new archive only when it holds typed edits.
fn archive_if_changed(dir: &Path) -> Result<()> {
    if !is_nonempty(&dir.join("raw.jsonl")) && !is_nonempty(&dir.join("edits.jsonl")) {
        return Ok(());
    }
    let carry_file = dir.join(CARRY_FILE);
    if let Ok(name) = std::fs::read_to_string(&carry_file) {
        let from = dir.join(PREVIOUS_DIR).join(name.trim());
        let same = |f: &str| {
            std::fs::read(dir.join(f)).unwrap_or_default()
                == std::fs::read(from.join(f)).unwrap_or_default()
        };
        if same("raw.jsonl") && same("edits.jsonl") {
            return Ok(());
        }
    }
    let typed = has_typed_edits(dir)?;
    if let Some(name) = archive(dir)? {
        if typed {
            write_atomically(&carry_file, name.as_bytes())?;
        }
    }
    Ok(())
}

/// The caller holds the transcription lock. A failed diarisation stage fails
/// the job, except on the attempt that reaches the retry cap, which writes the
/// words unlabelled with a warning.
pub(crate) fn run(
    dir: &Path,
    engine: &Engine,
    diarize: bool,
    meter: Option<&Arc<Meter>>,
) -> Result<PathBuf> {
    let audio = dir.join("audio");
    let tracks: Vec<(Track, PathBuf)> = [
        (Track::Room, audio.join("room.wav")),
        (Track::Call, audio.join("call.wav")),
    ]
    .into_iter()
    .filter(|(_, p)| p.is_file())
    .collect();
    if tracks.is_empty() {
        bail!(
            "{} has no audio to transcribe — the retention setting may have removed it",
            dir.display()
        );
    }

    set_status(dir, "transcribing", meter, MeterPhase::Transcribing);
    let mut words: Vec<(Track, PathBuf, Vec<Word>)> = Vec::new();
    for (track, wav) in &tracks {
        let t: TranscriptFile = stage(engine, dir, *track, wav, Stage::Transcribe, ASR_MODEL)?;
        eprintln!("  {} — {} word(s)", track.as_str(), t.words.len());
        words.push((*track, wav.clone(), t.words));
    }

    let mut turns: HashMap<Track, Vec<Turn>> = HashMap::new();
    let mut warning = None;
    if diarize && words.iter().any(|(_, _, w)| !w.is_empty()) {
        set_status(dir, "separating voices", meter, MeterPhase::Diarizing);
        for (track, wav, w) in &words {
            if w.is_empty() {
                continue;
            }
            match stage::<SpeakersFile>(engine, dir, *track, wav, Stage::Diarize, DIAR_MODEL) {
                Ok(s) => {
                    turns.insert(*track, s.turns);
                }
                // At the cap a diariser fault must not withhold the words.
                // Below it, fail; the retry resumes from the transcribe
                // checkpoint.
                Err(e) if attempts(dir) + 1 >= RETRY_CAP => {
                    warning = Some(format!(
                        "{UNLABELLED_WARNING} ({e:#}); the transcript is unlabelled — \
                         transcribe it again to try once more"
                    ));
                    turns.clear();
                    break;
                }
                Err(e) => return Err(e),
            }
        }
    }

    let mut raw = Vec::new();
    let mut speakers = Vec::new();
    for (track, _, w) in &words {
        let (r, s) = assemble(*track, w, turns.get(track).map(Vec::as_slice));
        raw.extend(r);
        speakers.extend(s);
    }

    archive_if_changed(dir)?;
    let carry_file = dir.join(CARRY_FILE);

    let now = crate::clock::now_rfc3339();
    let mut edits: Vec<Edit> = raw
        .iter()
        .zip(&speakers)
        .filter_map(|(r, s)| {
            s.map(|s| Edit::Speaker {
                target: Target {
                    track: r.track,
                    start_ms: r.start_ms,
                },
                name: label(r.track, s),
                by: DIARIZE_BY.into(),
                at: now.clone(),
            })
        })
        .collect();
    if let Ok(from) = std::fs::read_to_string(&carry_file) {
        let from = dir.join(PREVIOUS_DIR).join(from.trim());
        let old_raw: Vec<RawRecord> = read_jsonl(&from.join("raw.jsonl"))?;
        let old_edits: Vec<Edit> = read_jsonl(&from.join("edits.jsonl"))?;
        edits.extend(carry_names(&old_raw, &old_edits, &raw, &speakers, &now));
    }

    write_atomically(&dir.join("raw.jsonl"), &jsonl(&raw)?)?;
    write_atomically(&dir.join("edits.jsonl"), &jsonl(&edits)?)?;
    let labelled = speakers.iter().any(Option::is_some);
    match warning {
        Some(w) => crate::session::add_warning(dir, &w)?,
        None if labelled => crate::session::clear_warnings(dir, UNLABELLED_WARNING)?,
        None => {}
    }
    let md = dir.join("transcript.md");
    write_atomically(&md, crate::session::markdown(dir)?.as_bytes())?;

    // A run that labelled nobody keeps `carry-from`, so the next labelled run
    // can still carry the names.
    if labelled {
        std::fs::remove_file(&carry_file).ok();
    }
    std::fs::remove_file(dir.join(ATTEMPTS_FILE)).ok();
    remove_live_leftovers(dir);
    set_status(dir, "done", meter, MeterPhase::Done);
    Ok(md)
}

/// Files an older build's live transcriber left; remove only once the
/// transcript is written.
fn remove_live_leftovers(dir: &Path) {
    let audio = dir.join("audio");
    for p in [
        dir.join("live-asr.json"),
        audio.join("room.live.pcm"),
        audio.join("call.live.pcm"),
        audio.join("room.source.wav"),
        audio.join("call.source.wav"),
    ] {
        std::fs::remove_file(p).ok();
    }
}

pub fn attempts(dir: &Path) -> u32 {
    std::fs::read_to_string(dir.join(ATTEMPTS_FILE))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// A job abandoned because the app quit is never seen, so it never counts.
pub fn note_failure(dir: &Path) {
    let n = attempts(dir) + 1;
    std::fs::write(dir.join(ATTEMPTS_FILE), format!("{n}\n")).ok();
}

pub fn reset_attempts(dir: &Path) {
    std::fs::remove_file(dir.join(ATTEMPTS_FILE)).ok();
}

/// The caller holds the transcription lock. Archive first, then remove
/// `transcript.md`; a crash between leaves the session either still finished
/// or awaiting a transcript with every typed edit archived.
pub(crate) fn prepare_rerun(dir: &Path) -> Result<()> {
    let audio = dir.join("audio");
    if !audio.join("room.wav").is_file() && !audio.join("call.wav").is_file() {
        bail!(
            "{} has no audio left to transcribe again — the retention setting removed it",
            dir.display()
        );
    }
    archive_if_changed(dir)?;
    match std::fs::remove_file(dir.join("transcript.md")) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    reset_attempts(dir);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, start: f64, end: f64) -> Word {
        Word {
            text: text.into(),
            start,
            end,
            confidence: 0.9,
        }
    }
    fn t(speaker: usize, start: f64, end: f64) -> Turn {
        Turn {
            speaker,
            start,
            end,
        }
    }

    #[test]
    fn model_ids_match_the_helper() {
        let swift = include_str!("../engine/Sources/AmbientEngine/main.swift");
        assert!(swift.contains(&format!("let asrModel = \"{ASR_MODEL}\"")));
        assert!(swift.contains(&format!("let diarModel = \"{DIAR_MODEL}\"")));
        assert!(swift.contains(&format!("let asrFolder = \"{ASR_DIR}\"")));
        let (folder, sub) = DIAR_DIR.split_once('/').unwrap();
        assert!(swift.contains(&format!("let diarFolder = \"{folder}\"")));
        assert!(swift.contains(&format!("appendingPathComponent(\"{sub}\")")));
    }

    #[test]
    fn lines_break_at_pauses_without_turns() {
        let words = [
            w("Hello", 0.0, 0.4),
            w("there.", 0.5, 0.9),
            w("Next", 2.5, 2.8),
            w("bit", 2.9, 3.1),
        ];
        let (r, s) = assemble(Track::Call, &words, None);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].text, "Hello there.");
        assert_eq!((r[1].start_ms, r[1].end_ms), (2500, 3100));
        assert_eq!(s, vec![None, None]);
    }

    #[test]
    fn lines_break_at_speaker_changes_and_number_by_first_appearance() {
        let words = [
            w("Hi", 0.0, 0.3),
            w("Bob", 0.3, 0.6),
            w("Hey", 0.7, 1.0),
            w("Alice", 1.0, 1.4),
        ];
        let turns = [t(3, 0.0, 0.65), t(1, 0.65, 20.0), t(3, 20.0, 30.0)];
        let (r, s) = assemble(Track::Room, &words, Some(&turns));
        assert_eq!(
            r.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            ["Hi Bob", "Hey Alice"]
        );
        assert_eq!(s, vec![Some(0), Some(1)]);
    }

    #[test]
    fn start_times_stay_unique_when_words_share_one() {
        let words = [w("a", 1.0, 1.0), w("b", 1.0, 1.2)];
        let turns = [t(0, 0.0, 1.0), t(1, 1.0, 9.0)];
        let (r, _) = assemble(Track::Room, &words, Some(&turns));
        let starts: Vec<u64> = r.iter().map(|r| r.start_ms).collect();
        let mut dedup = starts.clone();
        dedup.dedup();
        assert_eq!(starts, dedup, "{starts:?}");
    }

    #[test]
    fn a_phantom_speaker_is_folded_into_the_nearest_real_one() {
        let words = [w("one", 0.0, 0.5), w("two", 6.0, 6.5), w("three", 6.6, 7.0)];
        let turns = [t(0, 0.0, 6.0), t(2, 6.0, 6.5), t(0, 6.5, 12.5)];
        let (_, s) = assemble(Track::Room, &words, Some(&turns));
        assert!(s.iter().all(|s| *s == Some(0)), "{s:?}");
    }

    #[test]
    fn long_monologues_end_at_a_sentence_after_twenty_seconds() {
        let mut words = Vec::new();
        for i in 0..60 {
            let start = i as f64 * 0.5;
            let text = if i == 45 { "end." } else { "word" };
            words.push(w(text, start, start + 0.4));
        }
        let (r, _) = assemble(Track::Room, &words, None);
        assert_eq!(r.len(), 2);
        assert!(r[0].text.ends_with("end."));
    }

    #[test]
    fn typed_names_carry_to_the_speaker_who_overlaps_most() {
        let rec = |start_ms, end_ms| RawRecord {
            track: Track::Call,
            start_ms,
            end_ms,
            text: "x".into(),
            confidence: 1.0,
        };
        let old_raw = vec![rec(0, 5000), rec(6000, 9000)];
        let speaker = |start_ms, name: &str, by: &str| Edit::Speaker {
            target: Target {
                track: Track::Call,
                start_ms,
            },
            name: name.into(),
            by: by.into(),
            at: "t".into(),
        };
        let old_edits = vec![
            speaker(0, "call-1", DIARIZE_BY),
            speaker(0, "Priya", "user"),
            speaker(6000, "call-2", DIARIZE_BY),
        ];
        let new_raw = vec![rec(0, 2000), rec(2100, 4800), rec(6000, 9000)];
        let new_speakers = vec![Some(1), Some(1), Some(0)];
        let carried = carry_names(&old_raw, &old_edits, &new_raw, &new_speakers, "now");
        let named: Vec<(u64, &str)> = carried
            .iter()
            .map(|e| match e {
                Edit::Speaker { target, name, .. } => (target.start_ms, name.as_str()),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(named, vec![(0, "Priya"), (2100, "Priya")]);
    }
}
