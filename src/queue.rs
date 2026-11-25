//! Serial transcription queue: one job at a time on one long-lived thread,
//! because two heavy jobs beside a live tap risk dropped audio. The worker is
//! injected so tests run a fake.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;

use crate::session::{Meter, MeterPhase};

pub struct Job {
    pub dir: PathBuf,
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

pub type Outcome = (PathBuf, anyhow::Result<PathBuf>);

pub struct Queue {
    tx: Sender<Job>,
    done: Receiver<Outcome>,
    /// Oldest first; the front is the job in progress.
    pending: VecDeque<Job>,
}

impl Queue {
    pub fn spawn(
        work: impl Fn(&Path, &Arc<Meter>) -> anyhow::Result<PathBuf> + Send + 'static,
    ) -> Queue {
        let (tx, rx) = channel::<Job>();
        let (done_tx, done) = channel::<Outcome>();
        std::thread::spawn(move || {
            // Background QoS so the capture's drain loop wins every contest
            // for a core; `drainbench` measures what it loses.
            background_qos();
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

    /// The meter starts at `Transcribing`, not `Capturing`, so a job the
    /// worker has not touched is never drawn as a recording.
    pub fn push(&mut self, dir: PathBuf) {
        let meter = Arc::new(Meter::default());
        meter.set_phase(MeterPhase::Transcribing);
        let job = Job {
            dir: dir.clone(),
            meter: meter.clone(),
        };
        // Fails only once the worker is gone, which `poll` reports per job.
        self.tx.send(Job { dir, meter }).ok();
        self.pending.push_back(job);
    }

    /// Serial and FIFO, so a result always belongs to the front of `pending`.
    pub fn poll(&mut self) -> Option<Outcome> {
        match self.done.try_recv() {
            Ok(o) => {
                self.pending.pop_front();
                Some(o)
            }
            Err(TryRecvError::Empty) => None,
            // The worker is gone: fail pending jobs one per poll, each against
            // its own session.
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

/// Put the calling thread at background QoS, as the transcription worker
/// runs, so the capture threads keep the CPU they need.
pub fn background_qos() {
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Sender as StdSender;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

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

    #[test]
    fn the_summary_names_the_front_and_counts_the_waiting() {
        let (mut q, gate) = gated();
        q.push(PathBuf::from("/s/a"));
        assert!(q.summary().unwrap().starts_with("Transcribing a"));
        q.push(PathBuf::from("/s/b"));
        q.push(PathBuf::from("/s/c"));
        // Wait for the fake to pick up the front job.
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

    /// Otherwise a deferred quit would wait on pending jobs for ever.
    #[test]
    fn a_dead_worker_fails_every_pending_job_rather_than_hanging() {
        // The panic message on stderr is expected.
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
