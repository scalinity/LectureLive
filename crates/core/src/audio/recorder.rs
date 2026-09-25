use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use hound::{SampleFormat, WavSpec, WavWriter};

use crate::audio::frame::{Frame, FRAME_SAMPLES};

pub const SAMPLE_RATE: u32 = 16_000;
const CHECKPOINT_SAMPLES: u32 = SAMPLE_RATE;
pub const HEADER_LEN: u64 = 44;

fn spec() -> WavSpec {
    WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: SampleFormat::Int }
}

pub struct Recorder {
    writer: WavWriter<BufWriter<File>>,
    sync_handle: File,
    path: PathBuf,
    since_checkpoint: u32,
    samples: u64,
}

impl Recorder {
    pub fn create(dir: &Path, stem: &str) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let mut n = 0;
        let (file, path) = loop {
            let name = if n == 0 { format!("{stem}.wav") } else { format!("{stem}_{n}.wav") };
            let path = dir.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(f) => break (f, path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e).with_context(|| format!("create {}", path.display())),
            }
        };
        let sync_handle = file.try_clone()?;
        let writer = WavWriter::new(BufWriter::new(file), spec())?;
        let mut r = Self { writer, sync_handle, path, since_checkpoint: 0, samples: 0 };
        r.checkpoint()?; // the header reaches disk before any audio, so a crash leaves a repairable file
        Ok(r)
    }

    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Writes a frame at its own offset; missing offsets before it become silence, so the
    /// file's timeline stays linear in wall-clock time (the gap itself is recorded by the caller).
    pub fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        ensure!(
            frame.sample_offset >= self.samples,
            "frame at sample {} overlaps audio already written up to {}",
            frame.sample_offset,
            self.samples
        );
        const SILENCE: [i16; FRAME_SAMPLES] = [0; FRAME_SAMPLES];
        let mut pad = frame.sample_offset - self.samples;
        while pad > 0 {
            let n = pad.min(FRAME_SAMPLES as u64) as usize;
            self.write(&SILENCE[..n])?;
            pad -= n as u64;
        }
        self.write(frame.pcm())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&mut self, pcm: &[i16]) -> Result<()> {
        for &s in pcm {
            self.writer.write_sample(s)?;
        }
        self.since_checkpoint += pcm.len() as u32;
        self.samples += pcm.len() as u64;
        if self.since_checkpoint >= CHECKPOINT_SAMPLES {
            self.checkpoint()?;
        }
        Ok(())
    }

    /// Flushes buffered samples, rewrites the header sizes, and syncs to disk.
    pub fn checkpoint(&mut self) -> Result<()> {
        self.writer.flush()?;
        self.sync_handle.sync_data()?;
        self.since_checkpoint = 0;
        Ok(())
    }

    pub fn finalize(self) -> Result<PathBuf> {
        let path = self.path.clone();
        self.writer.finalize()?;
        self.sync_handle.sync_all()?;
        Ok(path)
    }
}

/// Rewrites the RIFF and data sizes from the file length; returns the sample count.
pub fn repair_header(path: &Path) -> Result<u32> {
    let mut f = OpenOptions::new().read(true).write(true).open(path)?;
    let len = f.metadata()?.len();
    ensure!(len >= HEADER_LEN, "{} is shorter than a WAV header", path.display());
    let data_len = ((len - HEADER_LEN) / 2 * 2) as u32;
    f.set_len(HEADER_LEN + data_len as u64)?;
    f.seek(SeekFrom::Start(4))?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.seek(SeekFrom::Start(40))?;
    f.write_all(&data_len.to_le_bytes())?;
    f.sync_all()?;
    Ok(data_len / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(n: usize) -> Vec<i16> {
        (0..n).map(|i| ((i % 100) as i16) * 100).collect()
    }

    #[test]
    fn header_is_canonical_44_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), "session").unwrap();
        r.write(&frames(1600)).unwrap();
        let path = r.finalize().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(bytes.len(), 44 + 1600 * 2);
    }

    #[test]
    fn same_stem_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let a = Recorder::create(dir.path(), "session").unwrap();
        let b = Recorder::create(dir.path(), "session").unwrap();
        assert_ne!(a.path(), b.path());
    }

    #[test]
    fn checkpointed_audio_survives_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), "session").unwrap();
        let path = r.path().to_path_buf();
        for _ in 0..25 {
            r.write(&frames(1600)).unwrap(); // 25 × 100 ms = 2.5 s, checkpoints at 1 s and 2 s
        }
        std::mem::forget(r); // simulate a crash: no finalize, no drop

        let readable = hound::WavReader::open(&path).unwrap().len();
        assert!(readable >= 32_000, "readable samples {readable}");

        let repaired = repair_header(&path).unwrap();
        assert!(repaired >= 32_000);
        assert_eq!(hound::WavReader::open(&path).unwrap().len(), repaired);
    }

    fn frame(offset: u64, value: i16) -> crate::audio::frame::Frame {
        crate::audio::frame::Frame { recording_id: uuid::Uuid::nil(), sample_offset: offset, valid_samples: 1600, pcm16: [value; 1600] }
    }

    #[test]
    fn header_is_on_disk_before_any_audio() {
        let dir = tempfile::tempdir().unwrap();
        let r = Recorder::create(dir.path(), "session").unwrap();
        let path = r.path().to_path_buf();
        std::mem::forget(r); // crash straight after creation
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 44);
        assert_eq!(repair_header(&path).unwrap(), 0);
        assert_eq!(hound::WavReader::open(&path).unwrap().len(), 0);
    }

    #[test]
    fn write_frame_fills_skipped_offsets_with_silence() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), "session").unwrap();
        r.write_frame(&frame(0, 100)).unwrap();
        r.write_frame(&frame(3200, 300)).unwrap(); // 1600..3200 never arrived
        assert_eq!(r.samples(), 4800);
        let path = r.finalize().unwrap();
        let s: Vec<i16> = hound::WavReader::open(&path).unwrap().samples::<i16>().map(|x| x.unwrap()).collect();
        assert_eq!(s.len(), 4800);
        assert_eq!((s[0], s[1600], s[3199], s[3200]), (100, 0, 0, 300));
    }

    #[test]
    fn write_frame_rejects_overlap() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), "session").unwrap();
        r.write_frame(&frame(0, 1)).unwrap();
        assert!(r.write_frame(&frame(800, 1)).is_err());
    }

    #[test]
    fn repair_drops_a_trailing_odd_byte() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Recorder::create(dir.path(), "session").unwrap();
        r.write(&frames(1600)).unwrap();
        let path = r.finalize().unwrap();
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&[7]).unwrap();
        assert_eq!(repair_header(&path).unwrap(), 1600);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 44 + 3200);
    }
}
