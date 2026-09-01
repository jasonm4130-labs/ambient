//! The serial transcription queue.
//!
//! Capture and transcription are separate work — [`crate::session::capture_into`]
//! on the thread that owns the tap, [`crate::session::transcribe_session`]
//! here — so a new recording can start the moment the last one's audio is on
//! disk, rather than after its transcript is. One transcription at a time, on
//! one thread that lives as long as the process: ASR and diarisation are
//! heavy, and running two of them beside a live tap risks dropped audio, which
//! costs more than the wait does.
//!
//! Nothing here touches AppKit, and the worker is injected, so the queue is
//! tested with a fake that never loads a model.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;

use crate::session::{Meter, MeterPhase};

/// A session whose audio is written and whose transcript is not yet.
pub struct Job {
    pub dir: PathBuf,
    /// Written by the worker as it moves through ASR and diarisation, read by
    /// whichever UI is drawing the queue — the same `Meter` a live capture
    /// shares with the menu, so the words are the same too.
    pub meter: Arc<Meter>,
}

impl Job {
    pub fn id(&self) -> String {
        self.dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// What the worker hands back: which session, and how it went.
pub type Outcome = (PathBuf, anyhow::Result<PathBuf>);

pub struct Queue {
    tx: Sender<Job>,
    done: Receiver<Outcome>,
    /// Oldest first. The front is the job the worker is on; everything
    /// behind it is waiting. Kept here rather than asked of the thread so
    /// the menu can say how many without a round trip.
    pending: VecDeque<Job>,
}

impl Queue {
    /// Start the worker thread. `work` is called once per job, in order, and
    /// its result comes back through [`Queue::poll`].
    pub fn spawn(
        work: impl Fn(&Path, &Arc<Meter>) -> anyhow::Result<PathBuf> + Send + 'static,
    ) -> Queue {
        let (tx, rx) = channel::<Job>();
        let (done_tx, done) = channel::<Outcome>();
        std::thread::spawn(move || {
            // Background QoS: during back-to-back meetings this thread runs
            // ASR beside a live capture, and the capture's drain loop must
            // win every contest for a core. `drainbench` measures what the
            // loop actually loses with this set.
            #[cfg(target_os = "macos")]
            unsafe {
                libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
            }
            while let Ok(job) = rx.recv() {
                let r = work(&job.dir, &job.meter);
                if done_tx.send((job.dir, r)).is_err() {
                    break;
                }
            }
        });
        Queue {
            tx,
            done,
            pending: VecDeque::new(),
        }
    }

    /// Queue a session for transcription. The meter starts at `Transcribing`
    /// rather than the default `Capturing`, so a job read before the worker
    /// has touched it is never drawn as a recording.
    pub fn push(&mut self, dir: PathBuf) {
        let meter = Arc::new(Meter::default());
        meter.set_phase(MeterPhase::Transcribing);
        let job = Job {
            dir: dir.clone(),
            meter: meter.clone(),
        };
        // A send can only fail once the worker thread is gone, and `poll`
        // reports that for every job still pending; nothing to do here.
        self.tx.send(Job { dir, meter }).ok();
        self.pending.push_back(job);
    }

    /// Whether a result has arrived. Serial and first-in-first-out, so a
    /// result always belongs to the front of `pending`.
    pub fn poll(&mut self) -> Option<Outcome> {
        match self.done.try_recv() {
            Ok(o) => {
                self.pending.pop_front();
                Some(o)
            }
            Err(TryRecvError::Empty) => None,
            // The thread is gone. Every pending job is now a failure, surfaced
            // one per poll so each is reported against its own session.
            Err(TryRecvError::Disconnected) => self.pending.pop_front().map(|j| {
                (
                    j.dir,
                    Err(anyhow::anyhow!("the transcription thread went away")),
                )
            }),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// One line for the menu: what the worker is doing to which session, and
    /// how many are behind it. `None` when there is nothing to say.
    pub fn summary(&self) -> Option<String> {
        let front = self.pending.front()?;
        let verb = match front.meter.phase() {
            MeterPhase::Diarizing => "Separating voices",
            MeterPhase::Done => "Finishing",
            MeterPhase::Failed => "Failed",
            MeterPhase::Capturing | MeterPhase::Transcribing => "Transcribing",
        };
        let waiting = self.pending.len() - 1;
        Some(match waiting {
            0 => format!("{verb} {}", front.id()),
            1 => format!("{verb} {} · 1 waiting", front.id()),
            n => format!("{verb} {} · {n} waiting", front.id()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Sender as StdSender;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// A worker that finishes a job only when told to, so a test can hold the
    /// queue at a known depth.
    fn gated() -> (Queue, StdSender<anyhow::Result<()>>) {
        let (gate_tx, gate_rx) = channel::<anyhow::Result<()>>();
        let gate_rx = Mutex::new(gate_rx);
        let q = Queue::spawn(move |dir, meter| {
            meter.set_phase(MeterPhase::Diarizing);
            gate_rx.lock().unwrap().recv().unwrap_or(Ok(()))?;
            Ok(dir.to_path_buf())
        });
        (q, gate_tx)
    }

    fn wait(q: &mut Queue) -> Outcome {
        let t = Instant::now();
        loop {
            if let Some(o) = q.poll() {
                return o;
            }
            assert!(t.elapsed() < Duration::from_secs(5), "no result arrived");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The property the issue is about: pushing a second session while the
    /// first is being transcribed queues it, and the two finish in order.
    #[test]
    fn jobs_run_one_at_a_time_in_the_order_they_arrived() {
        let (mut q, gate) = gated();
        assert!(q.is_empty());
        assert_eq!(q.summary(), None);

        q.push(PathBuf::from("/s/2026-09-02T0900"));
        q.push(PathBuf::from("/s/2026-09-02T1000"));
        assert_eq!(q.len(), 2);
        assert!(q.poll().is_none(), "nothing has finished yet");

        gate.send(Ok(())).unwrap();
        let (dir, r) = wait(&mut q);
        assert_eq!(dir, PathBuf::from("/s/2026-09-02T0900"));
        assert!(r.is_ok());
        assert_eq!(q.len(), 1);

        gate.send(Ok(())).unwrap();
        let (dir, _) = wait(&mut q);
        assert_eq!(dir, PathBuf::from("/s/2026-09-02T1000"));
        assert!(q.is_empty());
    }

    /// The menu line names the session in flight and counts the rest.
    #[test]
    fn the_summary_names_the_front_and_counts_the_waiting() {
        let (mut q, gate) = gated();
        q.push(PathBuf::from("/s/a"));
        // Pushed and not yet touched by the worker: never "Recording".
        assert!(q.summary().unwrap().starts_with("Transcribing a"));
        q.push(PathBuf::from("/s/b"));
        q.push(PathBuf::from("/s/c"));
        // The fake moves the front meter to Diarizing as soon as it picks the
        // job up; give it a moment to.
        let t = Instant::now();
        while !q.summary().unwrap().starts_with("Separating voices") {
            assert!(t.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            q.summary().as_deref(),
            Some("Separating voices a · 2 waiting")
        );
        gate.send(Ok(())).unwrap();
        let _ = wait(&mut q);
        assert!(q.summary().unwrap().ends_with("b · 1 waiting"));
        gate.send(Ok(())).unwrap();
        let _ = wait(&mut q);
        assert!(q.summary().unwrap().ends_with(" c"));
    }

    /// A failed transcription is reported against its own session and does
    /// not stop the one behind it.
    #[test]
    fn a_failure_is_reported_and_the_queue_moves_on() {
        let (mut q, gate) = gated();
        q.push(PathBuf::from("/s/bad"));
        q.push(PathBuf::from("/s/good"));
        gate.send(Err(anyhow::anyhow!("the models did not load")))
            .unwrap();
        let (dir, r) = wait(&mut q);
        assert_eq!(dir, PathBuf::from("/s/bad"));
        assert!(r.unwrap_err().to_string().contains("models"));
        gate.send(Ok(())).unwrap();
        let (dir, r) = wait(&mut q);
        assert_eq!(dir, PathBuf::from("/s/good"));
        assert!(r.is_ok());
        assert!(q.is_empty());
    }

    /// A worker thread that dies must not leave jobs pending for ever, or a
    /// deferred quit would wait on them for ever.
    #[test]
    fn a_dead_worker_fails_every_pending_job_rather_than_hanging() {
        // Panicking is the one way a worker can die without reporting; the
        // panic message on stderr is expected.
        let mut q = Queue::spawn(|_, _| panic!("the worker died"));
        q.push(PathBuf::from("/s/a"));
        q.push(PathBuf::from("/s/b"));
        let (dir, r) = wait(&mut q);
        assert_eq!(dir, PathBuf::from("/s/a"));
        assert!(r.is_err());
        let (dir, r) = wait(&mut q);
        assert_eq!(dir, PathBuf::from("/s/b"));
        assert!(r.unwrap_err().to_string().contains("went away"));
        assert!(q.is_empty());
    }
}
