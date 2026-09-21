//! Incremental transcription over disk-backed capture spools.
//!
//! The capture thread only appends native-rate mono PCM. Model loading, VAD,
//! resampling and decoding happen on a background thread. A checkpoint and a
//! pending append journal make `raw.jsonl` a durable, append-only cursor even
//! when the process stops between writing a batch and advancing its offsets.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::resample::TARGET_HZ;
use crate::session::{RawRecord, Track};

pub const CADENCE_SECONDS: u64 = 10;
pub const MAX_CARRY_SECONDS: u64 = 30;
const STATE_FILE: &str = "live-asr.json";
const ROOM_SPOOL: &str = "room.live.pcm";
const CALL_SPOOL: &str = "call.live.pcm";

/// Serialize model loading, VAD, recognition and diarization in this process.
/// Live acquisition can be cancelled while another session owns this lane.
static DECODER: Mutex<()> = Mutex::new(());

pub fn decoder() -> MutexGuard<'static, ()> {
    DECODER.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
struct TrackState {
    /// Native samples consumed at cadence boundaries.
    read: u64,
    /// Start of speech held across the preceding boundary.
    carry: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    lines: Vec<String>,
    tracks: [TrackState; 2],
    complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct State {
    version: u8,
    rates: [u32; 2],
    tracks: [TrackState; 2],
    raw_len: u64,
    complete: bool,
    pending: Option<Pending>,
}

impl State {
    fn new(rates: [u32; 2], raw_len: u64) -> Self {
        Self {
            version: 1,
            rates,
            tracks: [TrackState::default(); 2],
            raw_len,
            complete: false,
            pending: None,
        }
    }
}

pub struct LiveTranscriber {
    room: Option<File>,
    call: Option<File>,
    stop: Sender<()>,
    join: Option<JoinHandle<Result<()>>>,
}

impl LiveTranscriber {
    /// Claim the session and start its model worker. Failure is returned before
    /// capture depends on this object; callers deliberately treat it as a
    /// transcription failure, not a recording failure.
    pub fn start(dir: &Path, rates: [u32; 2], asr_dir: PathBuf, vad: PathBuf) -> Result<Self> {
        if rates.contains(&0) {
            bail!("live transcription sample rates must be non-zero");
        }
        let _lock = crate::session::claim_transcription(dir)?;
        let audio = dir.join("audio");
        let room_path = audio.join(ROOM_SPOOL);
        let call_path = audio.join(CALL_SPOOL);
        let room = append_file(&room_path)?;
        let call = append_file(&call_path)?;
        let raw = dir.join("raw.jsonl");
        let raw_len = append_file(&raw)?.metadata()?.len();
        let state_path = dir.join(STATE_FILE);
        if state_path.exists() {
            let state = read_state(&state_path)?;
            if state.rates != rates {
                bail!("live transcription sample rates changed across recovery");
            }
        } else {
            write_state(&state_path, &State::new(rates, raw_len))?;
        }
        let (stop, rx) = channel();
        let owned = dir.to_path_buf();
        let join = std::thread::spawn(move || {
            let _lock = _lock;
            background_qos();
            run(&owned, &asr_dir, &vad, &rx, false)
        });
        Ok(Self {
            room: Some(room),
            call: Some(call),
            stop,
            join: Some(join),
        })
    }

    /// Append one drain to the disk queue. No model work or inter-thread wait
    /// occurs on the capture thread.
    pub fn append(&mut self, track: Track, samples: &[f32]) -> Result<()> {
        let file = match track {
            Track::Room => self.room.as_mut(),
            Track::Call => self.call.as_mut(),
        }
        .ok_or_else(|| anyhow!("live transcription is already stopping"))?;
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            let pcm = ((*sample).clamp(-1.0, 1.0) * 32767.0) as i16;
            bytes.extend_from_slice(&pcm.to_le_bytes());
        }
        file.write_all(&bytes)?;
        file.flush()?;
        Ok(())
    }

    /// Close the writers and pause after the current inference call. The
    /// serial queue resumes the durable backlog and final tail.
    pub fn finish(mut self) -> Result<()> {
        self.room.take();
        self.call.take();
        let _ = self.stop.send(());
        self.join
            .take()
            .expect("live worker exists")
            .join()
            .map_err(|_| anyhow!("live transcription worker panicked"))?
    }
}

impl Drop for LiveTranscriber {
    fn drop(&mut self) {
        self.room.take();
        self.call.take();
        let _ = self.stop.send(());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn append_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

fn background_qos() {
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
    }
}

fn run(
    dir: &Path,
    asr_dir: &Path,
    vad_path: &Path,
    stop: &Receiver<()>,
    drain_on_stop: bool,
) -> Result<()> {
    let state_path = dir.join(STATE_FILE);
    let mut state = read_state(&state_path)?;
    recover_pending(dir, &state_path, &mut state)?;

    let Some(model_guard) = decoder_for(stop, !drain_on_stop) else {
        return Ok(());
    };
    let mut vad = crate::vad::Vad::load(path_utf8(vad_path)?)?;
    let mut rec = crate::asr::Recognizer::load(path_utf8(asr_dir)?)?;
    drop(model_guard);
    if !drain_on_stop && stop_requested(stop) {
        return Ok(());
    }
    let mut stopping = false;
    loop {
        match stop.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) if !drain_on_stop => return Ok(()),
            Ok(()) | Err(TryRecvError::Disconnected) => stopping = true,
            Err(TryRecvError::Empty) => {}
        }
        let sizes = spool_samples(dir)?;
        let mut progressed = false;
        for index in 0..2 {
            if let Some(block) = next_block(
                state.tracks[index],
                state.rates[index],
                sizes[index],
                stopping,
            ) {
                let BlockResult::Done(lines, next) = transcribe_block(
                    dir,
                    index,
                    state.rates[index],
                    block,
                    &mut vad,
                    &mut rec,
                    stop,
                    !drain_on_stop,
                )?
                else {
                    return Ok(());
                };
                let mut tracks = state.tracks;
                tracks[index] = next;
                commit(dir, &state_path, &mut state, lines, tracks, false)?;
                progressed = true;
            }
        }
        if stopping && !progressed {
            let done = state
                .tracks
                .iter()
                .enumerate()
                .all(|(i, t)| t.read >= sizes[i] && t.carry.is_none());
            if done {
                let tracks = state.tracks;
                commit(dir, &state_path, &mut state, Vec::new(), tracks, true)?;
                return Ok(());
            }
        }
        if !progressed {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Block {
    start: u64,
    end: u64,
    final_tail: bool,
    force_boundary: bool,
}

fn next_block(track: TrackState, rate: u32, available: u64, stopping: bool) -> Option<Block> {
    let cadence = u64::from(rate) * CADENCE_SECONDS;
    if !stopping && available.saturating_sub(track.read) < cadence {
        return None;
    }
    if track.read >= available && track.carry.is_none() {
        return None;
    }
    let start = track.carry.unwrap_or(track.read);
    let end = (track.read + cadence)
        .min(start + u64::from(rate) * MAX_CARRY_SECONDS)
        .min(available);
    if start >= end {
        return None;
    }
    let final_tail = stopping && end == available;
    let force_boundary = end.saturating_sub(start) >= u64::from(rate) * MAX_CARRY_SECONDS;
    Some(Block {
        start,
        end,
        final_tail,
        force_boundary,
    })
}

enum BlockResult {
    Done(Vec<String>, TrackState),
    Cancelled,
}

#[allow(clippy::too_many_arguments)]
fn transcribe_block(
    dir: &Path,
    index: usize,
    rate: u32,
    block: Block,
    vad: &mut crate::vad::Vad,
    rec: &mut crate::asr::Recognizer,
    stop: &Receiver<()>,
    cancellable: bool,
) -> Result<BlockResult> {
    let path = dir
        .join("audio")
        .join(if index == 0 { ROOM_SPOOL } else { CALL_SPOOL });
    let native = read_pcm(&path, block.start, block.end)?;
    let samples = crate::resample::to_16k(&native, rate)?;
    let Some(vad_guard) = decoder_for(stop, cancellable) else {
        return Ok(BlockResult::Cancelled);
    };
    let mut turns = vad.turns(&samples, MAX_CARRY_SECONDS as usize)?;
    drop(vad_guard);
    if cancellable && stop_requested(stop) {
        return Ok(BlockResult::Cancelled);
    }
    let hold = !block.final_tail
        && !block.force_boundary
        && turns.last().is_some_and(|s| {
            samples.len().saturating_sub(s.end) <= crate::vad::PAD_MS * TARGET_HZ as usize / 1000
        });
    let held = hold.then(|| turns.pop().expect("last turn exists"));
    let mut lines = Vec::new();
    for turn in turns {
        let Some(_serial) = decoder_for(stop, cancellable) else {
            return Ok(BlockResult::Cancelled);
        };
        let decoded = { rec.transcribe_segments(&samples, &[turn])? };
        drop(_serial);
        if cancellable && stop_requested(stop) {
            // Nothing in this block has been journalled yet. Re-decoding the
            // whole block after Stop is safer than publishing half a block.
            return Ok(BlockResult::Cancelled);
        }
        for (seg, text, confidence) in decoded {
            let start_native = block.start + scale(seg.start as u64, TARGET_HZ, rate);
            let end_native = block.start + scale(seg.end as u64, TARGET_HZ, rate);
            let record = RawRecord {
                track: if index == 0 { Track::Room } else { Track::Call },
                start_ms: start_native * 1000 / u64::from(rate),
                end_ms: end_native * 1000 / u64::from(rate),
                text,
                confidence,
            };
            lines.push(serde_json::to_string(&record)?);
        }
    }
    let carry = held.map(|s| block.start + scale(s.start as u64, TARGET_HZ, rate));
    Ok(BlockResult::Done(
        lines,
        TrackState {
            read: block.end,
            carry,
        },
    ))
}

fn decoder_for(stop: &Receiver<()>, cancellable: bool) -> Option<MutexGuard<'static, ()>> {
    if !cancellable {
        return Some(decoder());
    }
    loop {
        match DECODER.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(error)) => return Some(error.into_inner()),
            Err(TryLockError::WouldBlock) => {
                if stop_requested(stop) {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn stop_requested(stop: &Receiver<()>) -> bool {
    matches!(stop.try_recv(), Ok(()) | Err(TryRecvError::Disconnected))
}

fn scale(value: u64, from: u32, to: u32) -> u64 {
    value.saturating_mul(u64::from(to)) / u64::from(from)
}

fn read_pcm(path: &Path, start: u64, end: u64) -> Result<Vec<f32>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start * 2))?;
    let mut bytes = vec![0; end.saturating_sub(start) as usize * 2];
    file.read_exact(&mut bytes)?;
    Ok(bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect())
}

fn spool_samples(dir: &Path) -> Result<[u64; 2]> {
    let audio = dir.join("audio");
    Ok([
        std::fs::metadata(audio.join(ROOM_SPOOL))?.len() / 2,
        std::fs::metadata(audio.join(CALL_SPOOL))?.len() / 2,
    ])
}

fn commit(
    dir: &Path,
    state_path: &Path,
    state: &mut State,
    lines: Vec<String>,
    tracks: [TrackState; 2],
    complete: bool,
) -> Result<()> {
    state.pending = Some(Pending {
        lines,
        tracks,
        complete,
    });
    write_state(state_path, state)?;
    recover_pending(dir, state_path, state)
}

fn recover_pending(dir: &Path, state_path: &Path, state: &mut State) -> Result<()> {
    let Some(pending) = state.pending.clone() else {
        return Ok(());
    };
    let payload = if pending.lines.is_empty() {
        Vec::new()
    } else {
        (pending.lines.join("\n") + "\n").into_bytes()
    };
    let raw_path = dir.join("raw.jsonl");
    let mut raw = append_file(&raw_path)?;
    let len = raw.metadata()?.len();
    if len < state.raw_len {
        bail!(
            "{} was truncated while live transcription was pending",
            raw_path.display()
        );
    }
    raw.seek(SeekFrom::Start(state.raw_len))?;
    let mut tail = Vec::new();
    raw.read_to_end(&mut tail)?;
    if !payload.starts_with(&tail) {
        bail!(
            "{} changed after the durable live-transcription prefix",
            raw_path.display()
        );
    }
    raw.write_all(&payload[tail.len()..])?;
    raw.flush()?;
    raw.sync_data()?;
    state.raw_len += payload.len() as u64;
    state.tracks = pending.tracks;
    state.complete = pending.complete;
    state.pending = None;
    write_state(state_path, state)
}

fn read_state(path: &Path) -> Result<State> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let state: State =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if state.version != 1 {
        bail!(
            "unsupported live transcription state version {}",
            state.version
        );
    }
    if state.rates.contains(&0) {
        bail!("live transcription state has a zero sample rate");
    }
    Ok(state)
}

fn write_state(path: &Path, state: &State) -> Result<()> {
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let mut file = File::create(&tmp)?;
    file.write_all(&serde_json::to_vec(state)?)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn path_utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("model path is not valid UTF-8"))
}

/// Whether this session used the live pipeline. A complete checkpoint means
/// finalization may skip whole-track ASR without touching the raw prefix.
pub fn complete(dir: &Path) -> Result<bool> {
    let path = dir.join(STATE_FILE);
    if !path.exists() {
        return Ok(false);
    }
    Ok(read_state(&path)?.complete)
}

/// Resume an interrupted live worker after capture has closed its spools.
/// The caller holds the session transcription lock.
pub fn finish_existing(dir: &Path, asr_dir: &Path, vad: &Path) -> Result<()> {
    let path = dir.join(STATE_FILE);
    if !path.exists() || read_state(&path)?.complete {
        return Ok(());
    }
    let (tx, rx) = channel();
    drop(tx);
    run(dir, asr_dir, vad, &rx, true)
}

/// Fill a missing spool tail from finalized native audio. The prefix is left
/// untouched because its offsets are already named by the durable checkpoint.
pub fn repair_spool(dir: &Path, track: Track, samples: &[f32]) -> Result<()> {
    if !dir.join(STATE_FILE).exists() {
        return Ok(());
    }
    let path = dir.join("audio").join(match track {
        Track::Room => ROOM_SPOOL,
        Track::Call => CALL_SPOOL,
    });
    let mut file = append_file(&path)?;
    let bytes_on_disk = file.metadata()?.len();
    if bytes_on_disk % 2 != 0 {
        // A failed two-byte sample write is not a published transcript byte;
        // discard only its orphan byte before rebuilding the spool tail.
        file.set_len(bytes_on_disk - 1)?;
    }
    let written = (file.metadata()?.len() / 2) as usize;
    if written > samples.len() {
        bail!("{} is longer than finalized capture audio", path.display());
    }
    let mut bytes = Vec::with_capacity((samples.len() - written) * 2);
    for sample in &samples[written..] {
        // `read_wav_any` divided the authoritative i16 by 32768; invert that
        // exactly instead of applying capture's original float quantizer a
        // second time and losing an LSB.
        let pcm = ((*sample * 32768.0).clamp(i16::MIN as f32, i16::MAX as f32)) as i16;
        bytes.extend_from_slice(&pcm.to_le_bytes());
    }
    file.write_all(&bytes)?;
    file.flush()?;
    Ok(())
}

/// Retry spool repair from native WAVs retained after a live write failure.
pub fn repair_from_native(dir: &Path) -> Result<()> {
    let audio = dir.join("audio");
    for (track, names) in [
        (Track::Room, ["room.source.wav", "room.native.wav"]),
        (Track::Call, ["call.source.wav", "call.native.wav"]),
    ] {
        for name in names {
            let native = audio.join(name);
            if native.exists() {
                let (samples, _) = crate::resample::read_wav_any(&native)?;
                repair_spool(dir, track, &samples)?;
                break;
            }
        }
    }
    Ok(())
}

/// Remove transient disk queues only after markdown/diarization is durable.
pub fn cleanup(dir: &Path) {
    let audio = dir.join("audio");
    for path in [
        audio.join(ROOM_SPOOL),
        audio.join(CALL_SPOOL),
        audio.join("room.source.wav"),
        audio.join("call.source.wav"),
        audio.join("room.native.wav"),
        audio.join("call.native.wav"),
    ] {
        std::fs::remove_file(path).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_speech_is_carried_but_forced_out_at_thirty_seconds() {
        let rate = 48_000;
        let b1 = next_block(TrackState::default(), rate, 10 * rate as u64, false).unwrap();
        assert_eq!((b1.start, b1.end), (0, 480_000));
        let held = TrackState {
            read: b1.end,
            carry: Some(9 * rate as u64),
        };
        let b2 = next_block(held, rate, 20 * rate as u64, false).unwrap();
        assert!(!b2.force_boundary);
        let held = TrackState {
            read: b2.end,
            carry: Some(0),
        };
        let b3 = next_block(held, rate, 30 * rate as u64, false).unwrap();
        assert!(b3.force_boundary, "carry is bounded at thirty seconds");
    }

    #[test]
    fn unequal_rates_keep_independent_offsets_and_flush_short_tails() {
        let room = next_block(TrackState::default(), 48_000, 480_000, false).unwrap();
        let call = next_block(TrackState::default(), 44_100, 441_000, false).unwrap();
        assert_eq!(room.end, 480_000);
        assert_eq!(call.end, 441_000);
        assert!(next_block(
            TrackState {
                read: room.end,
                carry: None
            },
            48_000,
            room.end + 1_000,
            false
        )
        .is_none());
        assert!(
            next_block(
                TrackState {
                    read: room.end,
                    carry: None
                },
                48_000,
                room.end + 1_000,
                true
            )
            .unwrap()
            .final_tail
        );
    }

    #[test]
    fn stop_on_an_exact_cadence_flushes_held_speech() {
        let state = TrackState {
            read: 480_000,
            carry: Some(432_000),
        };
        let block = next_block(state, 48_000, 480_000, true).unwrap();
        assert_eq!((block.start, block.end), (432_000, 480_000));
        assert!(block.final_tail);
    }

    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ambient-live-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        File::create(dir.join("raw.jsonl")).unwrap();
        dir
    }

    #[test]
    fn pending_append_recovers_without_truncating_or_reordering_prefix() {
        let dir = fixture("recover");
        std::fs::write(dir.join("raw.jsonl"), b"old\n").unwrap();
        let state_path = dir.join(STATE_FILE);
        let mut state = State::new([48_000, 44_100], 4);
        state.pending = Some(Pending {
            lines: vec!["one".into(), "two".into()],
            tracks: [
                TrackState {
                    read: 10,
                    carry: None,
                },
                TrackState::default(),
            ],
            complete: false,
        });
        write_state(&state_path, &state).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("raw.jsonl"))
            .unwrap()
            .write_all(b"one\n")
            .unwrap();
        recover_pending(&dir, &state_path, &mut state).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("raw.jsonl")).unwrap(),
            "old\none\ntwo\n"
        );
        assert_eq!(state.tracks[0].read, 10);
        assert!(state.pending.is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn spool_repair_drops_an_orphan_byte_and_rebuilds_exact_pcm() {
        let dir = fixture("repair-spool");
        write_state(&dir.join(STATE_FILE), &State::new([48_000, 48_000], 0)).unwrap();
        let path = dir.join("audio").join(ROOM_SPOOL);
        std::fs::write(&path, [0x00, 0x80, 0xff]).unwrap();
        let samples = [i16::MIN as f32 / 32768.0, i16::MAX as f32 / 32768.0];
        repair_spool(&dir, Track::Room, &samples).unwrap();
        assert_eq!(
            std::fs::read(path).unwrap(),
            [0x00, 0x80, 0xff, 0x7f],
            "authoritative native PCM replaces only the incomplete tail"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn overload_remains_disk_backed_until_the_worker_catches_up() {
        let track = TrackState {
            read: 10,
            carry: None,
        };
        let block = next_block(track, 1, 100, false).unwrap();
        assert_eq!((block.start, block.end), (10, 20));
        assert_eq!(100 - block.end, 80, "unread audio stays on disk");
    }

    #[test]
    fn stop_cancels_a_live_worker_waiting_for_the_decoder() {
        let held = decoder();
        let (tx, rx) = channel();
        tx.send(()).unwrap();
        assert!(decoder_for(&rx, true).is_none());
        drop(held);
    }

    #[test]
    fn recovery_rejects_a_zero_sample_rate() {
        let dir = fixture("bad-rate");
        let mut state = State::new([48_000, 48_000], 0);
        state.rates[1] = 0;
        write_state(&dir.join(STATE_FILE), &state).unwrap();
        assert!(read_state(&dir.join(STATE_FILE))
            .unwrap_err()
            .to_string()
            .contains("zero sample rate"));
        std::fs::remove_dir_all(dir).ok();
    }
}
