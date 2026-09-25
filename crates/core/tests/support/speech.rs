//! Synthetic speech for the fakes. Every sample of frame k holds k + 1, so any stretch of the
//! audio (a live frame, or a clip read back from the recording) tells where it sits, and words sit
//! at fixed positions, so a fake knows which words a stretch contains.
use serde_json::{json, Value};

pub const FRAME: u64 = 1600;
const WORD_FRAMES: u64 = 4;
const WORD_STRIDE: u64 = 5;
const UTTERANCE_WORDS: u64 = 8;
const UTTERANCE_FRAMES: u64 = 45;
const ENDPOINT_FRAMES: u64 = 2;

pub fn frame_pcm(k: u64) -> [i16; 1600] {
    [(k + 1) as i16; 1600]
}

pub fn word_start(w: u64) -> u64 {
    let (u, j) = (w / UTTERANCE_WORDS, w % UTTERANCE_WORDS);
    (u * UTTERANCE_FRAMES + j * WORD_STRIDE) * FRAME
}

pub fn word_end(w: u64) -> u64 {
    word_start(w) + WORD_FRAMES * FRAME
}

pub fn word_text(w: u64) -> String {
    format!("w{w}")
}

/// Where the server closes utterance `u` by itself: 0.2 s after its last word.
pub fn endpoint(u: u64) -> u64 {
    word_end(u * UTTERANCE_WORDS + UTTERANCE_WORDS - 1) + ENDPOINT_FRAMES * FRAME
}

/// Words whose first sample lies in [from, to).
pub fn words_starting(from: u64, to: u64) -> Vec<u64> {
    let mut w = from / FRAME / UTTERANCE_FRAMES * UTTERANCE_WORDS;
    let mut out = Vec::new();
    while word_start(w) < to {
        if word_start(w) >= from {
            out.push(w);
        }
        w += 1;
    }
    out
}

/// Every word of a recording `len` samples long, in order.
pub fn expected_words(len: u64) -> Vec<String> {
    words_starting(0, len).into_iter().map(word_text).collect()
}

pub fn text(ws: &[u64]) -> String {
    ws.iter().map(|&w| word_text(w)).collect::<Vec<_>>().join(" ")
}

/// Where a clip of synthetic audio starts, from the first frame boundary inside it.
pub fn clip_start(pcm: &[i16]) -> u64 {
    for i in 1..pcm.len() {
        if pcm[i] != pcm[i - 1] && pcm[i] > 0 && pcm[i - 1] > 0 {
            return (pcm[i] as u64 - 1) * FRAME - i as u64;
        }
    }
    (pcm[0] as u64 - 1) * FRAME
}

fn secs(samples: u64) -> f64 {
    samples as f64 / 16_000.0
}

/// The REST answer for a clip: the words that start in it, times from the clip's start; no
/// `words` key for silence, as the real endpoint.
pub fn transcribe_clip(pcm: &[i16]) -> Value {
    let start = clip_start(pcm);
    let end = start + pcm.len() as u64;
    let ws = words_starting(start, end);
    let rel = |x: u64| secs(x - start);
    let mut v = json!({"text": text(&ws), "language": if ws.is_empty() { "" } else { "en" }, "duration": rel(end)});
    if !ws.is_empty() {
        v["words"] = ws.iter().map(|&w| json!({"text": word_text(w), "start": rel(word_start(w)), "end": rel(word_end(w).min(end))})).collect();
    }
    v
}

/// One connection's hearing of the synthetic speech: what it has heard since its first frame.
#[derive(Default)]
pub struct Listener {
    origin: Option<u64>,
    pub heard_to: u64,
    open_from: u64,
    frames: u64,
}

impl Listener {
    fn rel(&self, at: u64) -> f64 {
        secs(at - self.origin.unwrap_or(at))
    }

    pub fn heard_secs(&self) -> f64 {
        self.rel(self.heard_to)
    }

    /// A frame at `at` arrived: a final pair at every endpoint it passed, an interim every tenth frame.
    pub fn frame(&mut self, at: u64, len: u64) -> Vec<Value> {
        if self.origin.is_none() {
            self.origin = Some(at);
            self.open_from = at;
        }
        self.heard_to = at + len;
        self.frames += 1;
        let mut out = Vec::new();
        let mut u = self.open_from / FRAME / UTTERANCE_FRAMES;
        while endpoint(u) <= self.heard_to {
            if endpoint(u) > self.open_from {
                out.extend(self.close(endpoint(u), false));
            }
            u += 1;
        }
        if self.frames % 10 == 0 {
            let ws = words_starting(self.open_from, self.heard_to);
            if let Some(&w0) = ws.first() {
                let s = self.rel(word_start(w0));
                out.push(json!({"type": "transcript.partial", "is_final": false, "speech_final": false, "start": s, "duration": self.rel(self.heard_to) - s, "text": text(&ws), "words": []}));
            }
        }
        out
    }

    /// Closes the open utterance at `at`: a chunk-final, then a speech_final running on to `at`.
    /// With nothing open, a finalize (`forced`) still gets an empty speech_final at `at`.
    pub fn close(&mut self, at: u64, forced: bool) -> Vec<Value> {
        let ws = words_starting(self.open_from, at);
        self.open_from = if ws.is_empty() && !forced { self.open_from } else { at };
        if ws.is_empty() {
            return if forced {
                vec![json!({"type": "transcript.partial", "is_final": true, "speech_final": true, "start": self.rel(at), "duration": 0.0, "text": "", "words": []})]
            } else {
                vec![]
            };
        }
        let first = self.rel(word_start(ws[0]));
        let last_end = self.rel(word_end(*ws.last().unwrap()).min(at));
        let words: Vec<Value> = ws.iter().map(|&w| json!({"text": word_text(w), "start": self.rel(word_start(w)), "end": self.rel(word_end(w).min(at))})).collect();
        let msg = |speech_final: bool, end: f64| {
            json!({"type": "transcript.partial", "is_final": true, "speech_final": speech_final, "start": first, "duration": end - first, "text": text(&ws), "words": words})
        };
        vec![msg(false, last_end), msg(true, self.rel(at))]
    }
}
