//! The session coordinator (spec §3.2): one task owns the sidecar, the segment log and the
//! recorder's instructions; the source, the recorder, the STT writer and recovery are workers
//! behind bounded channels.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::audio::frame::{Frame, FRAME_SAMPLES};
use crate::audio::recorder::Recorder;
use crate::audio::source::{Source, SourceEvent};
use crate::session::segments::{transcript_path, NewSegment, Segment, SegmentLog, SegmentSource};
use crate::session::sidecar::{sidecar_path, Gap, GapKind, RecState, RecordingEntry, Sidecar};
use crate::session::spend::{Spend, SpendKind, BATCH_USD_PER_SECOND, STREAM_USD_PER_SECOND};
use crate::stt::rest::{RecoverEvent, RecoverJob, RecoveryLink};
use crate::stt::stream::{SttEvent, SttInput, SttLink};

/// Frames the recorder may fall behind before frames become a recorder gap (5 s).
const FRAME_QUEUE: usize = 50;
/// Slots of the STT queue that frames never take, so Begin, End and cutoffs always fit.
const STT_CONTROL_RESERVE: usize = 8;

#[derive(Default)]
pub struct SessionConfig {
    pub dir: PathBuf,
    pub stem: String,
    /// Live transcription (spec §5); None records audio only.
    pub stt: Option<SttLink>,
    /// REST recovery of transcript gaps (spec §5.4).
    pub recovery: Option<RecoveryLink>,
    /// The ledger speech-to-text costs are written to (spec §8).
    pub spend: Option<Spend>,
    /// The transcript file; None is the standard name (`lecture_transcript_YYYYMMDD.txt`).
    pub transcript: Option<PathBuf>,
}

#[derive(Debug)]
pub enum Notification {
    Recording { path: PathBuf },
    Level(f32),
    Gap(Gap),
    DeviceGone { uid: String },
    DeviceBack { uid: String },
    Failed(String),
    Stt(SttStatus),
    /// The open utterance (display only).
    Open { stable: String, tentative: String },
    /// A segment committed to the log and the transcript.
    Segment(Segment),
    /// Recovery committed a transcript gap's whole interval.
    Recovered(Gap),
    RecoveryFailed(String),
    /// The spend ledger could not be written (spec §10: a warning; nothing else stops).
    SpendFailed(String),
    /// The audio has ended; the session is draining (transcript flush, recovery, stores).
    SourceEnded,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SttStatus {
    Connected,
    Retrying { after: Duration, reason: String },
    Refused(String),
    ServerError(String),
    Stopped(String),
}

#[derive(Debug, Default)]
pub struct StopReport {
    pub recordings: Vec<(PathBuf, u64)>,
    pub gaps: usize,
    pub stream_errors: u64,
    pub segments: u64,
    /// Transcript gaps still waiting for recovery.
    pub unresolved: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutoffResult {
    /// The transcript is committed through the cutoff (spec §5.3); false leaves the rest pending.
    pub confirmed: bool,
    /// Segments in the log when the cutoff settled: a snapshot takes the log up to here.
    pub segments: u64,
}

enum Command {
    Stop,
    Cutoff(oneshot::Sender<CutoffResult>),
}

pub struct SessionHandle {
    cmd: mpsc::Sender<Command>,
    task: JoinHandle<Result<StopReport>>,
}

impl SessionHandle {
    pub fn request_stop(&self) {
        let _ = self.cmd.try_send(Command::Stop);
    }

    /// Takes a snapshot cutoff at the audio forwarded so far and waits until the STT writer settles it (spec §5.3).
    pub async fn cutoff(&self) -> Result<CutoffResult> {
        let (tx, rx) = oneshot::channel();
        self.cmd.send(Command::Cutoff(tx)).await.map_err(|_| anyhow!("the session has ended"))?;
        rx.await.map_err(|_| anyhow!("the session ended before the cutoff settled"))
    }

    /// Waits for the session to end (after `request_stop`, or when the source ends) and every `Store`
    /// of it has been dropped.
    pub async fn finish(self) -> Result<StopReport> {
        let Self { cmd, task } = self;
        drop(cmd);
        task.await?
    }
}

/// A store update and, once the coordinator has saved its result, the reply to send.
type StoreJob = Box<dyn FnOnce(&mut Sidecar) -> StoreReply + Send>;
type StoreReply = Box<dyn FnOnce(Result<()>) + Send>;

/// The sidecar's one writer, however it is reached (spec §3.2): the coordinator while a session runs,
/// the file itself when none does.
#[derive(Clone)]
pub struct Store(StoreInner);

#[derive(Clone)]
enum StoreInner {
    Session { jobs: mpsc::Sender<StoreJob>, cmd: mpsc::Sender<Command> },
    Offline { sidecar: Arc<std::sync::Mutex<Sidecar>>, path: PathBuf },
}

impl Store {
    pub fn offline(sidecar: Sidecar, path: PathBuf) -> Self {
        Self(StoreInner::Offline { sidecar: Arc::new(std::sync::Mutex::new(sidecar)), path })
    }

    /// Runs `f` on the sidecar and saves it; in a session, inside the coordinator. When it returns,
    /// the result is on disk.
    pub async fn update<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Sidecar) -> Result<T> + Send + 'static,
    {
        match &self.0 {
            StoreInner::Session { jobs, .. } => {
                let (tx, rx) = oneshot::channel();
                let job: StoreJob = Box::new(move |sc| {
                    let out = f(sc);
                    Box::new(move |saved: Result<()>| {
                        let _ = tx.send(saved.and(out));
                    })
                });
                jobs.send(job).await.map_err(|_| anyhow!("the session has ended"))?;
                rx.await.map_err(|_| anyhow!("the session ended before the update ran"))?
            }
            StoreInner::Offline { sidecar, path } => {
                let mut sc = sidecar.lock().expect("the sidecar lock");
                let out = f(&mut sc);
                sc.save(path)?;
                out
            }
        }
    }

    pub async fn read(&self) -> Result<Sidecar> {
        self.update(|sc| Ok(sc.clone())).await
    }

    /// A snapshot cutoff (spec §5.3); None without a session.
    pub async fn cutoff(&self) -> Result<Option<CutoffResult>> {
        let StoreInner::Session { cmd, .. } = &self.0 else { return Ok(None) };
        let (tx, rx) = oneshot::channel();
        cmd.send(Command::Cutoff(tx)).await.map_err(|_| anyhow!("the session has ended"))?;
        Ok(Some(rx.await.map_err(|_| anyhow!("the session ended before the cutoff settled"))?))
    }
}

pub fn spawn(cfg: SessionConfig, source: Box<dyn Source>) -> (SessionHandle, mpsc::Receiver<Notification>) {
    let (handle, notes, _store) = spawn_with_store(cfg, source);
    (handle, notes)
}

/// A session and a `Store` of it. The session serves its stores until every clone is dropped, so an
/// update in flight when the audio ends still goes through the one writer.
pub fn spawn_with_store(cfg: SessionConfig, source: Box<dyn Source>) -> (SessionHandle, mpsc::Receiver<Notification>, Store) {
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let (store_tx, store_rx) = mpsc::channel(8);
    let (notify_tx, notify_rx) = mpsc::channel(256);
    let task = tokio::spawn(run(cfg, source, cmd_rx, store_rx, notify_tx));
    let store = Store(StoreInner::Session { jobs: store_tx, cmd: cmd_tx.clone() });
    (SessionHandle { cmd: cmd_tx, task }, notify_rx, store)
}

enum RecMsg {
    Open { recording_id: Uuid, recorder: Recorder },
    Frame(Frame),
    Finish { recording_id: Uuid },
}

enum RecorderEvent {
    Finished { recording_id: Uuid, path: PathBuf, samples: u64 },
    Failed { path: PathBuf, error: String },
}

fn finalize(id: Uuid, r: Recorder, events: &mpsc::Sender<RecorderEvent>) {
    let path = r.path().to_path_buf();
    let samples = r.samples();
    let e = match r.finalize() {
        Ok(path) => RecorderEvent::Finished { recording_id: id, path, samples },
        Err(e) => RecorderEvent::Failed { path, error: format!("{e:#}") },
    };
    let _ = events.blocking_send(e);
}

/// Recorder thread: writes and finalizes; never touches the network or the sidecar.
fn recorder_worker(mut rx: mpsc::Receiver<RecMsg>, events: mpsc::Sender<RecorderEvent>) {
    let mut current: Option<(Uuid, Recorder)> = None;
    while let Some(msg) = rx.blocking_recv() {
        match msg {
            RecMsg::Open { recording_id, recorder } => current = Some((recording_id, recorder)),
            RecMsg::Frame(f) => {
                if let Some((_, r)) = current.as_mut().filter(|(id, _)| *id == f.recording_id) {
                    if let Err(e) = r.write_frame(&f) {
                        let _ = events.blocking_send(RecorderEvent::Failed { path: r.path().to_path_buf(), error: format!("{e:#}") });
                        current = None;
                    }
                }
            }
            RecMsg::Finish { recording_id } => match current.take() {
                Some((id, r)) if id == recording_id => finalize(id, r, &events),
                other => current = other,
            },
        }
    }
    if let Some((id, r)) = current.take() {
        finalize(id, r, &events);
    }
}

struct Coordinator {
    dir: PathBuf,
    sidecar_path: PathBuf,
    sidecar: Sidecar,
    dirty: bool,
    rec_tx: Option<mpsc::Sender<RecMsg>>, // None once the session is closing
    notify: mpsc::Sender<Notification>,
    overflow: Option<usize>, // index in sidecar.gaps of the recorder-overflow gap still growing
    report: StopReport,
    segments: Option<SegmentLog>,
    stt: Option<mpsc::Sender<SttInput>>,
    stt_events: Option<mpsc::Receiver<SttEvent>>,
    /// The STT worker ended mid-session: later audio is recorded as gaps for recovery.
    stt_lost: bool,
    recovery: Option<mpsc::UnboundedSender<RecoverJob>>,
    recovery_events: Option<mpsc::Receiver<RecoverEvent>>,
    /// The recording being forwarded, and the end of the last frame the STT writer got.
    current: Option<Uuid>,
    forwarded_to: u64,
    cutoffs: HashMap<u64, oneshot::Sender<CutoffResult>>,
    next_cutoff: u64,
    spend: Option<Spend>,
}

impl Coordinator {
    async fn save(&mut self) -> Result<()> {
        let (sc, path) = (self.sidecar.clone(), self.sidecar_path.clone());
        tokio::task::spawn_blocking(move || sc.save(&path)).await??;
        self.dirty = false;
        Ok(())
    }

    fn notify(&self, n: Notification) {
        let _ = self.notify.try_send(n); // notifications describe decisions already made; none is durable
    }

    fn recorder(&self) -> Result<&mpsc::Sender<RecMsg>> {
        self.rec_tx.as_ref().ok_or_else(|| anyhow!("recorder closed"))
    }

    fn segment_count(&self) -> u64 {
        self.segments.as_ref().map_or(0, |l| l.len())
    }

    async fn on_source(&mut self, ev: SourceEvent) -> Result<()> {
        match ev {
            SourceEvent::Begin { recording_id, anchor, source_uid, input_rate, .. } => {
                let dir = self.dir.join("recordings");
                let stem = format!("session_{}", anchor.format("%Y%m%d_%H%M%S"));
                let d = dir.clone();
                let recorder = tokio::task::spawn_blocking(move || Recorder::create(&d, &stem))
                    .await?
                    .with_context(|| format!("create a recording in {}", dir.display()))?;
                let path = recorder.path().to_path_buf();
                let file = path.strip_prefix(&self.dir).unwrap_or(&path).to_string_lossy().into_owned();
                self.sidecar.recordings.push(RecordingEntry { id: recording_id, file, anchor, source_uid, input_rate, samples: None, state: RecState::Open });
                if self.stt.is_some() || self.stt_lost {
                    // Its transcript is owed from the first sample, until a connection or a gap covers it.
                    self.sidecar.open_transcript(recording_id, 0);
                }
                self.save().await?;
                self.recorder()?.send(RecMsg::Open { recording_id, recorder }).await.map_err(|_| anyhow!("recorder stopped"))?;
                self.notify(Notification::Recording { path });
                self.current = Some(recording_id);
                self.forwarded_to = 0;
                self.stt_control(SttInput::Begin { recording_id });
            }
            SourceEvent::Frame(f) => {
                let for_stt = self.stt.is_some().then(|| f.clone());
                let (id, start) = (f.recording_id, f.sample_offset);
                match self.recorder()?.try_send(RecMsg::Frame(f)) {
                    Ok(()) => {
                        if self.overflow.take().is_some() {
                            self.save().await?;
                        }
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        let end = start + FRAME_SAMPLES as u64;
                        match self.overflow {
                            Some(i) => self.sidecar.gaps[i].end_sample = Some(end),
                            None => {
                                let gap = Gap::new(id, start, Some(end), GapKind::RecorderOverflow);
                                self.sidecar.gaps.push(gap.clone());
                                self.overflow = Some(self.sidecar.gaps.len() - 1);
                                self.save().await?;
                                self.notify(Notification::Gap(gap));
                            }
                        }
                        self.dirty = true;
                    }
                    // The recorder thread closes its queue only by ending, and while the session runs only a panic ends it.
                    Err(mpsc::error::TrySendError::Closed(_)) => return Err(anyhow!("the recorder thread stopped unexpectedly")),
                }
                if let Some(f) = for_stt {
                    self.stt_frame(f);
                }
            }
            SourceEvent::Gap(g) => {
                self.sidecar.gaps.push(g.clone());
                self.save().await?;
                self.notify(Notification::Gap(g));
            }
            SourceEvent::End { recording_id, samples, stream_errors } => {
                self.report.stream_errors += stream_errors;
                self.overflow = None;
                self.recorder()?.send(RecMsg::Finish { recording_id }).await.map_err(|_| anyhow!("recorder stopped"))?;
                self.stt_control(SttInput::End { recording_id, samples });
                if self.stt_lost {
                    self.untranscribed(recording_id, samples).await?;
                }
                self.current = None;
            }
            SourceEvent::Level(l) => self.notify(Notification::Level(l)),
            SourceEvent::DeviceGone { uid } => self.notify(Notification::DeviceGone { uid }),
            SourceEvent::DeviceBack { uid } => self.notify(Notification::DeviceBack { uid }),
            SourceEvent::Failed(msg) => return Err(anyhow!(msg)),
        }
        Ok(())
    }

    /// Forwards a frame unless the queue is down to its reserve: dropped, never waited for. The
    /// writer sees the hole in the offsets and reports the gap.
    fn stt_frame(&mut self, f: Frame) {
        let Some(tx) = &self.stt else { return };
        if tx.capacity() <= STT_CONTROL_RESERVE {
            return;
        }
        let end = f.sample_offset + f.valid_samples as u64;
        match tx.try_send(SttInput::Frame(f)) {
            Ok(()) => self.forwarded_to = end,
            Err(mpsc::error::TrySendError::Full(_)) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => self.stt_gone(),
        }
    }

    fn stt_control(&mut self, m: SttInput) {
        let Some(tx) = &self.stt else { return };
        if tx.try_send(m).is_err() {
            self.stt_gone(); // the reserve always has room for these: the worker is stuck or gone
        }
    }

    /// The STT worker ended mid-session. Recording continues; the audio it will not transcribe
    /// becomes gaps for recovery.
    fn stt_gone(&mut self) {
        if self.stt_lost {
            return;
        }
        self.stt = None;
        self.stt_events = None;
        self.stt_lost = true;
        let segments = self.segment_count();
        for (_, reply) in self.cutoffs.drain() {
            let _ = reply.send(CutoffResult { confirmed: false, segments });
        }
        self.notify(Notification::Stt(SttStatus::Stopped("the transcription worker stopped; recording continues".into())));
    }

    /// With the STT worker gone, this recording's audio after what was committed becomes a gap.
    async fn untranscribed(&mut self, recording_id: Uuid, samples: u64) -> Result<()> {
        let marker = self.sidecar.close_transcript(recording_id);
        let logged = self.segments.as_ref().and_then(|l| l.last_end(recording_id)).unwrap_or(0);
        let from = marker.map_or(0, |m| m.from_sample).max(logged);
        if samples > from {
            let g = self.add_gap(Gap::new(recording_id, from, Some(samples), GapKind::SttInterrupted));
            self.recover(&g);
        }
        self.save().await
    }

    /// A computed speech-to-text cost (spec §8); a ledger that cannot be written is a warning, never a stop (§10).
    fn spend_audio(&self, samples: u64, usd_per_second: f64) {
        let Some(spend) = &self.spend else { return };
        let secs = samples as f64 / crate::audio::recorder::SAMPLE_RATE as f64;
        if let Err(e) = spend.add(SpendKind::Transcribe, secs * usd_per_second, false, Some(secs)) {
            self.notify(Notification::SpendFailed(format!("{e:#}")));
        }
    }

    fn add_gap(&mut self, g: Gap) -> Gap {
        self.sidecar.gaps.push(g.clone());
        self.notify(Notification::Gap(g.clone()));
        g
    }

    /// Queues recovery of a transcript gap, from where the segment log shows it is not yet committed.
    fn recover(&mut self, g: &Gap) {
        let (Some(jobs), Some(end)) = (&self.recovery, g.end_sample) else { return };
        let Some(r) = self.sidecar.recordings.iter().find(|r| r.id == g.recording_id) else { return };
        let done = self.segments.as_ref().and_then(|l| l.committed_within(g.recording_id, g.start_sample, end));
        let job = RecoverJob { recording_id: g.recording_id, gap_start: g.start_sample, from: done.unwrap_or(g.start_sample).max(g.start_sample), end, wav: self.dir.join(&r.file) };
        if jobs.send(job).is_err() {
            self.recovery = None;
        }
    }

    /// Appends a segment to the log and the transcript on a blocking thread (spec §3.2).
    async fn commit(&mut self, s: NewSegment) -> Result<()> {
        let anchor = self.sidecar.recordings.iter().find(|r| r.id == s.recording_id).map(|r| r.anchor).context("a segment for a recording the sidecar does not know")?;
        let mut log = self.segments.take().context("transcription without a segment log")?;
        let (log, seg) = tokio::task::spawn_blocking(move || {
            let seg = log.append(s, anchor);
            (log, seg)
        })
        .await?;
        self.segments = Some(log);
        let seg = seg.context("append to the segment log")?;
        self.notify(Notification::Segment(seg));
        Ok(())
    }

    async fn on_stt(&mut self, ev: SttEvent) -> Result<()> {
        match ev {
            SttEvent::Connected { recording_id, origin, gap } => {
                let gap = gap.map(|g| self.add_gap(g));
                self.sidecar.open_transcript(recording_id, origin);
                self.save().await?; // the gap and the new origin in one write (spec §5.4)
                if let Some(g) = gap {
                    self.recover(&g);
                }
                self.notify(Notification::Stt(SttStatus::Connected));
            }
            SttEvent::Open { stable, tentative, .. } => self.notify(Notification::Open { stable, tentative }),
            SttEvent::Utterance { recording_id, utterance: u } => {
                self.commit(NewSegment { recording_id, start_sample: u.start_sample, end_sample: u.end_sample, text: u.text, words: u.words, source: SegmentSource::Live }).await?;
            }
            SttEvent::Ended { recording_id, gap } => {
                let gap = gap.map(|g| self.add_gap(g));
                self.sidecar.close_transcript(recording_id);
                self.save().await?;
                if let Some(g) = gap {
                    self.recover(&g);
                }
            }
            SttEvent::Cutoff { id, confirmed } => {
                if let Some(reply) = self.cutoffs.remove(&id) {
                    let _ = reply.send(CutoffResult { confirmed, segments: self.segment_count() });
                }
            }
            SttEvent::Streamed { samples, .. } => self.spend_audio(samples, STREAM_USD_PER_SECOND),
            SttEvent::Retrying { after, reason } => self.notify(Notification::Stt(SttStatus::Retrying { after, reason })),
            SttEvent::ServerError(m) => self.notify(Notification::Stt(SttStatus::ServerError(m))),
            SttEvent::Refused(m) => self.notify(Notification::Stt(SttStatus::Refused(m))),
        }
        Ok(())
    }

    async fn on_recovery(&mut self, ev: RecoverEvent) -> Result<()> {
        match ev {
            RecoverEvent::Piece { recording_id, start_sample, end_sample, text, words, .. } => {
                self.spend_audio(end_sample - start_sample, BATCH_USD_PER_SECOND);
                if !text.is_empty() {
                    self.commit(NewSegment { recording_id, start_sample, end_sample, text, words, source: SegmentSource::Recovered }).await?;
                }
            }
            RecoverEvent::Done { recording_id, gap_start } => {
                let found = self.sidecar.gaps.iter_mut().find(|g| g.recording_id == recording_id && g.start_sample == gap_start && g.kind.is_transcript());
                if let Some(g) = found {
                    g.resolved = true;
                    let g = g.clone();
                    self.save().await?;
                    self.notify(Notification::Recovered(g));
                }
            }
            RecoverEvent::Failed { message, refused, .. } => {
                if refused {
                    self.recovery = None;
                }
                self.notify(Notification::RecoveryFailed(message));
            }
        }
        Ok(())
    }

    fn cutoff(&mut self, reply: oneshot::Sender<CutoffResult>) {
        let segments = self.segment_count();
        let Some(recording_id) = self.current.filter(|_| self.stt.is_some()) else {
            // Without live STT nothing is confirmed. Between recordings, one whose live transcript is
            // still being flushed (its mark is still set) may yet commit its last sentence.
            let settled = self.stt.is_some() && self.sidecar.open_utterances.is_empty();
            let _ = reply.send(CutoffResult { confirmed: settled, segments });
            return;
        };
        let id = self.next_cutoff;
        self.next_cutoff += 1;
        self.cutoffs.insert(id, reply);
        self.stt_control(SttInput::Cutoff { id, recording_id, sample: self.forwarded_to });
    }

    /// A store update (spec §3.2): run against a copy on a blocking thread, adopted, saved, and only
    /// then answered, so an update that returns is durable.
    async fn run_job(&mut self, job: StoreJob) -> Result<()> {
        let sc = self.sidecar.clone();
        let (sc, reply) = tokio::task::spawn_blocking(move || {
            let mut sc = sc;
            let reply = job(&mut sc);
            (sc, reply)
        })
        .await?;
        self.sidecar = sc;
        match self.save().await {
            Ok(()) => {
                reply(Ok(()));
                Ok(())
            }
            Err(e) => {
                reply(Err(anyhow!("save the sidecar: {e:#}")));
                Err(e)
            }
        }
    }

    async fn on_recorder(&mut self, ev: RecorderEvent) -> Result<()> {
        match ev {
            RecorderEvent::Finished { recording_id, path, samples } => {
                if let Some(r) = self.sidecar.recording_mut(recording_id) {
                    r.samples = Some(samples);
                    r.state = RecState::Finalized;
                }
                self.save().await?;
                self.report.recordings.push((path, samples));
                Ok(())
            }
            RecorderEvent::Failed { path, error } => Err(anyhow!("recording to {} failed: {error}", path.display())),
        }
    }
}

async fn recv<T>(rx: &mut Option<mpsc::Receiver<T>>) -> Option<T> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn run(cfg: SessionConfig, source: Box<dyn Source>, mut cmd_rx: mpsc::Receiver<Command>, store_rx: mpsc::Receiver<StoreJob>, notify: mpsc::Sender<Notification>) -> Result<StopReport> {
    let mut store_rx = Some(store_rx);
    let sidecar_path = sidecar_path(&cfg.dir, &cfg.stem);
    let sidecar = Sidecar::load(&sidecar_path)?.unwrap_or_default();
    let segments = if cfg.stt.is_some() || cfg.recovery.is_some() {
        let (dir, stem) = (cfg.dir.clone(), cfg.stem.clone());
        let transcript = cfg.transcript.clone().unwrap_or_else(|| transcript_path(&dir, &stem));
        Some(tokio::task::spawn_blocking(move || SegmentLog::open_at(&dir, &stem, &transcript)).await??)
    } else {
        None
    };
    let stop = Arc::new(AtomicBool::new(false));
    let (src_tx, mut src_rx) = mpsc::channel(256);
    let source_thread = {
        let stop = stop.clone();
        std::thread::spawn(move || source.run(src_tx, stop))
    };
    let (rec_tx, rec_rx) = mpsc::channel(FRAME_QUEUE);
    let (rev_tx, mut rev_rx) = mpsc::channel(16);
    let recorder_thread = std::thread::spawn(move || recorder_worker(rec_rx, rev_tx));
    let (stt, stt_events) = cfg.stt.map_or((None, None), |l| (Some(l.input), Some(l.events)));
    let (recovery, recovery_events) = cfg.recovery.map_or((None, None), |l| (Some(l.jobs), Some(l.events)));

    let mut c = Coordinator {
        dir: cfg.dir,
        sidecar_path,
        sidecar,
        dirty: false,
        rec_tx: Some(rec_tx),
        notify,
        overflow: None,
        report: StopReport::default(),
        segments,
        stt,
        stt_events,
        stt_lost: false,
        recovery,
        recovery_events,
        current: None,
        forwarded_to: 0,
        cutoffs: HashMap::new(),
        next_cutoff: 0,
        spend: cfg.spend,
    };
    // Transcript gaps an earlier session left (a crash, recovery cut short) are recovered first.
    let pending: Vec<Gap> = c.sidecar.gaps.iter().filter(|g| g.kind.is_transcript() && !g.resolved).cloned().collect();
    for g in &pending {
        c.recover(g);
    }

    let mut failure: Option<anyhow::Error> = None;
    let fail = |e: anyhow::Error, failure: &mut Option<anyhow::Error>| {
        stop.store(true, Ordering::Relaxed);
        failure.get_or_insert(e);
    };
    loop {
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => match cmd {
                Command::Stop => stop.store(true, Ordering::Relaxed),
                Command::Cutoff(reply) => c.cutoff(reply),
            },
            ev = src_rx.recv() => match ev {
                Some(ev) => if failure.is_none() {
                    if let Err(e) = c.on_source(ev).await { fail(e, &mut failure) }
                },
                None => break, // the source thread has exited
            },
            Some(ev) = rev_rx.recv() => if let Err(e) = c.on_recorder(ev).await { fail(e, &mut failure) },
            ev = recv(&mut c.stt_events), if c.stt_events.is_some() => match ev {
                Some(ev) => if let Err(e) = c.on_stt(ev).await { fail(e, &mut failure) },
                None => c.stt_gone(),
            },
            ev = recv(&mut c.recovery_events), if c.recovery_events.is_some() => match ev {
                Some(ev) => if let Err(e) = c.on_recovery(ev).await { fail(e, &mut failure) },
                None => c.recovery_events = None,
            },
            job = recv(&mut store_rx), if store_rx.is_some() => match job {
                Some(job) => if let Err(e) = c.run_job(job).await { fail(e, &mut failure) },
                None => store_rx = None,
            },
        }
    }
    c.notify(Notification::SourceEnded);
    // The source has ended: close the recorder and STT queues and drain every worker. Recovery's
    // queue closes once STT can raise no more gaps, and recovery finishes what it holds. Stores are
    // served until the last one is dropped.
    c.rec_tx = None;
    c.stt = None;
    let mut recorder_open = true;
    while recorder_open || c.stt_events.is_some() || c.recovery_events.is_some() || store_rx.is_some() {
        if c.stt_events.is_none() {
            c.recovery = None;
        }
        tokio::select! {
            ev = rev_rx.recv(), if recorder_open => match ev {
                Some(ev) => if let Err(e) = c.on_recorder(ev).await { failure.get_or_insert(e); },
                None => recorder_open = false,
            },
            ev = recv(&mut c.stt_events), if c.stt_events.is_some() => match ev {
                Some(ev) => if let Err(e) = c.on_stt(ev).await { failure.get_or_insert(e); },
                None => c.stt_events = None,
            },
            ev = recv(&mut c.recovery_events), if c.recovery_events.is_some() => match ev {
                Some(ev) => if let Err(e) = c.on_recovery(ev).await { failure.get_or_insert(e); },
                None => c.recovery_events = None,
            },
            job = recv(&mut store_rx), if store_rx.is_some() => match job {
                Some(job) => if let Err(e) = c.run_job(job).await { failure.get_or_insert(e); },
                None => store_rx = None,
            },
            Some(cmd) = cmd_rx.recv() => match cmd {
                // A stop while draining: stop waiting for recovery; its gaps wait for the next session.
                Command::Stop => {
                    if c.recovery_events.take().is_some() {
                        c.recovery = None;
                        c.notify(Notification::RecoveryFailed("recovery stopped by a second stop; its gaps wait for the next session".into()));
                    }
                }
                Command::Cutoff(reply) => c.cutoff(reply),
            },
        }
    }
    let segments = c.segment_count();
    for (_, reply) in c.cutoffs.drain() {
        let _ = reply.send(CutoffResult { confirmed: false, segments });
    }
    tokio::task::spawn_blocking(move || {
        let _ = source_thread.join();
        let _ = recorder_thread.join();
    })
    .await?;
    if c.dirty {
        if let Err(e) = c.save().await {
            failure.get_or_insert(e); // a failing last save never hides the error that ended the session
        }
    }
    c.report.gaps = c.sidecar.gaps.len();
    c.report.segments = segments;
    c.report.unresolved = c.sidecar.gaps.iter().filter(|g| g.kind.is_transcript() && !g.resolved).count();
    match failure {
        Some(e) => {
            c.notify(Notification::Failed(format!("{e:#}")));
            Err(e)
        }
        None => Ok(c.report),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::frame::Frame;
    use chrono::{Local, TimeZone};
    use std::path::Path;

    const STEM: &str = "lecture_notes_20260925";

    struct Script(Vec<SourceEvent>);
    impl Source for Script {
        fn run(self: Box<Self>, out: mpsc::Sender<SourceEvent>, _stop: Arc<AtomicBool>) {
            for e in self.0 {
                out.blocking_send(e).unwrap();
            }
        }
    }

    /// Sends frames until stopped, like a live device.
    struct Endless;
    impl Source for Endless {
        fn run(self: Box<Self>, out: mpsc::Sender<SourceEvent>, stop: Arc<AtomicBool>) {
            let id = Uuid::new_v4();
            out.blocking_send(begin(id, 0)).unwrap();
            let mut n = 0;
            while !stop.load(Ordering::Relaxed) {
                out.blocking_send(frame(id, n)).unwrap();
                n += 1600;
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            out.blocking_send(SourceEvent::End { recording_id: id, samples: n, stream_errors: 0 }).unwrap();
        }
    }

    fn begin(id: Uuid, secs: u32) -> SourceEvent {
        SourceEvent::Begin {
            recording_id: id,
            anchor: Local.with_ymd_and_hms(2026, 9, 25, 10, 0, secs).unwrap(),
            source_uid: "Receiver_UID".into(),
            input_rate: 48_000,
            channels: 1,
        }
    }

    fn frame(id: Uuid, offset: u64) -> SourceEvent {
        SourceEvent::Frame(Frame { recording_id: id, sample_offset: offset, valid_samples: 1600, pcm16: [1000; 1600] })
    }

    fn cfg(dir: &Path) -> SessionConfig {
        SessionConfig { dir: dir.to_path_buf(), stem: STEM.into(), ..Default::default() }
    }

    async fn run(dir: &Path, source: impl Source) -> (Result<StopReport>, Vec<Notification>) {
        let (handle, mut notes) = spawn(cfg(dir), Box::new(source));
        let result = handle.finish().await;
        let mut seen = Vec::new();
        while let Ok(n) = notes.try_recv() {
            seen.push(n);
        }
        (result, seen)
    }

    fn samples(path: &Path) -> Vec<i16> {
        hound::WavReader::open(path).unwrap().samples::<i16>().map(|s| s.unwrap()).collect()
    }

    #[tokio::test]
    async fn device_gone_and_back_gives_two_recordings_and_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let overflow = Gap { recording_id: a, start_sample: 3200, end_sample: Some(4800), kind: GapKind::CaptureOverflow, resolved: false };
        let gone = Gap { recording_id: a, start_sample: 6400, end_sample: None, kind: GapKind::DeviceGone, resolved: false };
        let script = Script(vec![
            begin(a, 0),
            frame(a, 0),
            frame(a, 1600),
            SourceEvent::Gap(overflow.clone()),
            frame(a, 4800),
            SourceEvent::End { recording_id: a, samples: 6400, stream_errors: 2 },
            SourceEvent::Gap(gone.clone()),
            SourceEvent::DeviceGone { uid: "Receiver_UID".into() },
            SourceEvent::DeviceBack { uid: "Receiver_UID".into() },
            begin(b, 9),
            frame(b, 0),
            SourceEvent::End { recording_id: b, samples: 1600, stream_errors: 0 },
        ]);
        let (report, notes) = run(dir.path(), script).await;
        let report = report.unwrap();

        assert_eq!(report.recordings.len(), 2);
        assert_eq!(report.stream_errors, 2);
        let first = samples(&report.recordings[0].0);
        assert_eq!(first.len(), 6400);
        assert!(first[3200..4800].iter().all(|&s| s == 0), "the lost interval is silence");
        assert!(first[..3200].iter().chain(&first[4800..]).all(|&s| s == 1000));
        assert_eq!(samples(&report.recordings[1].0).len(), 1600);

        let sc = Sidecar::load(&sidecar_path(dir.path(), "lecture_notes_20260925")).unwrap().unwrap();
        assert_eq!(sc.recordings.len(), 2);
        assert!(sc.recordings.iter().all(|r| r.state == RecState::Finalized && r.source_uid == "Receiver_UID"));
        assert_eq!(sc.recordings[0].samples, Some(6400));
        assert_eq!(sc.recordings[1].anchor, Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 9).unwrap());
        assert_eq!(sc.gaps, vec![overflow, gone]);
        assert!(notes.iter().any(|n| matches!(n, Notification::DeviceGone { .. })));
        assert!(notes.iter().any(|n| matches!(n, Notification::DeviceBack { .. })));
    }

    #[tokio::test]
    async fn stop_request_ends_the_session_with_a_finalized_recording() {
        let dir = tempfile::tempdir().unwrap();
        let (handle, _notes) = spawn(cfg(dir.path()), Box::new(Endless));
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        handle.request_stop();
        let report = handle.finish().await.unwrap();
        assert_eq!(report.recordings.len(), 1);
        let (path, n) = &report.recordings[0];
        assert!(*n > 0 && n % 1600 == 0);
        assert_eq!(samples(path).len() as u64, *n);
    }

    #[tokio::test]
    async fn unwritable_recordings_dir_stops_with_the_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let rec = dir.path().join("recordings");
        std::fs::create_dir_all(&rec).unwrap();
        std::fs::set_permissions(&rec, std::fs::Permissions::from_mode(0o555)).unwrap();
        let id = Uuid::new_v4();
        let (result, notes) = run(dir.path(), Script(vec![begin(id, 0), frame(id, 0)])).await;
        std::fs::set_permissions(&rec, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("recordings"), "{err}");
        assert!(notes.iter().any(|n| matches!(n, Notification::Failed(_))));
    }

    #[tokio::test]
    async fn a_source_that_never_begins_leaves_no_recording() {
        let dir = tempfile::tempdir().unwrap();
        let (report, _) = run(dir.path(), Script(vec![])).await;
        assert!(report.unwrap().recordings.is_empty());
        assert!(!sidecar_path(dir.path(), "lecture_notes_20260925").exists());
    }

    #[tokio::test]
    async fn source_failure_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (result, _) = run(dir.path(), Script(vec![SourceEvent::Failed("input X not found".into())])).await;
        assert!(format!("{:#}", result.unwrap_err()).contains("input X not found"));
    }

    use crate::session::segments::{self, segments_path, transcript_path, NewSegment, SegmentLog, SegmentSource};
    use crate::stt::rest::{RecoverEvent, RecoverJob, RecoveryLink};
    use crate::stt::stream::{SttEvent, SttInput, SttLink, INPUT_QUEUE};
    use crate::stt::transcript::{Utterance, Word};
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    fn scripted_stt(mut script: impl FnMut(SttInput) -> Vec<SttEvent> + Send + 'static) -> SttLink {
        let (input, mut rx) = mpsc::channel(INPUT_QUEUE);
        let (tx, events) = mpsc::channel(64);
        tokio::spawn(async move {
            while let Some(i) = rx.recv().await {
                for e in script(i) {
                    if tx.send(e).await.is_err() {
                        return;
                    }
                }
            }
        });
        SttLink { input, events }
    }

    fn scripted_recovery(mut script: impl FnMut(RecoverJob) -> Vec<RecoverEvent> + Send + 'static) -> RecoveryLink {
        let (jobs, mut rx) = mpsc::unbounded_channel();
        let (tx, events) = mpsc::channel(64);
        tokio::spawn(async move {
            while let Some(j) = rx.recv().await {
                for e in script(j) {
                    if tx.send(e).await.is_err() {
                        return;
                    }
                }
            }
        });
        RecoveryLink { jobs, events }
    }

    async fn run_with(cfg: SessionConfig, source: impl Source) -> (Result<StopReport>, Vec<Notification>) {
        let (handle, mut notes) = spawn(cfg, Box::new(source));
        let result = handle.finish().await;
        let mut seen = Vec::new();
        while let Ok(n) = notes.try_recv() {
            seen.push(n);
        }
        (result, seen)
    }

    fn word(text: &str, s: u64, e: u64) -> Word {
        Word { text: text.into(), start_sample: s, end_sample: e }
    }

    /// Begin, `frames` frames, End.
    fn recording(id: Uuid, frames: u64) -> Vec<SourceEvent> {
        let mut v = vec![begin(id, 0)];
        v.extend((0..frames).map(|k| frame(id, k * 1600)));
        v.push(SourceEvent::End { recording_id: id, samples: frames * 1600, stream_errors: 0 });
        v
    }

    /// Sends `before`, then waits for `go` before sending `after`, like a device still recording.
    struct Gated {
        before: Vec<SourceEvent>,
        go: Arc<AtomicBool>,
        after: Vec<SourceEvent>,
    }

    impl Source for Gated {
        fn run(self: Box<Self>, out: mpsc::Sender<SourceEvent>, _stop: Arc<AtomicBool>) {
            for e in self.before {
                out.blocking_send(e).unwrap();
            }
            while !self.go.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            for e in self.after {
                out.blocking_send(e).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn live_utterances_and_gaps_are_committed_and_the_gaps_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4();
        let stt = scripted_stt(move |i| match i {
            SttInput::Frame(f) if f.sample_offset == 0 => vec![SttEvent::Connected { recording_id: a, origin: 0, gap: None }],
            SttInput::Frame(f) if f.sample_offset == 8_000 => vec![SttEvent::Utterance {
                recording_id: a,
                utterance: Utterance { start_sample: 1_600, end_sample: 8_000, text: "gradient descent".into(), words: vec![word("gradient", 1_600, 4_800), word("descent", 4_800, 8_000)] },
            }],
            SttInput::End { samples, .. } => vec![SttEvent::Ended { recording_id: a, gap: Some(Gap::new(a, 8_000, Some(samples), GapKind::SttOffline)) }],
            _ => vec![],
        });
        let recovery = scripted_recovery(|j| {
            vec![
                RecoverEvent::Piece { recording_id: j.recording_id, gap_start: j.gap_start, start_sample: j.from, end_sample: j.end, text: "recovered words".into(), words: vec![word("recovered", 9_600, 11_200), word("words", 11_200, 12_800)] },
                RecoverEvent::Done { recording_id: j.recording_id, gap_start: j.gap_start },
            ]
        });
        let (report, notes) = run_with(SessionConfig { stt: Some(stt), recovery: Some(recovery), ..cfg(dir.path()) }, Script(recording(a, 10))).await;
        let report = report.unwrap();
        assert_eq!((report.segments, report.unresolved), (2, 0));
        let segs = segments::read(&segments_path(dir.path(), STEM)).unwrap();
        assert_eq!(
            segs.iter().map(|s| (s.start_sample, s.end_sample, s.text.as_str(), s.source)).collect::<Vec<_>>(),
            vec![(1_600, 8_000, "gradient descent", SegmentSource::Live), (8_000, 16_000, "recovered words", SegmentSource::Recovered)]
        );
        assert_eq!(std::fs::read_to_string(transcript_path(dir.path(), STEM)).unwrap(), "[10:00:00] gradient descent\n[10:00:00] recovered words\n");
        let sc = Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap();
        assert_eq!(sc.gaps, vec![Gap { recording_id: a, start_sample: 8_000, end_sample: Some(16_000), kind: GapKind::SttOffline, resolved: true }]);
        assert!(sc.open_utterances.is_empty());
        assert!(notes.iter().any(|n| matches!(n, Notification::Recovered(_))));
        assert_eq!(notes.iter().filter(|n| matches!(n, Notification::Segment(_))).count(), 2);
    }

    #[tokio::test]
    async fn a_lost_stt_worker_leaves_its_audio_as_a_gap_and_recording_continues() {
        let dir = tempfile::tempdir().unwrap();
        let (input, gone) = mpsc::channel(INPUT_QUEUE);
        drop(gone);
        let (_worker_events, events) = mpsc::channel(1);
        let a = Uuid::new_v4();
        let (report, notes) = run_with(SessionConfig { stt: Some(SttLink { input, events }), ..cfg(dir.path()) }, Script(recording(a, 10))).await;
        let report = report.unwrap();
        assert_eq!(report.recordings[0].1, 16_000, "recording continued");
        assert_eq!(report.unresolved, 1);
        let sc = Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap();
        assert_eq!(sc.gaps, vec![Gap::new(a, 0, Some(16_000), GapKind::SttInterrupted)]);
        assert!(notes.iter().any(|n| matches!(n, Notification::Stt(SttStatus::Stopped(_)))));
    }

    #[tokio::test]
    async fn a_stuck_stt_worker_loses_frames_not_control_messages() {
        let dir = tempfile::tempdir().unwrap();
        let (input, mut rx) = mpsc::channel(INPUT_QUEUE);
        let (tx, events) = mpsc::channel::<SttEvent>(8);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await; // stuck while the whole recording arrives
            while let Some(i) = rx.recv().await {
                log.lock().unwrap().push(match i {
                    SttInput::Begin { .. } => "begin".to_string(),
                    SttInput::Frame(f) => (f.sample_offset / 1600).to_string(),
                    SttInput::End { .. } => "end".to_string(),
                    SttInput::Cutoff { .. } => "cutoff".to_string(),
                });
            }
            drop(tx);
        });
        let a = Uuid::new_v4();
        let (report, _) = run_with(SessionConfig { stt: Some(SttLink { input, events }), ..cfg(dir.path()) }, Script(recording(a, 200))).await;
        assert_eq!(report.unwrap().recordings[0].1, 200 * 1600, "the recorder is unaffected");
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.first().map(String::as_str), Some("begin"));
        assert_eq!(seen.last().map(String::as_str), Some("end"));
        assert!(seen.len() - 2 < 200, "frames were dropped, not waited for: {} arrived", seen.len() - 2);
    }

    #[tokio::test]
    async fn a_cutoff_is_settled_by_the_writer_after_the_utterance_that_covers_it() {
        let dir = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4();
        let frames_seen = Arc::new(AtomicUsize::new(0));
        let cut = Arc::new(Mutex::new(None));
        let (count, cut_seen) = (frames_seen.clone(), cut.clone());
        let stt = scripted_stt(move |i| match i {
            SttInput::Frame(_) => {
                count.fetch_add(1, Ordering::SeqCst);
                vec![]
            }
            SttInput::Cutoff { id, recording_id, sample } => {
                *cut_seen.lock().unwrap() = Some((recording_id, sample));
                vec![
                    SttEvent::Utterance { recording_id, utterance: Utterance { start_sample: 0, end_sample: sample, text: "momentum".into(), words: vec![] } },
                    SttEvent::Cutoff { id, confirmed: true },
                ]
            }
            SttInput::End { recording_id, .. } => vec![SttEvent::Ended { recording_id, gap: None }],
            _ => vec![],
        });
        let go = Arc::new(AtomicBool::new(false));
        let mut before = recording(a, 8);
        let after = before.split_off(6); // Begin and five frames, then the rest
        let (handle, _notes) = spawn(SessionConfig { stt: Some(stt), ..cfg(dir.path()) }, Box::new(Gated { before, go: go.clone(), after }));
        while frames_seen.load(Ordering::SeqCst) < 5 {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert_eq!(handle.cutoff().await.unwrap(), CutoffResult { confirmed: true, segments: 1 });
        assert_eq!(*cut.lock().unwrap(), Some((a, 8_000)));
        go.store(true, Ordering::Relaxed);
        handle.finish().await.unwrap();
    }

    #[tokio::test]
    async fn a_cutoff_without_stt_confirms_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (handle, _notes) = spawn(cfg(dir.path()), Box::new(Endless));
        assert_eq!(handle.cutoff().await.unwrap(), CutoffResult { confirmed: false, segments: 0 });
        handle.request_stop();
        handle.finish().await.unwrap();
    }

    #[tokio::test]
    async fn recovery_resumes_after_the_pieces_already_logged() {
        let dir = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4();
        let anchor = Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 0).unwrap();
        let mut sc = Sidecar::default();
        sc.recordings.push(RecordingEntry { id: a, file: "recordings/r.wav".into(), anchor, source_uid: "Receiver_UID".into(), input_rate: 48_000, samples: Some(48_000), state: RecState::Finalized });
        sc.gaps.push(Gap::new(a, 8_000, Some(40_000), GapKind::SttInterrupted));
        sc.save(&sidecar_path(dir.path(), STEM)).unwrap();
        let mut log = SegmentLog::open(dir.path(), STEM).unwrap();
        log.append(NewSegment { recording_id: a, start_sample: 8_000, end_sample: 24_000, text: "already recovered".into(), words: vec![], source: SegmentSource::Recovered }, anchor).unwrap();
        drop(log);
        let jobs = Arc::new(Mutex::new(Vec::new()));
        let seen = jobs.clone();
        let recovery = scripted_recovery(move |j| {
            seen.lock().unwrap().push(j.clone());
            vec![
                RecoverEvent::Piece { recording_id: j.recording_id, gap_start: j.gap_start, start_sample: j.from, end_sample: j.end, text: "the rest".into(), words: vec![] },
                RecoverEvent::Done { recording_id: j.recording_id, gap_start: j.gap_start },
            ]
        });
        let (report, _) = run_with(SessionConfig { recovery: Some(recovery), ..cfg(dir.path()) }, Script(vec![])).await;
        assert_eq!(report.unwrap().unresolved, 0);
        let jobs = jobs.lock().unwrap().clone();
        assert_eq!(jobs.len(), 1);
        assert_eq!((jobs[0].gap_start, jobs[0].from, jobs[0].end), (8_000, 24_000, 40_000));
        assert_eq!(jobs[0].wav, dir.path().join("recordings/r.wav"));
        assert_eq!(std::fs::read_to_string(transcript_path(dir.path(), STEM)).unwrap(), "[10:00:00] already recovered\n[10:00:01] the rest\n");
    }

    /// A copy of the lecture folder as a crash would leave it at this moment.
    fn crash_copy(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for e in std::fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            let to = dst.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                crash_copy(&e.path(), &to);
            } else {
                std::fs::copy(e.path(), &to).unwrap();
            }
        }
    }

    /// Waits until the sidecar lists `recording` and the recorder has written `samples` of it.
    async fn written(dir: &Path, recording: Uuid, samples: u64) {
        for _ in 0..5_000 {
            let sc = Sidecar::load(&sidecar_path(dir, STEM)).ok().flatten();
            let wav = sc.and_then(|sc| sc.recordings.iter().find(|r| r.id == recording).map(|r| dir.join(&r.file)));
            if wav.is_some_and(|w| std::fs::metadata(w).is_ok_and(|m| m.len() >= 44 + samples * 2)) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        panic!("the recorder never wrote {samples} samples of {recording}");
    }

    #[tokio::test]
    async fn a_crash_before_stt_ever_connected_leaves_the_audio_as_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4();
        let stt = scripted_stt(|i| match i {
            SttInput::Begin { .. } => vec![SttEvent::Refused("400 Bad Request: Incorrect API key provided.".into())],
            SttInput::End { recording_id, samples } => vec![SttEvent::Ended { recording_id, gap: Some(Gap::new(recording_id, 0, Some(samples), GapKind::SttRefused)) }],
            _ => vec![],
        });
        let go = Arc::new(AtomicBool::new(false));
        let mut before = recording(a, 40);
        let after = before.split_off(41); // Begin and forty frames, then End
        let (handle, _notes) = spawn(SessionConfig { stt: Some(stt), ..cfg(dir.path()) }, Box::new(Gated { before, go: go.clone(), after }));
        written(dir.path(), a, 48_000).await;
        let crashed = tempfile::tempdir().unwrap();
        crash_copy(dir.path(), crashed.path());
        go.store(true, Ordering::Relaxed);
        handle.finish().await.unwrap();

        let report = crate::session::launch::recover(crashed.path(), crate::session::launch::Retention::KeepAll, Local::now()).unwrap();
        let len = report.repaired[0].1;
        assert_eq!(report.untranscribed, vec![Gap::new(a, 0, Some(len), GapKind::SttInterrupted)], "the crashed audio waits for recovery");
    }

    /// After a rate change the next recording begins while the worker is still flushing the last one.
    #[tokio::test]
    async fn a_crash_while_two_recordings_await_their_transcript_leaves_a_gap_for_each() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let stt = scripted_stt(move |i| match i {
            SttInput::End { recording_id, .. } if recording_id == b => {
                vec![SttEvent::Ended { recording_id: a, gap: None }, SttEvent::Ended { recording_id: b, gap: None }]
            }
            _ => vec![],
        });
        let go = Arc::new(AtomicBool::new(false));
        let mut before = recording(a, 20);
        before.push(begin(b, 2));
        before.extend((0..20).map(|k| frame(b, k * 1600)));
        let after = vec![SourceEvent::End { recording_id: b, samples: 32_000, stream_errors: 0 }];
        let (handle, _notes) = spawn(SessionConfig { stt: Some(stt), ..cfg(dir.path()) }, Box::new(Gated { before, go: go.clone(), after }));
        written(dir.path(), b, 16_000).await;
        for _ in 0..5_000 {
            let sc = Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap();
            if sc.recordings.iter().any(|r| r.id == a && r.state == RecState::Finalized) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        let crashed = tempfile::tempdir().unwrap();
        crash_copy(dir.path(), crashed.path());
        go.store(true, Ordering::Relaxed);
        handle.finish().await.unwrap();

        let report = crate::session::launch::recover(crashed.path(), crate::session::launch::Retention::KeepAll, Local::now()).unwrap();
        let len_b = report.repaired[0].1;
        assert_eq!(
            report.untranscribed,
            vec![Gap::new(a, 0, Some(32_000), GapKind::SttInterrupted), Gap::new(b, 0, Some(len_b), GapKind::SttInterrupted)],
            "both recordings' audio waits for recovery"
        );
    }

    #[tokio::test]
    async fn streamed_and_recovered_audio_are_written_to_the_ledger() {
        use crate::session::spend::{self, Spend};
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("spend.jsonl");
        let spend = Spend::open(&ledger, "Machine Learning", "Week 01", chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()).unwrap();
        let a = Uuid::new_v4();
        let stt = scripted_stt(move |i| match i {
            SttInput::End { samples, .. } => vec![
                SttEvent::Streamed { recording_id: a, samples: 16_000 },
                SttEvent::Ended { recording_id: a, gap: Some(Gap::new(a, 8_000, Some(samples), GapKind::SttOffline)) },
            ],
            _ => vec![],
        });
        let recovery = scripted_recovery(|j| {
            vec![
                RecoverEvent::Piece { recording_id: j.recording_id, gap_start: j.gap_start, start_sample: j.from, end_sample: j.end, text: "recovered".into(), words: vec![] },
                RecoverEvent::Done { recording_id: j.recording_id, gap_start: j.gap_start },
            ]
        });
        let cfg = SessionConfig { stt: Some(stt), recovery: Some(recovery), spend: Some(spend), ..cfg(dir.path()) };
        run_with(cfg, Script(recording(a, 10))).await.0.unwrap();
        let entries = spend::read(&ledger).unwrap();
        let got: Vec<(&str, f64, bool, Option<f64>)> = entries.iter().map(|e| (e.what.as_str(), e.usd, e.billed, e.audio_s)).collect();
        assert_eq!(got, vec![("transcribe", 0.000056, false, Some(1.0)), ("transcribe", 0.000014, false, Some(0.5))]);
        assert!(entries.iter().all(|e| e.course == "Machine Learning" && e.lecture == "Week 01"));
    }

    #[tokio::test]
    async fn a_store_update_runs_in_the_coordinator_and_later_saves_keep_it() {
        let dir = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4();
        let go = Arc::new(AtomicBool::new(false));
        let mut before = recording(a, 4);
        let after = before.split_off(3);
        let (handle, _notes, store) = spawn_with_store(cfg(dir.path()), Box::new(Gated { before, go: go.clone(), after }));
        while !Sidecar::load(&sidecar_path(dir.path(), STEM)).ok().flatten().is_some_and(|sc| sc.recordings.iter().any(|r| r.id == a)) {
            tokio::time::sleep(Duration::from_millis(1)).await; // the recording has begun
        }
        let seen = store
            .update(|sc| {
                sc.notes.segment_cursor = 7;
                Ok(sc.recordings.len())
            })
            .await
            .unwrap();
        assert_eq!(seen, 1, "the update sees the coordinator's sidecar");
        assert_eq!(Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap().notes.segment_cursor, 7);
        go.store(true, Ordering::Relaxed);
        drop(store);
        handle.finish().await.unwrap();
        let sc = Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap();
        assert_eq!((sc.notes.segment_cursor, sc.recordings[0].state), (7, RecState::Finalized), "the coordinator's own saves keep it");
    }

    #[tokio::test]
    async fn the_session_serves_its_stores_until_they_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4();
        let (handle, _notes, store) = spawn_with_store(cfg(dir.path()), Box::new(Script(recording(a, 3))));
        let finished = tokio::spawn(handle.finish());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!finished.is_finished(), "a store still held keeps the session open");
        store
            .update(|sc| {
                sc.notes.slide_index = 4;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(store.cutoff().await.unwrap(), Some(CutoffResult { confirmed: false, segments: 0 }), "after the audio nothing more is confirmed");
        drop(store);
        finished.await.unwrap().unwrap();
        let path = sidecar_path(dir.path(), STEM);
        assert_eq!(Sidecar::load(&path).unwrap().unwrap().notes.slide_index, 4);
        let offline = Store::offline(Sidecar::load(&path).unwrap().unwrap(), path.clone());
        offline
            .update(|sc| {
                sc.notes.slide_index = 5;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(offline.cutoff().await.unwrap(), None);
        assert_eq!(Sidecar::load(&path).unwrap().unwrap().notes.slide_index, 5);
    }

    #[tokio::test]
    async fn a_cutoff_while_a_recordings_transcript_is_still_flushing_is_not_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let (input, mut rx) = mpsc::channel(INPUT_QUEUE);
        let (tx, events) = mpsc::channel(8);
        let (flushed_tx, flushed_rx) = oneshot::channel::<()>();
        let ended = Arc::new(AtomicBool::new(false));
        let seen_end = ended.clone();
        tokio::spawn(async move {
            let mut flushed = Some(flushed_rx);
            while let Some(i) = rx.recv().await {
                if let SttInput::End { recording_id, .. } = i {
                    seen_end.store(true, Ordering::SeqCst);
                    if let Some(f) = flushed.take() {
                        let _ = f.await; // still flushing this recording's last sentence
                    }
                    let _ = tx.send(SttEvent::Ended { recording_id, gap: None }).await;
                }
            }
        });
        let a = Uuid::new_v4();
        let go = Arc::new(AtomicBool::new(false));
        let (handle, _notes) = spawn(SessionConfig { stt: Some(SttLink { input, events }), ..cfg(dir.path()) }, Box::new(Gated { before: recording(a, 3), go: go.clone(), after: vec![] }));
        while !ended.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(!handle.cutoff().await.unwrap().confirmed, "the last sentence may still arrive");
        flushed_tx.send(()).unwrap();
        while !Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap().open_utterances.is_empty() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(handle.cutoff().await.unwrap().confirmed, "between recordings with nothing flushing, all is settled");
        go.store(true, Ordering::Relaxed);
        handle.finish().await.unwrap();
    }

    #[tokio::test]
    async fn a_second_stop_abandons_pending_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let old = Uuid::new_v4();
        let anchor = Local.with_ymd_and_hms(2026, 9, 25, 9, 0, 0).unwrap();
        let mut sc = Sidecar::default();
        sc.recordings.push(RecordingEntry { id: old, file: "recordings/old.wav".into(), anchor, source_uid: "X".into(), input_rate: 48_000, samples: Some(16_000), state: RecState::Finalized });
        sc.gaps.push(Gap::new(old, 0, Some(16_000), GapKind::SttOffline));
        sc.save(&sidecar_path(dir.path(), STEM)).unwrap();
        let (jobs, mut jobs_rx) = mpsc::unbounded_channel::<RecoverJob>();
        let (events_tx, events) = mpsc::channel::<RecoverEvent>(8);
        tokio::spawn(async move {
            let _hold = events_tx;
            while jobs_rx.recv().await.is_some() {
                std::future::pending::<()>().await; // a long gap that never finishes
            }
        });
        let (handle, _notes) = spawn(SessionConfig { recovery: Some(RecoveryLink { jobs, events }), ..cfg(dir.path()) }, Box::new(Endless));
        tokio::time::sleep(Duration::from_millis(30)).await;
        handle.request_stop();
        loop {
            let sc = Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap();
            if sc.recordings.len() == 2 && sc.recordings.iter().all(|r| r.state == RecState::Finalized) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        handle.request_stop();
        let report = tokio::time::timeout(Duration::from_secs(5), handle.finish()).await.expect("the second stop ends the wait").unwrap();
        assert_eq!(report.unresolved, 1, "the gap waits for the next session");
        assert_eq!(report.recordings.len(), 1, "this session's recording is intact");
    }
}
