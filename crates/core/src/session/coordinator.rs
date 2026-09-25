//! The session coordinator (spec §3.2): one task owns the sidecar and the recorder's
//! instructions; the source and the recorder are workers behind bounded channels.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::audio::frame::{Frame, FRAME_SAMPLES};
use crate::audio::recorder::Recorder;
use crate::audio::source::{Source, SourceEvent};
use crate::session::sidecar::{sidecar_path, Gap, GapKind, RecState, RecordingEntry, Sidecar};

/// Frames the recorder may fall behind before frames become a recorder gap (5 s).
const FRAME_QUEUE: usize = 50;

pub struct SessionConfig {
    pub dir: PathBuf,
    pub stem: String,
}

#[derive(Debug)]
pub enum Notification {
    Recording { path: PathBuf },
    Level(f32),
    Gap(Gap),
    DeviceGone { uid: String },
    DeviceBack { uid: String },
    Failed(String),
}

#[derive(Debug, Default)]
pub struct StopReport {
    pub recordings: Vec<(PathBuf, u64)>,
    pub gaps: usize,
    pub stream_errors: u64,
}

pub struct SessionHandle {
    stop: mpsc::Sender<()>,
    task: JoinHandle<Result<StopReport>>,
}

impl SessionHandle {
    pub fn request_stop(&self) {
        let _ = self.stop.try_send(());
    }

    /// Waits for the session to end (after `request_stop`, or when the source ends).
    pub async fn finish(self) -> Result<StopReport> {
        self.task.await?
    }
}

pub fn spawn(cfg: SessionConfig, source: Box<dyn Source>) -> (SessionHandle, mpsc::Receiver<Notification>) {
    let (stop_tx, stop_rx) = mpsc::channel(1);
    let (notify_tx, notify_rx) = mpsc::channel(256);
    let task = tokio::spawn(run(cfg, source, stop_rx, notify_tx));
    (SessionHandle { stop: stop_tx, task }, notify_rx)
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
                self.save().await?;
                self.recorder()?.send(RecMsg::Open { recording_id, recorder }).await.map_err(|_| anyhow!("recorder stopped"))?;
                self.notify(Notification::Recording { path });
            }
            SourceEvent::Frame(f) => {
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
                                let gap = Gap { recording_id: id, start_sample: start, end_sample: Some(end), kind: GapKind::RecorderOverflow, resolved: false };
                                self.sidecar.gaps.push(gap.clone());
                                self.overflow = Some(self.sidecar.gaps.len() - 1);
                                self.save().await?;
                                self.notify(Notification::Gap(gap));
                            }
                        }
                        self.dirty = true;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {} // the recorder's own failure event ends the session
                }
            }
            SourceEvent::Gap(g) => {
                self.sidecar.gaps.push(g.clone());
                self.save().await?;
                self.notify(Notification::Gap(g));
            }
            SourceEvent::End { recording_id, stream_errors, .. } => {
                self.report.stream_errors += stream_errors;
                self.overflow = None;
                self.recorder()?.send(RecMsg::Finish { recording_id }).await.map_err(|_| anyhow!("recorder stopped"))?;
            }
            SourceEvent::Level(l) => self.notify(Notification::Level(l)),
            SourceEvent::DeviceGone { uid } => self.notify(Notification::DeviceGone { uid }),
            SourceEvent::DeviceBack { uid } => self.notify(Notification::DeviceBack { uid }),
            SourceEvent::Failed(msg) => return Err(anyhow!(msg)),
        }
        Ok(())
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

async fn run(cfg: SessionConfig, source: Box<dyn Source>, mut stop_rx: mpsc::Receiver<()>, notify: mpsc::Sender<Notification>) -> Result<StopReport> {
    let sidecar_path = sidecar_path(&cfg.dir, &cfg.stem);
    let sidecar = Sidecar::load(&sidecar_path)?.unwrap_or_default();
    let stop = Arc::new(AtomicBool::new(false));
    let (src_tx, mut src_rx) = mpsc::channel(256);
    let source_thread = {
        let stop = stop.clone();
        std::thread::spawn(move || source.run(src_tx, stop))
    };
    let (rec_tx, rec_rx) = mpsc::channel(FRAME_QUEUE);
    let (rev_tx, mut rev_rx) = mpsc::channel(16);
    let recorder_thread = std::thread::spawn(move || recorder_worker(rec_rx, rev_tx));

    let mut c = Coordinator { dir: cfg.dir, sidecar_path, sidecar, dirty: false, rec_tx: Some(rec_tx), notify, overflow: None, report: StopReport::default() };
    let mut failure: Option<anyhow::Error> = None;
    let fail = |e: anyhow::Error, failure: &mut Option<anyhow::Error>| {
        stop.store(true, Ordering::Relaxed);
        failure.get_or_insert(e);
    };
    loop {
        tokio::select! {
            Some(()) = stop_rx.recv() => stop.store(true, Ordering::Relaxed),
            ev = src_rx.recv() => match ev {
                Some(ev) => if failure.is_none() {
                    if let Err(e) = c.on_source(ev).await { fail(e, &mut failure) }
                },
                None => break, // the source thread has exited
            },
            Some(ev) = rev_rx.recv() => if let Err(e) = c.on_recorder(ev).await { fail(e, &mut failure) },
        }
    }
    // Close the recorder queue, drain its events (each open recording reports Finished),
    // join both threads, then save.
    c.rec_tx = None;
    while let Some(ev) = rev_rx.recv().await {
        if let Err(e) = c.on_recorder(ev).await {
            failure.get_or_insert(e);
        }
    }
    tokio::task::spawn_blocking(move || {
        let _ = source_thread.join();
        let _ = recorder_thread.join();
    })
    .await?;
    if c.dirty {
        c.save().await?;
    }
    c.report.gaps = c.sidecar.gaps.len();
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
        SessionConfig { dir: dir.to_path_buf(), stem: "lecture_notes_20260925".into() }
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
}
