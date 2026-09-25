//! The Grok speech-to-text wire format as recorded at M0 and in M2's plan research (spec §5.1).
use serde::Deserialize;

use crate::audio::recorder::SAMPLE_RATE;

/// Control messages are JSON text; bare text is answered with an `error` and ignored.
pub const FINALIZE: &str = r#"{"type":"finalize"}"#;
pub const AUDIO_DONE: &str = r#"{"type":"audio.done"}"#;

/// Server times have millisecond resolution: the tolerance when they meet frame positions.
pub const SERVER_RESOLUTION: u64 = SAMPLE_RATE as u64 / 1000;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMsg {
    #[serde(rename = "transcript.created")]
    Created,
    #[serde(rename = "transcript.partial")]
    Partial(Partial),
    /// Its text is empty: the client owns the transcript.
    #[serde(rename = "transcript.done")]
    Done {
        #[serde(default)]
        duration: f64,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        message: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Partial {
    #[serde(default)]
    pub text: String,
    /// Present only on finals.
    #[serde(default)]
    pub words: Vec<ServerWord>,
    #[serde(default)]
    pub is_final: bool,
    #[serde(default)]
    pub speech_final: bool,
    /// Seconds of audio from the first frame sent on the connection.
    #[serde(default)]
    pub start: f64,
    #[serde(default)]
    pub duration: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ServerWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

pub fn parse(text: &str) -> serde_json::Result<ServerMsg> {
    serde_json::from_str(text)
}

pub fn to_samples(secs: f64) -> u64 {
    (secs.max(0.0) * SAMPLE_RATE as f64).round() as u64
}

/// A 4xx refuses the request itself (key, parameter, model): retrying cannot help. 408 and 429 pass with time.
pub fn is_refusal(status: u16) -> bool {
    (400..500).contains(&status) && status != 408 && status != 429
}

/// The `error` field of a JSON refusal, or the body as sent (plain text for a bad parameter).
/// The server sends refusals chunked, and the websocket handshake hands the body over raw.
pub fn refusal_message(body: &str) -> String {
    let body = dechunk(body).unwrap_or_else(|| body.to_string());
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.trim().to_string())
}

/// The payload of a chunked body, or None when `body` is not chunked. A body cut short keeps what arrived.
fn dechunk(body: &str) -> Option<String> {
    let mut rest = body;
    let mut out = String::new();
    loop {
        let (size, after) = rest.split_once("\r\n")?;
        let size = usize::from_str_radix(size.trim(), 16).ok()?;
        if size == 0 {
            return Some(out);
        }
        let chunk = after.get(..size).unwrap_or(after);
        out.push_str(chunk);
        match after.get(size..).and_then(|r| r.strip_prefix("\r\n")) {
            Some(next) => rest = next,
            None => return Some(out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: [&str; 6] = [
        include_str!("../../tests/fixtures/stt/finalize_json.jsonl"),
        include_str!("../../tests/fixtures/stt/finalize_text.jsonl"),
        include_str!("../../tests/fixtures/stt/endpoint_pauses.jsonl"),
        include_str!("../../tests/fixtures/stt/finalize_silence.jsonl"),
        include_str!("../../tests/fixtures/stt/finalize_between_pair.jsonl"),
        include_str!("../../tests/fixtures/stt/silence_after_speech.jsonl"),
    ];

    /// The server's messages of a recorded session, in the order the log lists them.
    fn server_messages(fixture: &str) -> Vec<ServerMsg> {
        fixture
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|v| v["dir"] == "in")
            .map(|v| parse(&v["msg"].to_string()).unwrap_or_else(|e| panic!("{e}: {}", v["msg"])))
            .collect()
    }

    #[test]
    fn every_recorded_server_message_parses() {
        for f in FIXTURES {
            let msgs = server_messages(f);
            assert_eq!(msgs.iter().filter(|m| matches!(m, ServerMsg::Created)).count(), 1);
            assert_eq!(msgs.iter().filter(|m| matches!(m, ServerMsg::Done { .. })).count(), 1);
            assert!(!msgs.iter().any(|m| matches!(m, ServerMsg::Other)));
        }
        assert!(server_messages(FIXTURES[1]).contains(&ServerMsg::Error { message: "Invalid message: expected ident at line 1 column 2".into() }));
        assert!(server_messages(FIXTURES[0]).contains(&ServerMsg::Done { duration: 8.615 }));
    }

    #[test]
    fn words_come_only_on_finals() {
        for f in FIXTURES {
            for m in server_messages(f) {
                if let ServerMsg::Partial(p) = m {
                    assert!(p.is_final || p.words.is_empty(), "{p:?}");
                }
            }
        }
        let finals: Vec<Partial> = server_messages(FIXTURES[0])
            .into_iter()
            .filter_map(|m| match m {
                ServerMsg::Partial(p) if p.speech_final => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(finals.len(), 2);
        assert_eq!(finals[0].words[0], ServerWord { text: "Transfer".into(), start: 0.021, end: 0.484 });
    }

    #[test]
    fn seconds_map_to_samples_at_millisecond_resolution() {
        assert_eq!(to_samples(0.001), 16);
        assert_eq!(to_samples(0.001 + 2.999), 48_000);
        assert_eq!(to_samples(8.04 + 2.436), 167_616);
        assert_eq!(to_samples(-0.5), 0);
        assert_eq!(SERVER_RESOLUTION, 16);
    }

    #[test]
    fn refusals_are_4xx_except_timeout_and_rate_limit() {
        assert!(is_refusal(400) && is_refusal(401) && is_refusal(404));
        assert!(!is_refusal(408) && !is_refusal(429) && !is_refusal(503) && !is_refusal(200));
        assert_eq!(
            refusal_message(r#"{"code":"Client specified an invalid argument","error":"Incorrect API key provided."}"#),
            "Incorrect API key provided."
        );
        assert_eq!(
            refusal_message("Failed to deserialize query string: sample_rate: invalid digit found in string\n"),
            "Failed to deserialize query string: sample_rate: invalid digit found in string"
        );
    }

    /// The live server sends refusal bodies chunked, and the websocket handshake hands them over raw.
    #[test]
    fn a_chunked_refusal_body_is_read_through_its_framing() {
        let json = r#"{"code":"Client specified an invalid argument","error":"Incorrect API key provided. You can obtain an API key from https://console.x.ai."}"#;
        let chunked = format!("{:x}\r\n{json}\r\n0\r\n\r\n", json.len());
        assert!(chunked.starts_with("8a\r\n"), "the recorded framing");
        assert_eq!(refusal_message(&chunked), "Incorrect API key provided. You can obtain an API key from https://console.x.ai.");
        let text = "Failed to deserialize query string: sample_rate: invalid digit found in string";
        assert_eq!(refusal_message(&format!("{:x}\r\n{text}\r\n0\r\n\r\n", text.len())), text);
        assert_eq!(refusal_message(&format!("{:x}\r\n{text}", text.len())), text, "a tail cut short keeps what arrived");
    }

    #[test]
    fn unknown_message_types_are_kept_not_fatal() {
        assert_eq!(parse(r#"{"type":"transcript.speech_started","at":1.2}"#).unwrap(), ServerMsg::Other);
    }
}
