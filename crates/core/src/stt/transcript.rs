//! The transcript of one STT connection (spec §5.2): closed utterances are committed; the open
//! utterance is a stable part (locked chunk-finals) and a tentative tail (the latest interim).
use serde::{Deserialize, Serialize};

use super::protocol::{to_samples, Partial, ServerWord};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start_sample: u64,
    pub end_sample: u64,
}

/// A closed utterance: its `speech_final`'s span of the recording, text and words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    pub start_sample: u64,
    pub end_sample: u64,
    pub text: String,
    pub words: Vec<Word>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Open { stable: String, tentative: String },
    Closed(Utterance),
}

#[derive(Debug, Clone)]
struct Chunk {
    start: u64,
    end: u64,
    text: String,
    words: Vec<Word>,
}

pub struct Transcript {
    origin: u64,
    closed_through: u64,
    stable: Vec<Chunk>,
    tentative: String,
}

impl Transcript {
    /// `origin` is the recording sample of the first frame sent on the connection: server time 0.
    pub fn new(origin: u64) -> Self {
        Self { origin, closed_through: origin, stable: Vec::new(), tentative: String::new() }
    }

    /// End of the last closed utterance, or the origin: the transcript is settled up to here.
    pub fn closed_through(&self) -> u64 {
        self.closed_through
    }

    fn at(&self, secs: f64) -> u64 {
        self.origin + to_samples(secs)
    }

    fn words(&self, words: &[ServerWord]) -> Vec<Word> {
        words.iter().map(|w| Word { text: w.text.clone(), start_sample: self.at(w.start), end_sample: self.at(w.end) }).collect()
    }

    fn stable_text(&self) -> String {
        join(self.stable.iter().map(|c| c.text.as_str()))
    }

    pub fn apply(&mut self, p: &Partial) -> Option<Update> {
        let end = self.at(p.start + p.duration);
        if end <= self.closed_through {
            return None; // about audio already closed: a repeat
        }
        let before = (self.stable_text(), self.tentative.clone());
        if p.is_final {
            // A final's range replaces the stable chunks it overlaps.
            let start = self.at(p.start).max(self.closed_through);
            let words = self.words(&p.words);
            let text = p.text.trim().to_string();
            self.stable.retain(|c| c.end <= start);
            if !text.is_empty() || !words.is_empty() {
                self.stable.push(Chunk { start, end, text, words });
            }
            self.tentative.clear();
            if p.speech_final {
                let chunks = std::mem::take(&mut self.stable);
                self.closed_through = end;
                return Some(Update::Closed(Utterance {
                    start_sample: chunks.first().map_or(start, |c| c.start),
                    end_sample: end,
                    text: join(chunks.iter().map(|c| c.text.as_str())),
                    words: chunks.into_iter().flat_map(|c| c.words).collect(),
                }));
            }
        } else {
            // An interim is the whole open utterance so far: its tail past the stable text is tentative.
            let text = p.text.trim();
            let stable = before.0.as_str();
            self.tentative = match text.strip_prefix(stable) {
                Some(rest) if !stable.is_empty() => rest.trim_start().to_string(),
                _ => text.to_string(),
            };
        }
        let after = (self.stable_text(), self.tentative.clone());
        (after != before).then(|| Update::Open { stable: after.0, tentative: after.1 })
    }
}

fn join<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    parts.filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::protocol::{parse, Partial, ServerMsg, ServerWord};

    /// Replays a recorded session's server messages in arrival order (ties keep the log's order).
    fn replay(fixture: &str, origin: u64) -> (Vec<Utterance>, Vec<Update>) {
        let mut msgs: Vec<(u64, String)> = fixture
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|v| v["dir"] == "in")
            .map(|v| (v["t_ms"].as_u64().unwrap(), v["msg"].to_string()))
            .collect();
        msgs.sort_by_key(|m| m.0);
        let mut t = Transcript::new(origin);
        let mut updates = Vec::new();
        for (_, m) in msgs {
            if let ServerMsg::Partial(p) = parse(&m).unwrap() {
                updates.extend(t.apply(&p));
            }
        }
        let closed = updates
            .iter()
            .filter_map(|u| match u {
                Update::Closed(u) => Some(u.clone()),
                _ => None,
            })
            .collect();
        (closed, updates)
    }

    fn spans(u: &[Utterance]) -> Vec<(u64, u64, &str)> {
        u.iter().map(|u| (u.start_sample, u.end_sample, u.text.as_str())).collect()
    }

    fn word(text: &str, start: u64, end: u64) -> Word {
        Word { text: text.into(), start_sample: start, end_sample: end }
    }

    fn partial(is_final: bool, speech_final: bool, start: f64, duration: f64, text: &str) -> Partial {
        Partial { text: text.into(), words: vec![], is_final, speech_final, start, duration }
    }

    #[test]
    fn json_finalize_fixture_closes_two_exact_utterances() {
        let (u, _) = replay(include_str!("../../tests/fixtures/stt/finalize_json.jsonl"), 0);
        assert_eq!(
            spans(&u),
            vec![
                (16, 48_000, "Transfer learning reuses pre-trained weights. The"),
                (48_000, 137_840, "losses cross entropy over the vocabulary, gradient descent updates the weights after every batch."),
            ]
        );
        assert_eq!(
            u[0].words,
            vec![
                word("Transfer", 336, 7_744),
                word("learning", 8_704, 13_216),
                word("reuses", 13_856, 21_264),
                word("pre-trained", 21_904, 30_608),
                word("weights.", 31_568, 38_016),
                word("The", 42_848, 44_128),
            ]
        );
        assert_eq!(u[1].words.len(), 14);
        assert_eq!(u[1].words[0], word("losses", 48_000, 50_560));
        assert_eq!(u[1].words[13], word("batch.", 127_888, 133_984));
    }

    #[test]
    fn bare_text_finalize_fixture_closes_one_utterance_at_audio_done() {
        let (u, _) = replay(include_str!("../../tests/fixtures/stt/finalize_text.jsonl"), 0);
        assert_eq!(
            spans(&u),
            vec![(16, 137_840, "Transfer learning reuses pre-trained weights; the losses cross entropy over the vocabulary, gradient descent updates the weights after every batch.")]
        );
        assert_eq!(u[0].words.len(), 20);
    }

    #[test]
    fn natural_endpoints_close_each_sentence_once_through_its_endpoint_silence() {
        let (u, updates) = replay(include_str!("../../tests/fixtures/stt/endpoint_pauses.jsonl"), 0);
        assert_eq!(
            spans(&u),
            vec![
                (16, 49_280, "Gradient descent updates the weights."),
                (59_520, 117_760, "The learning rate controls the step size."),
                (128_640, 167_616, "Momentum smooths the updates."),
            ]
        );
        assert_eq!(u[1].words.first(), Some(&word("The", 66_288, 67_568)));
        // Between the chunk-final and its speech_final the sentence is stable but not closed.
        assert!(updates.contains(&Update::Open { stable: "Gradient descent updates the weights.".into(), tentative: String::new() }));
    }

    #[test]
    fn a_finalize_between_chunk_final_and_speech_final_closes_at_the_cutoff() {
        let (u, _) = replay(include_str!("../../tests/fixtures/stt/finalize_between_pair.jsonl"), 0);
        assert_eq!(u.len(), 3);
        assert_eq!(spans(&u)[0], (16, 44_800, "Gradient descent updates the weights."));
    }

    #[test]
    fn a_finalize_in_silence_closes_an_empty_utterance_at_the_cutoff() {
        let (u, _) = replay(include_str!("../../tests/fixtures/stt/finalize_silence.jsonl"), 0);
        assert_eq!(u, vec![Utterance { start_sample: 24_000, end_sample: 24_000, text: String::new(), words: vec![] }]);
    }

    #[test]
    fn empty_chunk_finals_in_silence_close_and_change_nothing() {
        let (u, updates) = replay(include_str!("../../tests/fixtures/stt/silence_after_speech.jsonl"), 0);
        assert_eq!(spans(&u), vec![(16, 39_360, "The gradient points uphill.")]);
        assert!(matches!(updates.last(), Some(Update::Closed(_))), "nothing after the close changes the display: {updates:?}");
    }

    #[test]
    fn an_empty_hypothesis_never_erases_stable_words() {
        let mut t = Transcript::new(0);
        t.apply(&partial(true, false, 0.0, 1.0, "gradient descent"));
        assert_eq!(t.apply(&partial(false, false, 0.0, 1.5, "")), None);
        assert_eq!(
            t.apply(&partial(false, false, 0.0, 1.8, "gradient descent converges")),
            Some(Update::Open { stable: "gradient descent".into(), tentative: "converges".into() })
        );
        assert_eq!(
            t.apply(&partial(false, false, 0.0, 2.0, "")),
            Some(Update::Open { stable: "gradient descent".into(), tentative: String::new() })
        );
    }

    #[test]
    fn a_repeated_final_is_ignored() {
        let mut t = Transcript::new(0);
        let fin = partial(true, true, 0.5, 1.0, "momentum");
        assert!(matches!(t.apply(&fin), Some(Update::Closed(_))));
        assert_eq!(t.apply(&fin), None);
        assert_eq!(t.closed_through(), 24_000);
    }

    #[test]
    fn server_times_count_from_the_connection_origin() {
        let mut t = Transcript::new(160_000);
        let fin = Partial {
            text: "adam".into(),
            words: vec![ServerWord { text: "adam".into(), start: 0.25, end: 0.5 }],
            is_final: true,
            speech_final: true,
            start: 0.2,
            duration: 0.4,
        };
        let Some(Update::Closed(u)) = t.apply(&fin) else { panic!("a speech_final closes") };
        assert_eq!((u.start_sample, u.end_sample), (163_200, 169_600));
        assert_eq!(u.words, vec![word("adam", 164_000, 168_000)]);
    }
}
