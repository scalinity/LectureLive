# M2 Streaming + Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stream every recording to Grok speech-to-text while it is recorded. Commit each closed utterance exactly once to a segment log and the transcript file, and finalize at snapshot cutoffs. After a disconnect, reconnect live-first and recover every interval the live stream missed from the recording through the REST endpoint. A 4xx stops STT, never the recording.

**Architecture:** An STT worker (one Tokio task per session) is the single ordered writer. It owns the websocket, the transcript state machine of the current connection, and all gap arithmetic. Each connection is an epoch whose server times count from the first frame sent on it, so a new recording always gets a new connection. The coordinator forwards frames to it through a bounded queue that never waits and keeps room for control messages. It commits the worker's closed utterances to the segment log and the transcript file, and records the worker's transcript gaps in the sidecar. It hands those gaps to a recovery worker, which re-transcribes the recorded interval through the REST endpoint in pieces of at most 30 s. The sidecar records the live epoch's origin, so after a crash launch repair turns the audio after the last logged segment into a gap.

**Tech Stack:** Rust 1.96.1, `tokio 1.53.1`, `tokio-tungstenite 0.30.0` / `tungstenite 0.30.0` (rustls with `ring`), `reqwest 0.13.5` (`rustls-no-provider`, `multipart`, `json`, no default features), `serde_json 1.0.151`, `hound 3.5.1`, `uuid 1.26.1`, `chrono 0.4.45`.

**Spec:** `docs/spec.md` §5 (all of it), §8 (segment log, transcript file), §3.2–3.5, §10 rows "Network down" and "STT 4xx". Gate: `docs/milestones.md` → M2. Evidence: the M0 and M1 Findings in `docs/milestones.md`, and the plan research below.

**Plan research (2026-09-25, before this plan was written).** `canary stt-probe` gained `--url` and `--pace-ms` (Task 1 commits them). Synthesised speech from `say -o … --data-format=LEI16@16000` was then streamed to the live endpoint, about 90 s in total. Four logs are kept as new fixtures in `crates/core/tests/fixtures/stt/`:

| Run | Result | Fixture |
|---|---|---|
| Speech with two 2 s pauses, no finalize | The server endpoints by itself. For each sentence a chunk-final (`is_final: true, speech_final: false`) comes about 0.5 s after speech stops, and a `speech_final` about 0.5 s after that. The two carry the same text and words, but the `speech_final`'s duration runs on through the endpoint silence (`[0.001, 2.48)`, then `[0.001, 3.08)`). An empty interim at the close's end follows. The next sentence's interims start where speech resumes (3.72), not where the last utterance ended (3.08). At `audio.done` the flushed pair arrives at one instant. | `endpoint_pauses.jsonl` |
| 3.1 s of silence, finalize at 1.5 s | A lone `speech_final` with empty text, `start: 1.5, duration: 0.0`, 40 ms after the finalize. No chunk-final comes first. | `finalize_silence.jsonl` |
| Finalize after the chunk-final, before its `speech_final` (2.8 s) | The `speech_final` ends exactly at the cutoff (`0.001 + 2.799`). There is no duplicate. | `finalize_between_pair.jsonl` |
| Speech, then 10 s of silence | After the close the server keeps talking: an empty interim every second and an empty chunk-final every two seconds, with no `speech_final`. The same holds for 12 s of pure silence. | `silence_after_speech.jsonl` |
| Finalize just after speech (2.2 s), finalize after a close (3.5 s) | A same-instant pair ending at 2.2; an empty lone `speech_final` at 3.5. | (logs only) |
| Bad key / unknown model / `sample_rate=abc` | Refused at the upgrade: HTTP 400 `{"code":"Client specified an invalid argument","error":"Incorrect API key provided. You can obtain an API key from https://console.x.ai."}` / 404 / 400 `Failed to deserialize query string: sample_rate: invalid digit found in string` (text/plain). | (logs only) |
| `&keyterm=backpropagation&keyterm=gradient%20descent` | Accepted; "Backpropagation computes gradients." | (log only) |
| 10.5 s sent unpaced (`--pace-ms 0`) | The same finals as paced, all within 660 ms: the server takes audio faster than real time. | (log only) |
| `&endpointing=400` | Accepted; timings identical to the default. It is left out of the URL. | (log only) |
| REST `POST https://api.x.ai/v1/stt`, multipart `file`, `language=en`, `format=true` | 200 `{"text":…,"language":"en","duration":10.476,"words":[{"text":"Gradient","start":0.0,"end":0.441},…]}`, word times from the clip's start. Silence: `{"text":"","language":"","duration":3.1}` with **no** `words` key. Bad key: 400 with the same JSON body as the websocket. No cost field. REST with `keyterm=backpropagation` wrote "That propagation", so the keyterm was not applied. | (logs only) |

Consequences that are design, not detail:
- A finalize is always answered by a `speech_final` whose end (`start + duration`) is the cutoff, even with nothing open. The cutoff is therefore confirmed by exact correlation, with no "no open utterance" special case (spec §5.3 is rewritten to say so).
- Utterances do not tile the audio. A live segment's interval is its `speech_final`'s span. The transcript is "settled" through the end of the last close.
- The server speaks at least every ~2 s while audio flows, so a 5 s idle watchdog detects a connection that died without closing (Wi-Fi gone).
- 4xx comes at the upgrade, so "refused" is decided in the handshake. 408 and 429 stay transient.
- `reqwest 0.13` renamed its TLS features. `rustls` pulls `aws-lc-rs`, which alongside the `ring` feature would leave rustls without a default provider. `rustls-no-provider` keeps ring alone, but reqwest then panics at `Client::build` unless a process-default provider is installed first (`reqwest-0.13.5/src/async_impl/client.rs:2503`). `RestClient::new` installs ring.

**What "resolved" means (decided here, because retention depends on it).** A gap is resolved when nothing more can be done for its interval. A transcript gap (`stt_*`) is resolved when recovery has committed its whole interval. An audio gap (capture or recorder overflow, device gone, rate change, interrupted) has no audio behind it for REST to read, so it is resolved when it is recorded. Retention (§4.4, unchanged code) then keeps a recording exactly while it holds a transcript not yet recovered. Task 3 applies this to the M1 creation sites. One M1 test assertion changes with it (`launch::tests::open_recording_is_repaired_and_its_tail_marked_interrupted`: the `interrupted` gap is now `resolved: true`).

**Execution:** executing-plans, inline, without approval stops. A fresh reviewer on the most capable model (`opus`) reviews the whole branch before the findings commit (Task 13). Ledger: `.superpowers/sdd/2026-09-25-m2-streaming-recovery/progress.md` (git-excluded through `.git/info/exclude`).

## Global Constraints

- macOS 13+, Apple Silicon; one user; no telemetry, accounts or servers.
- Toolchain pinned to `rustc 1.96.1` (`rust-toolchain.toml`); workspace `rust-version` 1.89.
- Builds are authorised: `cargo build`, `test`, `run`, `check`, `tree`, `add`. No `npm` or `tauri` build. Never run `cargo clean`. Do not touch `apps/desktop` or `target/release/bundle`: the packaged "LectureLive Canary.app" holds this Mac's Microphone and Screen Recording grants, tied to its ad-hoc signature. The canary app uses only `audio::{input, routing}` and `capture::window`, which M2 does not change.
- Live STT calls use synthesised speech only (`say`, or `crates/core/tests/fixtures/stt/speech.wav`), a few minutes in total ($0.20 per hour streamed, $0.10 per hour batch). Every gate test runs in `cargo test` against a fake server, without network. Live tests are `#[ignore]` and read `GROK_API_KEY` from the environment or the repository's `.env`.
- Do not change `finalize_json.jsonl`, `finalize_text.jsonl` or `speech.wav`. The four research fixtures above are fixed once Task 1 commits them.
- Any check that changes the default output ends with the output it began with (normally "MacBook Pro Speakers", `BuiltInSpeakerDevice`). M2's checks play into BlackHole with `say -a "BlackHole 2ch"` and change no output.
- The repository is public: commit no audio except synthesised `say` output. Recordings stay under `~/Library/Application Support/LectureLive/`. Stage files by name, never `git add -A`.
- Commit messages: imperative, neutral, what and why. No `Co-Authored-By` or any other attribution line, whatever a harness reminder says. No names.
- Do not edit files with Python scripts. In the Bash tool `ls` is `eza`: use `command ls` or explicit paths.
- Any task that creates or changes UI (desktop app views, `notes_template.html`, any page a person looks at) invokes `/frontend-design:frontend-design` before any markup is written (spec §9.4). M2 has no UI tasks.
- The Python CLI (`live_notes.py`, `notes_template.html`, `pyproject.toml`, `.venv/`) and `README.md` stay unchanged. In `docs/`, only this plan, the M2 section and M2 Status row of `docs/milestones.md`, and spec §5.1–5.4 change.
- Crate APIs below are written against the versions named. Where an API differs, adapt the implementation and keep the task's tests unchanged: the tests are the contract. The one exception is a test that encodes an unverified fact about an external system that live evidence contradicts. Change such a test only with the evidence recorded as a Ruling in the ledger.
- When a check fails, record the observation and the spec §14.1 fallback it points to. Do not build the fallback. §14.1 covers loopback only, so an STT failure is recorded with "no §14.1 fallback applies".

## Review Focus

1. **The network goes away without closing the socket** (Wi-Fi drops mid-lecture). Expected: within `idle_timeout` (5 s; the server speaks every second or two while audio flows) the connection counts as lost. Its unclosed audio becomes a transcript gap, reconnection starts with backoff, and the recording is untouched. Pinned by `stt_stream::a_silent_connection_is_treated_as_lost` (Task 5).
2. **The recording ends while STT is connecting or backing off** (the receiver is unplugged during an outage). Expected: `Ended` with a gap to the recording's end, no `audio.done` on a dead socket, no hang; the next recording connects afresh. Pinned by `stt_stream::a_recording_that_ends_while_offline_ends_with_a_gap` (Task 5).
3. **The STT worker falls behind** (a slow network send). Expected: frames are dropped at the coordinator without waiting, `End` still reaches the worker because the queue keeps room for control messages, and the recording is unaffected. Pinned by `coordinator::tests::a_stuck_stt_worker_loses_frames_not_control_messages` (Task 9).
4. **A crash in the middle of recovering a long gap.** Expected: the next session recovers only what the segment log does not already hold, so no transcript line is written twice. Pinned by `coordinator::tests::recovery_resumes_after_the_pieces_already_logged` (Task 9).
5. **Keyterms with spaces, `&` or non-ASCII, and too many or too long.** Expected: each is URL-encoded as its own parameter, and the 101st term, or one over 50 characters, is refused before any connection. Pinned by `stream::tests::keyterms_are_validated_and_encoded_one_by_one` (Task 5).

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/core/src/stt/protocol.rs` | Wire format: server messages, control messages, seconds → samples, refusal classification | 1 |
| `crates/core/src/stt/transcript.rs` | Transcript state machine of one connection: stable chunks, tentative tail, closes | 2 |
| `crates/core/src/session/sidecar.rs` | Transcript gap kinds, `Gap::new` (the resolved rule), `OpenUtterance` marker, `wall_time_at` | 3 |
| `crates/core/tests/support/*` | Fake STT websocket (replay + synthetic speech + faults), fake REST, fixtures, test sources | 4, 8, 10 |
| `crates/core/src/stt/stream.rs` | STT worker: connect, ordered writer, epochs, hold, reconnect, gaps, refusals, watchdog, end; then cutoffs | 5, 6 |
| `crates/core/src/session/segments.rs` | Segment log (`<stem>.segments.jsonl`) and transcript file (`lecture_transcript_*.txt`) | 7 |
| `crates/core/src/session/launch.rs` | Crash: the open utterance after the last logged segment becomes a gap | 7 |
| `crates/core/src/stt/rest.rs` | REST client, piece cutting, recording reads, recovery worker | 8 |
| `crates/core/src/session/coordinator.rs` | STT and recovery links, commits, gaps, marker, cutoff API, shutdown drain | 9 |
| `crates/core/tests/stt_stream.rs`, `stt_rest.rs`, `stt_gate.rs`, `fake_stt.rs`, `stt_live.rs` | Integration tests (gate in `stt_gate.rs`; live checks `#[ignore]`) | 4–10 |
| `crates/cli/src/main.rs` | Probe options (research); `record --stt --keyterm` | 1, 11 |
| `docs/spec.md` §5.1–5.4 | Rewritten to the recorded protocol and M2's design | 12 |
| `docs/milestones.md` M2 section + Status row | Gate lines, Findings, Status | 13 |

---

### Task 1: Wire types and recorded fixtures (milestone task 1, part)

**Files:**
- Create: `crates/core/src/stt/protocol.rs`; `crates/core/tests/fixtures/stt/{endpoint_pauses,finalize_silence,finalize_between_pair,silence_after_speech}.jsonl` (in the working tree from plan research)
- Modify: `crates/core/src/stt/mod.rs`, `crates/cli/src/main.rs` (probe `--url`, `--pace-ms`; in the working tree from plan research)

**Interfaces:**
- Produces: `stt::protocol::{ServerMsg, Partial, ServerWord, parse, to_samples, is_refusal, refusal_message, FINALIZE, AUDIO_DONE, SERVER_RESOLUTION}`.
  - `ServerMsg::{Created, Partial(Partial), Done { duration: f64 }, Error { message: String }, Other}`
  - `Partial { text: String, words: Vec<ServerWord>, is_final: bool, speech_final: bool, start: f64, duration: f64 }`
  - `ServerWord { text: String, start: f64, end: f64 }`
  - `parse(&str) -> serde_json::Result<ServerMsg>`; `to_samples(f64) -> u64`; `is_refusal(u16) -> bool`; `refusal_message(&str) -> String`; `SERVER_RESOLUTION: u64 = 16`.

If a research fixture is missing, regenerate it: `say -o /tmp/x.wav --file-format=WAVE --data-format=LEI16@16000 "<text>"`, then `cargo run -q -p lecturelive-cli -- canary stt-probe /tmp/x.wav --log <fixture> [--finalize-after-secs S]`. The texts were "Gradient descent updates the weights. [[slnc 2000]] The learning rate controls the step size. [[slnc 2000]] Momentum smooths the updates." (pauses: none; between-pair: 2.8), "[[slnc 3000]]" (silence: 1.5), and "The gradient points uphill. [[slnc 10000]]" (silence after speech: none). A regenerated fixture has different times; Task 2's expected values then come from it, and the ledger records that.

- [ ] **Step 1: Write the failing tests**

Create `crates/core/src/stt/protocol.rs` with only the test module:

```rust
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

    #[test]
    fn unknown_message_types_are_kept_not_fatal() {
        assert_eq!(parse(r#"{"type":"transcript.speech_started","at":1.2}"#).unwrap(), ServerMsg::Other);
    }
}
```

Replace `crates/core/src/stt/mod.rs` with:

```rust
pub mod probe;
pub mod protocol;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core protocol`
Expected: compile errors (`parse`, `ServerMsg` not found).

- [ ] **Step 3: Implement**

Prepend to `crates/core/src/stt/protocol.rs`:

```rust
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
pub fn refusal_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.trim().to_string())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core protocol` → 5 passed. Run `cargo build -p lecturelive-cli` → builds (probe options).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/stt/protocol.rs crates/core/src/stt/mod.rs crates/cli/src/main.rs \
  crates/core/tests/fixtures/stt/endpoint_pauses.jsonl crates/core/tests/fixtures/stt/finalize_silence.jsonl \
  crates/core/tests/fixtures/stt/finalize_between_pair.jsonl crates/core/tests/fixtures/stt/silence_after_speech.jsonl
git commit -m "Add the STT wire types and the protocol fixtures recorded for M2

The probe takes the endpoint URL and pacing, which recorded natural
endpointing, finalize in silence and between a final pair, and the
server's messages through long silence. Every recorded server message
parses into the wire types."
```

---

### Task 2: Transcript state machine (milestone task 2)

**Files:**
- Create: `crates/core/src/stt/transcript.rs`
- Modify: `crates/core/src/stt/mod.rs`

**Interfaces:**
- Consumes: `protocol::{Partial, ServerWord, to_samples}`.
- Produces: `stt::transcript::{Transcript, Update, Utterance, Word}`.
  - `Word { text: String, start_sample: u64, end_sample: u64 }` (serde; recording samples)
  - `Utterance { start_sample: u64, end_sample: u64, text: String, words: Vec<Word> }`
  - `Update::{Open { stable: String, tentative: String }, Closed(Utterance)}`
  - `Transcript::{new(origin: u64) -> Self, closed_through(&self) -> u64, apply(&mut self, &Partial) -> Option<Update>}`. `apply` returns `Open` only when the display changed. It returns `Closed` for every `speech_final` past the last close, including an empty one, so callers can advance and correlate.

- [ ] **Step 1: Write the failing tests**

Create `crates/core/src/stt/transcript.rs` with only the test module:

```rust
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
```

Replace `crates/core/src/stt/mod.rs` with:

```rust
pub mod probe;
pub mod protocol;
pub mod transcript;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core transcript`
Expected: compile errors (`Transcript`, `Utterance` not found).

- [ ] **Step 3: Implement**

Prepend to `crates/core/src/stt/transcript.rs`:

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core transcript` → 9 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/stt/transcript.rs crates/core/src/stt/mod.rs
git commit -m "Add the transcript state machine with exact fixture outputs

A final's range replaces the stable chunks it overlaps, a speech_final
closes the open utterance over its own span, an empty hypothesis never
erases stable words and a final for closed audio is ignored. All six
recorded sessions replay to exact utterances and word times."
```

---

### Task 3: Transcript gap kinds, the resolved rule, the open-utterance marker (milestone task 5, part)

**Files:**
- Modify: `crates/core/src/session/sidecar.rs`, `crates/core/src/audio/source.rs:139,231`, `crates/core/src/session/coordinator.rs:167`, `crates/core/src/session/launch.rs:62` and its test at line 159

**Interfaces:**
- Produces:
  - `GapKind::{SttOffline, SttOverflow, SttRefused, SttInterrupted}` (serde `stt_offline` …) and `GapKind::is_transcript(self) -> bool`
  - `Gap::new(recording_id: Uuid, start_sample: u64, end_sample: Option<u64>, kind: GapKind) -> Gap` with `resolved = !kind.is_transcript()`
  - `Sidecar.open_utterance: Option<OpenUtterance>` (serde default, omitted when `None`); `OpenUtterance { recording_id: Uuid, from_sample: u64 }` (Copy)
  - `sidecar::wall_time_at(anchor: DateTime<Local>, sample: u64) -> DateTime<Local>`

- [ ] **Step 1: Write the failing tests**

Append to the test module of `crates/core/src/session/sidecar.rs`:

```rust
    #[test]
    fn audio_gaps_are_resolved_when_recorded_and_transcript_gaps_wait_for_recovery() {
        let id = Uuid::new_v4();
        for kind in [GapKind::CaptureOverflow, GapKind::RecorderOverflow, GapKind::DeviceGone, GapKind::RateChange, GapKind::Interrupted] {
            assert!(!kind.is_transcript());
            assert!(Gap::new(id, 0, None, kind).resolved, "{kind:?} has no audio to recover");
        }
        for kind in [GapKind::SttOffline, GapKind::SttOverflow, GapKind::SttRefused, GapKind::SttInterrupted] {
            assert!(kind.is_transcript());
            assert!(!Gap::new(id, 0, Some(16_000), kind).resolved, "{kind:?} waits for recovery");
        }
    }

    #[test]
    fn transcript_gaps_and_the_open_utterance_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = sidecar_path(dir.path(), "x");
        let id = Uuid::new_v4();
        let mut s = Sidecar::default();
        s.recordings.push(entry(id));
        s.gaps.push(Gap::new(id, 48_000, Some(96_000), GapKind::SttOffline));
        s.open_utterance = Some(OpenUtterance { recording_id: id, from_sample: 96_000 });
        s.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"stt_offline\"") && text.contains("\"open_utterance\""), "{text}");
        assert_eq!(Sidecar::load(&path).unwrap().unwrap(), s);

        s.open_utterance = None;
        s.save(&path).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("open_utterance"));
    }

    #[test]
    fn an_m1_sidecar_loads_with_no_open_utterance() {
        let m1 = r#"{"version":2,"recordings":[],"gaps":[{"recording_id":"6f2c1f7e-0d6b-4d0c-9b8e-2a4c1f0e9d31","start_sample":0,"end_sample":null,"kind":"device_gone","resolved":false}]}"#;
        let s: Sidecar = serde_json::from_str(m1).unwrap();
        assert_eq!(s.open_utterance, None);
        assert_eq!(s.gaps[0].kind, GapKind::DeviceGone);
    }

    #[test]
    fn wall_time_is_the_anchor_plus_samples() {
        let anchor = Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 0).unwrap();
        assert_eq!(wall_time_at(anchor, 66_288), anchor + chrono::Duration::milliseconds(4_143));
    }
```

Append to the test module of `crates/core/src/session/launch.rs`:

```rust
    /// The resolved rule's effect on retention: gaps with no audio behind them do not keep a recording.
    #[test]
    fn a_recording_is_kept_only_while_its_transcript_awaits_recovery() {
        let (audio_only, pending) = (Uuid::new_v4(), Uuid::new_v4());
        let mut sc = Sidecar::default();
        sc.recordings.push(entry(audio_only, "a.wav", 30, RecState::Finalized));
        sc.recordings.push(entry(pending, "b.wav", 30, RecState::Finalized));
        sc.gaps.push(Gap::new(audio_only, 6_400, None, GapKind::DeviceGone));
        sc.gaps.push(Gap::new(audio_only, 0, Some(1_600), GapKind::CaptureOverflow));
        sc.gaps.push(Gap::new(pending, 0, Some(16_000), GapKind::SttOffline));
        assert_eq!(prunable(&sc, Retention::KeepDays(14), now()), vec![audio_only]);
    }
```

In `launch::tests::open_recording_is_repaired_and_its_tail_marked_interrupted`, change the expected gap's `resolved: false` to `resolved: true`: an interrupted tail has no audio to recover.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core sidecar launch`
Expected: compile errors (`SttOffline`, `Gap::new`, `OpenUtterance`, `wall_time_at` not found).

- [ ] **Step 3: Implement**

In `crates/core/src/session/sidecar.rs`:

Add the field to `Sidecar` after `gaps`, and to its `Default`:

```rust
    /// The live transcript's open interval (spec §5.2, §8): this recording was streamed from
    /// `from_sample` and is committed only as far as the segment log shows. A crash turns the rest into a gap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_utterance: Option<OpenUtterance>,
```

```rust
impl Default for Sidecar {
    fn default() -> Self {
        Self { version: SIDECAR_VERSION, recordings: Vec::new(), gaps: Vec::new(), open_utterance: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenUtterance {
    pub recording_id: Uuid,
    pub from_sample: u64,
}
```

Replace the `Gap` doc comment and `GapKind` with:

```rust
/// An interval of a recording without durable audio (audio kinds) or without a committed
/// transcript (`stt_*` kinds). `end_sample: None` runs from `start_sample` to the next recording's
/// anchor, or to the end of the session.
```

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    CaptureOverflow,
    RecorderOverflow,
    DeviceGone,
    RateChange,
    Interrupted,
    /// No live STT connection carried this audio: connecting, disconnected, or it did not finish in time.
    SttOffline,
    /// Frames were dropped on the way to the STT writer.
    SttOverflow,
    /// STT was refused (4xx) and stopped for the session.
    SttRefused,
    /// The session stopped (a crash, or the STT worker ended) before this audio's transcript was committed.
    SttInterrupted,
}

impl GapKind {
    /// Audio exists for the interval but its transcript does not: recovery can fill it (spec §5.4).
    pub fn is_transcript(self) -> bool {
        matches!(self, Self::SttOffline | Self::SttOverflow | Self::SttRefused | Self::SttInterrupted)
    }
}

impl Gap {
    /// A gap is resolved when nothing more can be done for it: a transcript gap once recovery has
    /// committed its interval, an audio gap at once, since it has no audio to recover from.
    pub fn new(recording_id: Uuid, start_sample: u64, end_sample: Option<u64>, kind: GapKind) -> Self {
        Self { recording_id, start_sample, end_sample, kind, resolved: !kind.is_transcript() }
    }
}
```

Replace `Sidecar::wall_time`'s body with `self.recordings.iter().find(|r| r.id == id).map(|r| wall_time_at(r.anchor, sample))`, and add:

```rust
/// Wall time of a recording's sample: its anchor plus sample / 16 kHz (spec §3.3).
pub fn wall_time_at(anchor: DateTime<Local>, sample: u64) -> DateTime<Local> {
    anchor + chrono::Duration::microseconds((sample * 1_000_000 / SAMPLE_RATE as u64) as i64)
}
```

Make every audio-gap creation site use `Gap::new`:
- `crates/core/src/audio/source.rs` (capture overflow): `let gap = Gap::new(self.recording_id, lost.start, Some(lost.end), GapKind::CaptureOverflow);`
- `crates/core/src/audio/source.rs` (end of a segment): `send(out, SourceEvent::Gap(Gap::new(recording_id, seg.samples, None, kind)))?;`
- `crates/core/src/session/coordinator.rs` (recorder overflow): `let gap = Gap::new(id, start, Some(end), GapKind::RecorderOverflow);`
- `crates/core/src/session/launch.rs` (interrupted): `gaps.push(Gap::new(r.id, start_sample, None, GapKind::Interrupted));`

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core` → all pass, including the M1 suites (the coordinator's M1 test builds its own gaps, so it is unaffected).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/session/sidecar.rs crates/core/src/audio/source.rs crates/core/src/session/coordinator.rs crates/core/src/session/launch.rs
git commit -m "Add transcript gap kinds and resolve gaps that have no audio at once

A gap is resolved when nothing more can be done for it: a transcript gap
once recovery commits its interval, an audio gap when it is recorded,
since no audio exists to recover. Retention therefore keeps a recording
exactly while its transcript awaits recovery. The sidecar gains the open
utterance marker that launch repair turns into a gap after a crash."
```

---
### Task 4: Fake STT server replaying fixtures, with synthetic speech and injected faults (milestone task 7)

**Files:**
- Create: `crates/core/tests/support/mod.rs`, `crates/core/tests/support/fixtures.rs`, `crates/core/tests/support/speech.rs`, `crates/core/tests/support/fake_stt.rs`, `crates/core/tests/fake_stt.rs`

**Interfaces:**
- Produces (test support, used by Tasks 5, 6, 10):
  - `fixtures::{load(name) -> Fixture, Fixture { steps: Vec<Step>, frames: usize }, Step { frames, texts, msg }}`. Each server message carries how many frames and text messages the client had sent when it arrived.
  - `speech::{FRAME, frame_pcm(k) -> [i16; 1600], word_start(w), word_end(w), word_text(w), endpoint(u), words_starting(from, to) -> Vec<u64>, expected_words(len) -> Vec<String>, clip_start(&[i16]) -> u64, text(&[u64]) -> String, transcribe_clip(&[i16]) -> Value, Listener}`. Every sample of frame k is k + 1. Word w starts at frame `45·(w/8) + 5·(w%8)` and lasts 4 frames. Utterance u closes by itself 2 frames after its last word (`endpoint(u)`).
  - `fake_stt::{start(Config) -> FakeStt, FakeStt { url, state: Arc<State> }, Config { mode, outages, refuse_all, refuse_after_drop, drop_on_finalize, mute_after_audio_done } (Default), Mode::{Synthetic, Replay(Fixture)}, Outage { at_sample, refuse_for, refusal, freeze }, Refusal::{Status(u16), TcpClose}, State { attempts, accepted, frames, feed, … } + wait_accepted(n), wait_frames(n), texts(), BAD_KEY_BODY}`.

Replay releases each recorded message once the client has sent as many frames and text messages as it had when the message arrived. A client that sends the same frames and control messages at the same frame positions therefore gets the recorded answers in causal order. The contents of the frames and control messages do not matter.

- [ ] **Step 1: Write the self-test**

Create `crates/core/tests/fake_stt.rs`:

```rust
mod support;

use std::sync::atomic::Ordering::SeqCst;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use support::fake_stt::{self, Config, Mode, Refusal, BAD_KEY_BODY};
use support::{fixtures, speech};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn next_text(ws: &mut Client) -> Option<Value> {
    loop {
        match ws.next().await? {
            Ok(Message::Text(t)) => return Some(serde_json::from_str(&t).unwrap()),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

async fn all_texts(ws: &mut Client) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some(v) = next_text(ws).await {
        out.push(v);
    }
    out
}

#[tokio::test]
async fn replay_answers_in_the_recorded_causal_order() {
    let fake = fake_stt::start(Config { mode: Mode::Replay(fixtures::load("finalize_json")), ..Default::default() }).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(fake.url.as_str()).await.unwrap();
    assert_eq!(next_text(&mut ws).await.unwrap()["type"], "transcript.created");
    let frame = Message::Binary(vec![0u8; 3200].into());
    for _ in 0..30 {
        ws.send(frame.clone()).await.unwrap();
    }
    ws.send(Message::Text(r#"{"type":"finalize"}"#.into())).await.unwrap();
    for _ in 30..87 {
        ws.send(frame.clone()).await.unwrap();
    }
    ws.send(Message::Text(r#"{"type":"audio.done"}"#.into())).await.unwrap();
    let want: Vec<Value> = fixtures::load("finalize_json").steps.iter().map(|s| serde_json::from_str(&s.msg).unwrap()).collect();
    assert_eq!(all_texts(&mut ws).await, want);
    assert_eq!(fake.state.frames.load(SeqCst), 87);
}

#[tokio::test]
async fn synthetic_speech_closes_utterances_at_endpoints_and_at_a_finalize() {
    let fake = fake_stt::start(Config::default()).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(fake.url.as_str()).await.unwrap();
    next_text(&mut ws).await.unwrap();
    for k in 0..60 {
        let bytes: Vec<u8> = speech::frame_pcm(k).iter().flat_map(|s| s.to_le_bytes()).collect();
        ws.send(Message::Binary(bytes.into())).await.unwrap();
    }
    ws.send(Message::Text(r#"{"type":"finalize"}"#.into())).await.unwrap();
    ws.send(Message::Text(r#"{"type":"audio.done"}"#.into())).await.unwrap();
    let msgs = all_texts(&mut ws).await;
    let finals: Vec<(String, f64)> = msgs
        .iter()
        .filter(|m| m["speech_final"] == true)
        .map(|m| (m["text"].as_str().unwrap().to_string(), m["start"].as_f64().unwrap() + m["duration"].as_f64().unwrap()))
        .collect();
    assert_eq!(finals.len(), 2);
    assert_eq!(finals[0].0, "w0 w1 w2 w3 w4 w5 w6 w7");
    assert!((finals[0].1 - 4.1).abs() < 1e-9);
    assert_eq!(finals[1].0, "w8 w9 w10");
    assert!((finals[1].1 - 6.0).abs() < 1e-9);
    assert_eq!(msgs.last().unwrap()["type"], "transcript.done");
    assert_eq!(msgs.last().unwrap()["duration"], 6.0);
}

#[tokio::test]
async fn a_refusal_answers_the_upgrade_with_its_status_and_body() {
    let fake = fake_stt::start(Config { refuse_all: Some(Refusal::Status(400)), ..Default::default() }).await;
    let err = tokio_tungstenite::connect_async(fake.url.as_str()).await.unwrap_err();
    let tungstenite::Error::Http(resp) = err else { panic!("expected an HTTP refusal, got {err}") };
    assert_eq!(resp.status(), 400);
    assert_eq!(resp.body().as_deref(), Some(BAD_KEY_BODY.as_bytes()));
    assert_eq!(fake.state.attempts.load(SeqCst), 1);
}
```

- [ ] **Step 2: Run the self-test to verify it fails**

Run: `cargo test -p lecturelive-core --test fake_stt`
Expected: compile error (`support` module not found).

- [ ] **Step 3: Implement the support modules**

`crates/core/tests/support/mod.rs`:

```rust
//! Test doubles shared by the integration tests (spec §11): fake STT websocket and REST servers,
//! recorded fixtures and synthetic speech.
#![allow(dead_code)]
pub mod fake_stt;
pub mod fixtures;
pub mod speech;
```

`crates/core/tests/support/fixtures.rs`:

```rust
use serde_json::Value;

/// A recorded session as the replaying fake needs it: every server message with the number of
/// frames and text messages the client had sent when it arrived.
pub struct Fixture {
    pub steps: Vec<Step>,
    pub frames: usize,
}

pub struct Step {
    pub frames: usize,
    pub texts: usize,
    pub msg: String,
}

pub fn path(name: &str) -> String {
    format!("{}/tests/fixtures/stt/{name}.jsonl", env!("CARGO_MANIFEST_DIR"))
}

pub fn load(name: &str) -> Fixture {
    let lines: Vec<Value> = std::fs::read_to_string(path(name)).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let sent = |binary: bool| -> Vec<u64> {
        lines.iter().filter(|v| v["dir"] == "out" && v["msg"].is_object() == binary).map(|v| v["t_ms"].as_u64().unwrap()).collect()
    };
    let (frames, texts) = (sent(true), sent(false));
    let mut received: Vec<(u64, &Value)> = lines
        .iter()
        .filter(|v| v["dir"] == "in" && v["msg"]["type"] != "transcript.created")
        .map(|v| (v["t_ms"].as_u64().unwrap(), &v["msg"]))
        .collect();
    received.sort_by_key(|m| m.0);
    let steps = received
        .into_iter()
        .map(|(t, m)| Step {
            frames: frames.iter().filter(|&&f| f <= t).count(),
            texts: texts.iter().filter(|&&x| x <= t).count(),
            msg: m.to_string(),
        })
        .collect();
    Fixture { steps, frames: frames.len() }
}
```

`crates/core/tests/support/speech.rs`:

```rust
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
```

`crates/core/tests/support/fake_stt.rs`:

```rust
//! A fake Grok STT websocket (spec §11). It replays a recorded fixture in causal order, or plays
//! synthetic speech (`speech`), with injected disconnects, silences and refusals.
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::{http, Message};
use tokio_tungstenite::WebSocketStream;

use super::fixtures::Fixture;
use super::speech::{clip_start, Listener};

/// What the real server answers an unknown key with: HTTP 400 at the upgrade.
pub const BAD_KEY_BODY: &str = r#"{"code":"Client specified an invalid argument","error":"Incorrect API key provided. You can obtain an API key from https://console.x.ai."}"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Status(u16),
    TcpClose,
}

/// The connection that hears `at_sample` vanishes (or goes silent, `freeze`). New connections are
/// then refused until the test's source has fed `refuse_for` more samples.
#[derive(Clone, Copy, Debug)]
pub struct Outage {
    pub at_sample: u64,
    pub refuse_for: u64,
    pub refusal: Refusal,
    pub freeze: bool,
}

#[derive(Default)]
pub enum Mode {
    #[default]
    Synthetic,
    Replay(Fixture),
}

#[derive(Default)]
pub struct Config {
    pub mode: Mode,
    pub outages: Vec<Outage>,
    pub refuse_all: Option<Refusal>,
    pub refuse_after_drop: Option<Refusal>,
    pub drop_on_finalize: bool,
    pub mute_after_audio_done: bool,
}

#[derive(Default)]
pub struct State {
    pub attempts: AtomicUsize,
    pub accepted: AtomicUsize,
    pub frames: AtomicUsize,
    texts: Mutex<Vec<String>>,
    /// End of the audio the test's source has produced, in samples.
    pub feed: AtomicU64,
    refuse_until: AtomicU64,
    dropped: AtomicUsize,
}

impl State {
    pub fn texts(&self) -> Vec<String> {
        self.texts.lock().unwrap().clone()
    }

    pub async fn wait_accepted(&self, n: usize) {
        wait(|| self.accepted.load(SeqCst) >= n, "connections").await
    }

    pub async fn wait_frames(&self, n: usize) {
        wait(|| self.frames.load(SeqCst) >= n, "frames").await
    }
}

async fn wait(done: impl Fn() -> bool, what: &str) {
    for _ in 0..5_000 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the fake STT server never saw enough {what}");
}

pub struct FakeStt {
    pub url: String,
    pub state: Arc<State>,
}

pub async fn start(cfg: Config) -> FakeStt {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/v1/stt?model=grok-voice-transcribe-2.0&encoding=pcm&sample_rate=16000&interim_results=true&language=en",
        listener.local_addr().unwrap()
    );
    let state = Arc::new(State::default());
    let (cfg, st) = (Arc::new(cfg), state.clone());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let (cfg, st) = (cfg.clone(), st.clone());
            tokio::spawn(async move { serve(tcp, &cfg, &st).await });
        }
    });
    FakeStt { url, state }
}

fn refusal(cfg: &Config, st: &State) -> Option<Refusal> {
    let dropped = st.dropped.load(SeqCst);
    cfg.refuse_all
        .or(if dropped > 0 { cfg.refuse_after_drop } else { None })
        .or_else(|| (st.feed.load(SeqCst) < st.refuse_until.load(SeqCst)).then(|| cfg.outages[dropped - 1].refusal))
}

async fn serve(tcp: TcpStream, cfg: &Config, st: &State) {
    st.attempts.fetch_add(1, SeqCst);
    let refusal = refusal(cfg, st);
    if refusal == Some(Refusal::TcpClose) {
        return; // dropped before the handshake
    }
    let answer = |_: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        let Some(Refusal::Status(code)) = refusal else { return Ok(resp) };
        let body = if code == 400 { BAD_KEY_BODY.to_string() } else { json!({"error": format!("status {code}")}).to_string() };
        Err(http::Response::builder().status(code).header("content-type", "application/json").body(Some(body)).unwrap())
    };
    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tcp, answer).await else { return };
    st.accepted.fetch_add(1, SeqCst);
    let created = json!({"type": "transcript.created", "id": "00000000-0000-0000-0000-000000000000"});
    if ws.send(Message::Text(created.to_string().into())).await.is_err() {
        return;
    }
    match &cfg.mode {
        Mode::Replay(f) => replay(ws, f, st).await,
        Mode::Synthetic => synthetic(ws, cfg, st).await,
    }
}

async fn replay(mut ws: WebSocketStream<TcpStream>, f: &Fixture, st: &State) {
    let (mut frames, mut texts, mut next) = (0, 0, 0);
    loop {
        while let Some(step) = f.steps.get(next).filter(|s| s.frames <= frames && s.texts <= texts) {
            if ws.send(Message::Text(step.msg.clone().into())).await.is_err() {
                return;
            }
            next += 1;
        }
        if next == f.steps.len() {
            let _ = ws.close(None).await;
            return;
        }
        match ws.next().await {
            Some(Ok(Message::Binary(_))) => {
                frames += 1;
                st.frames.fetch_add(1, SeqCst);
            }
            Some(Ok(Message::Text(t))) => {
                texts += 1;
                st.texts.lock().unwrap().push(t.to_string());
            }
            Some(Ok(_)) => {}
            _ => return,
        }
    }
}

async fn synthetic(mut ws: WebSocketStream<TcpStream>, cfg: &Config, st: &State) {
    let mut ear = Listener::default();
    while let Some(Ok(msg)) = ws.next().await {
        let out: Vec<Value> = match msg {
            Message::Binary(b) => {
                st.frames.fetch_add(1, SeqCst);
                let pcm: Vec<i16> = b.chunks_exact(2).map(|x| i16::from_le_bytes([x[0], x[1]])).collect();
                let at = clip_start(&pcm);
                let fired = st.dropped.load(SeqCst);
                if let Some(o) = cfg.outages.get(fired).filter(|o| at + pcm.len() as u64 > o.at_sample) {
                    st.refuse_until.store(o.at_sample + o.refuse_for, SeqCst);
                    st.dropped.fetch_add(1, SeqCst);
                    if o.freeze {
                        let _held_open = ws;
                        return std::future::pending().await;
                    }
                    return; // gone mid-utterance, without a close frame
                }
                ear.frame(at, pcm.len() as u64)
            }
            Message::Text(t) => {
                st.texts.lock().unwrap().push(t.to_string());
                match serde_json::from_str::<Value>(&t) {
                    Err(e) => vec![json!({"type": "error", "message": format!("Invalid message: {e}")})],
                    Ok(v) if v["type"] == "finalize" => {
                        if cfg.drop_on_finalize {
                            return;
                        }
                        let at = ear.heard_to;
                        ear.close(at, true)
                    }
                    Ok(v) if v["type"] == "audio.done" => {
                        if cfg.mute_after_audio_done {
                            let _held_open = ws;
                            return std::future::pending().await;
                        }
                        let at = ear.heard_to;
                        let mut out = ear.close(at, false);
                        out.push(json!({"type": "transcript.done", "text": "", "words": [], "duration": ear.heard_secs()}));
                        for m in out {
                            let _ = ws.send(Message::Text(m.to_string().into())).await;
                        }
                        let _ = ws.close(None).await;
                        return;
                    }
                    Ok(_) => vec![],
                }
            }
            _ => vec![],
        };
        for m in out {
            if ws.send(Message::Text(m.to_string().into())).await.is_err() {
                return;
            }
        }
    }
}
```

- [ ] **Step 4: Run the self-test to verify it passes**

Run: `cargo test -p lecturelive-core --test fake_stt` → 3 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/core/tests/fake_stt.rs crates/core/tests/support/mod.rs crates/core/tests/support/fixtures.rs \
  crates/core/tests/support/speech.rs crates/core/tests/support/fake_stt.rs
git commit -m "Add a fake STT server that replays fixtures and plays synthetic speech

Replay releases each recorded message once the client has sent what it
had sent when the message arrived. Synthetic speech encodes its own
position in every sample, so the fakes know which words any stretch of
audio holds, and outages, silent sockets and refusals can be injected at
exact sample positions."
```

---

### Task 5: STT worker: connection, ordered writer, epochs, reconnect, gaps, refusals (milestone tasks 1, 5 and 6, worker side)

**Files:**
- Create: `crates/core/src/stt/stream.rs`, `crates/core/tests/stt_stream.rs`
- Modify: `crates/core/src/stt/mod.rs`, `crates/core/Cargo.toml` (`reqwest`)

**Interfaces:**
- Consumes: `protocol::*` (Task 1), `transcript::{Transcript, Update, Utterance}` (Task 2), `Gap::new`, `GapKind::Stt*` (Task 3), `audio::frame::{Frame, FRAME_SAMPLES}`.
- Produces:
  - `stt::stream::{STT_URL, INPUT_QUEUE (64), SttConfig, validate_keyterms, SttInput, SttEvent, SttLink, spawn}`
  - `SttConfig { url, api_key, keyterms, backoff_unit, connect_timeout, send_timeout, idle_timeout, done_wait }`, `SttConfig::new(api_key, keyterms)` (1 s, 10 s, 5 s, 5 s, 5 s), `request_url(&self) -> Result<String>`
  - `SttInput::{Begin { recording_id }, Frame(Frame), End { recording_id, samples }}`
  - `SttEvent::{Connected { recording_id, origin, gap: Option<Gap> }, Open { recording_id, stable, tentative }, Utterance { recording_id, utterance }, Ended { recording_id, gap: Option<Gap> }, Retrying { after: Duration, reason }, ServerError(String), Refused(String)}`
  - `SttLink { input: mpsc::Sender<SttInput>, events: mpsc::Receiver<SttEvent> }`; `spawn(SttConfig) -> Result<SttLink>` (fails on a bad keyterm; needs a Tokio runtime)
- Invariants later tasks rely on: for each recording, one `Ended`. Every `Connected` gap and every `Ended` gap is disjoint from the others and from every `Utterance` span. `Connected` is emitted when the first frame goes out on the connection. An `Utterance` never has empty text.

Behaviour, in one place:
- `Begin` starts a connection at once. While it is not live, frames are held, the newest 50 (5 s). When `transcript.created` arrives, the held frames go first, as a burst. The first frame sent fixes the epoch origin and emits `Connected` with the gap `[settled, origin)`, if any.
- The recording's transcript is settled through the end of the last `speech_final` (empty ones included) and, after `transcript.done`, through `origin + duration`.
- A connection counts as lost on close, read error, a send that fails or takes over `send_timeout`, no server message for `idle_timeout`, or a frame whose offset is not the expected next one (frames were dropped before the writer). A lost connection waits `backoff_unit × 1, 2, 4 … 30` and reconnects; the wait resets once a connection is live. The gap's kind is the first cause since the last settled point.
- A 4xx at the upgrade (except 408/429) is `Refused`: no further connection this session, and every recording's untranscribed audio becomes an `stt_refused` gap.
- `End` aborts a pending connect. A live epoch gets `audio.done` and its finals are read until `transcript.done` or `done_wait`. Then `Ended` carries the gap `[settled, samples)` if more than 1 ms remains.

- [ ] **Step 1: Add the dependency**

Run: `cargo add -p lecturelive-core reqwest --no-default-features --features multipart,json,rustls-no-provider`
Expected: `reqwest v0.13.5`, with no `aws-lc-rs` in `cargo tree -p lecturelive-core -i aws-lc-rs` (the command reports no match). Record the version in the ledger. The worker uses `reqwest::Url` to encode keyterms; Task 8 uses the client.

- [ ] **Step 2: Write the failing tests**

Create `crates/core/src/stt/stream.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(terms: Vec<String>) -> SttConfig {
        SttConfig::new("k".into(), terms)
    }

    #[test]
    fn keyterms_are_validated_and_encoded_one_by_one() {
        let url = cfg(vec!["gradient descent".into(), "Q&A".into(), "Softmax–Temperatur".into()]).request_url().unwrap();
        assert!(url.starts_with(STT_URL), "{url}");
        assert!(url.ends_with("&keyterm=gradient+descent&keyterm=Q%26A&keyterm=Softmax%E2%80%93Temperatur"), "{url}");
        assert_eq!(cfg(vec![]).request_url().unwrap(), STT_URL);
        assert!(cfg(vec!["x".repeat(51)]).request_url().is_err());
        assert!(cfg(vec!["ü".repeat(50)]).request_url().is_ok(), "the limit counts characters, not bytes");
        assert!(cfg((0..101).map(|i| format!("t{i}")).collect()).request_url().is_err());
        assert!(cfg(vec!["  ".into()]).request_url().is_err());
    }
}
```

Create `crates/core/tests/stt_stream.rs`:

```rust
mod support;

use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

use lecturelive_core::audio::frame::Frame;
use lecturelive_core::session::sidecar::{Gap, GapKind};
use lecturelive_core::stt::stream::{spawn, SttConfig, SttEvent, SttInput, SttLink};
use lecturelive_core::stt::transcript::Utterance;
use support::fake_stt::{self, Config, FakeStt, Mode, Outage, Refusal};
use support::{fixtures, speech};
use uuid::Uuid;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn cfg(fake: &FakeStt) -> SttConfig {
    SttConfig {
        url: fake.url.clone(),
        backoff_unit: ms(1),
        connect_timeout: ms(2_000),
        send_timeout: ms(2_000),
        idle_timeout: ms(2_000),
        done_wait: ms(2_000),
        ..SttConfig::new("test-key".into(), vec![])
    }
}

fn speech_frame(id: Uuid, k: u64) -> Frame {
    Frame { recording_id: id, sample_offset: k * 1600, valid_samples: 1600, pcm16: speech::frame_pcm(k) }
}

/// The frames of a recording `samples` long, silent, the last one partial.
fn silent_frames(id: Uuid, samples: u64) -> Vec<Frame> {
    (0..samples.div_ceil(1600))
        .map(|k| Frame { recording_id: id, sample_offset: k * 1600, valid_samples: (samples - k * 1600).min(1600) as u32, pcm16: [0; 1600] })
        .collect()
}

/// Sends frames paced like a fast source and tells the fake how far the audio has got.
async fn feed(link: &SttLink, fake: &FakeStt, frames: impl IntoIterator<Item = Frame>) {
    for f in frames {
        let end = f.sample_offset + f.valid_samples as u64;
        link.input.send(SttInput::Frame(f)).await.unwrap();
        fake.state.feed.store(end, SeqCst);
        tokio::time::sleep(ms(1)).await;
    }
}

async fn next_event(link: &mut SttLink) -> SttEvent {
    tokio::time::timeout(Duration::from_secs(20), link.events.recv()).await.expect("an event within 20 s").expect("the worker is running")
}

async fn until_ended(link: &mut SttLink) -> Vec<SttEvent> {
    let mut seen = Vec::new();
    loop {
        let e = next_event(link).await;
        let ended = matches!(e, SttEvent::Ended { .. });
        seen.push(e);
        if ended {
            return seen;
        }
    }
}

fn utterances(ev: &[SttEvent]) -> Vec<Utterance> {
    ev.iter()
        .filter_map(|e| match e {
            SttEvent::Utterance { utterance, .. } => Some(utterance.clone()),
            _ => None,
        })
        .collect()
}

fn spans(ev: &[SttEvent]) -> Vec<(u64, u64, String)> {
    utterances(ev).into_iter().map(|u| (u.start_sample, u.end_sample, u.text)).collect()
}

fn origins(ev: &[SttEvent]) -> Vec<(u64, Option<Gap>)> {
    ev.iter()
        .filter_map(|e| match e {
            SttEvent::Connected { origin, gap, .. } => Some((*origin, gap.clone())),
            _ => None,
        })
        .collect()
}

fn gaps(ev: &[SttEvent]) -> Vec<Gap> {
    ev.iter()
        .filter_map(|e| match e {
            SttEvent::Connected { gap, .. } | SttEvent::Ended { gap, .. } => gap.clone(),
            _ => None,
        })
        .collect()
}

/// Every synthetic word exactly once: the live words, plus the words recovery will find in each gap.
fn assert_partition(ev: &[SttEvent], len: u64) {
    let mut words: Vec<(u64, String)> = utterances(ev).iter().flat_map(|u| u.words.iter().map(|w| (w.start_sample, w.text.clone()))).collect();
    for g in gaps(ev) {
        words.extend(speech::words_starting(g.start_sample, g.end_sample.unwrap()).into_iter().map(|w| (speech::word_start(w), speech::word_text(w))));
    }
    words.sort();
    assert_eq!(words.into_iter().map(|w| w.1).collect::<Vec<_>>(), speech::expected_words(len));
}

async fn begin(link: &SttLink) -> Uuid {
    let id = Uuid::new_v4();
    link.input.send(SttInput::Begin { recording_id: id }).await.unwrap();
    id
}

async fn end(link: &SttLink, id: Uuid, samples: u64) {
    link.input.send(SttInput::End { recording_id: id, samples }).await.unwrap();
}

#[tokio::test]
async fn natural_endpoints_replay_to_exact_utterances() {
    let fake = fake_stt::start(Config { mode: Mode::Replay(fixtures::load("endpoint_pauses")), ..Default::default() }).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    fake.state.wait_accepted(1).await;
    feed(&link, &fake, silent_frames(id, 167_615)).await;
    end(&link, id, 167_615).await;
    let ev = until_ended(&mut link).await;
    assert_eq!(origins(&ev), vec![(0, None)]);
    assert_eq!(
        spans(&ev),
        vec![
            (16, 49_280, "Gradient descent updates the weights.".to_string()),
            (59_520, 117_760, "The learning rate controls the step size.".to_string()),
            (128_640, 167_616, "Momentum smooths the updates.".to_string()),
        ]
    );
    assert_eq!(ev.last(), Some(&SttEvent::Ended { recording_id: id, gap: None }));
    assert_eq!(fake.state.frames.load(SeqCst), 105);
    assert_eq!(fake.state.texts(), vec![r#"{"type":"audio.done"}"#.to_string()]);
}

#[tokio::test]
async fn each_recording_gets_its_own_connection_from_its_first_frame() {
    let fake = fake_stt::start(Config::default()).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    for n in [50, 30] {
        let id = begin(&link).await;
        feed(&link, &fake, (0..n).map(|k| speech_frame(id, k))).await;
        end(&link, id, n * 1600).await;
        let ev = until_ended(&mut link).await;
        assert_eq!(origins(&ev), vec![(0, None)]);
        assert!(gaps(&ev).is_empty(), "{:?}", gaps(&ev));
        assert_partition(&ev, n * 1600);
    }
    assert_eq!(fake.state.accepted.load(SeqCst), 2);
}

#[tokio::test]
async fn a_dropped_connection_reconnects_and_its_unclosed_audio_becomes_a_gap() {
    let outage = Outage { at_sample: 30 * 16_000, refuse_for: 3 * 16_000, refusal: Refusal::Status(503), freeze: false };
    let fake = fake_stt::start(Config { outages: vec![outage], ..Default::default() }).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..600).map(|k| speech_frame(id, k))).await;
    end(&link, id, 600 * 1600).await;
    let ev = until_ended(&mut link).await;
    let o = origins(&ev);
    assert_eq!(o.len(), 2, "{o:?}");
    let gap = o[1].1.clone().expect("the reconnect reports what it missed");
    assert_eq!((gap.start_sample, gap.kind, gap.resolved), (speech::endpoint(5), GapKind::SttOffline, false));
    assert_eq!(gap.end_sample, Some(o[1].0));
    assert!(o[1].0 > 30 * 16_000);
    assert!(ev.iter().any(|e| matches!(e, SttEvent::Retrying { .. })));
    assert_partition(&ev, 600 * 1600);
}

#[tokio::test]
async fn after_a_long_outage_the_stream_resumes_at_most_five_seconds_back() {
    let outage = Outage { at_sample: 10 * 16_000, refuse_for: 20 * 16_000, refusal: Refusal::TcpClose, freeze: false };
    let fake = fake_stt::start(Config { outages: vec![outage], ..Default::default() }).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..400).map(|k| speech_frame(id, k))).await;
    end(&link, id, 400 * 1600).await;
    let ev = until_ended(&mut link).await;
    let o = origins(&ev);
    assert_eq!(o.len(), 2, "{o:?}");
    assert!(o[1].0 >= 25 * 16_000, "refused until 30 s, and the hold keeps only 5 s: resumed at {}", o[1].0);
    assert_partition(&ev, 400 * 1600);
}

#[tokio::test]
async fn frames_that_never_reached_the_writer_restart_the_epoch_with_an_overflow_gap() {
    let fake = fake_stt::start(Config::default()).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..100).chain(110..200).map(|k| speech_frame(id, k))).await;
    end(&link, id, 200 * 1600).await;
    let ev = until_ended(&mut link).await;
    let o = origins(&ev);
    assert_eq!(o[0], (0, None));
    assert_eq!(o[1].0, 110 * 1600);
    assert_eq!(o[1].1.as_ref().map(|g| g.kind), Some(GapKind::SttOverflow));
    assert_partition(&ev, 200 * 1600);
}

#[tokio::test]
async fn a_refusal_stops_stt_for_the_session_without_retrying() {
    let fake = fake_stt::start(Config { refuse_all: Some(Refusal::Status(400)), ..Default::default() }).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    for first in [true, false] {
        let id = begin(&link).await;
        feed(&link, &fake, (0..20).map(|k| speech_frame(id, k))).await;
        end(&link, id, 32_000).await;
        let ev = until_ended(&mut link).await;
        assert_eq!(gaps(&ev), vec![Gap::new(id, 0, Some(32_000), GapKind::SttRefused)]);
        let refused = SttEvent::Refused("400 Bad Request: Incorrect API key provided. You can obtain an API key from https://console.x.ai.".into());
        assert_eq!(ev.contains(&refused), first);
        assert!(!ev.iter().any(|e| matches!(e, SttEvent::Retrying { .. })));
    }
    assert_eq!(fake.state.attempts.load(SeqCst), 1);
}

#[tokio::test]
async fn a_refusal_after_a_drop_stops_reconnecting() {
    let outage = Outage { at_sample: 10 * 16_000, refuse_for: 0, refusal: Refusal::Status(503), freeze: false };
    let fake = fake_stt::start(Config { outages: vec![outage], refuse_after_drop: Some(Refusal::Status(401)), ..Default::default() }).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..200).map(|k| speech_frame(id, k))).await;
    end(&link, id, 200 * 1600).await;
    let ev = until_ended(&mut link).await;
    assert!(ev.iter().any(|e| matches!(e, SttEvent::Refused(_))));
    assert_eq!(fake.state.attempts.load(SeqCst), 2);
    assert_eq!(gaps(&ev), vec![Gap::new(id, speech::endpoint(1), Some(200 * 1600), GapKind::SttRefused)]);
    assert_partition(&ev, 200 * 1600);
}

#[tokio::test]
async fn a_silent_connection_is_treated_as_lost() {
    let outage = Outage { at_sample: 10 * 16_000, refuse_for: 0, refusal: Refusal::Status(503), freeze: true };
    let fake = fake_stt::start(Config { outages: vec![outage], ..Default::default() }).await;
    let mut link = spawn(SttConfig { idle_timeout: ms(300), ..cfg(&fake) }).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..300).map(|k| speech_frame(id, k))).await;
    end(&link, id, 300 * 1600).await;
    let ev = until_ended(&mut link).await;
    assert!(ev.iter().any(|e| matches!(e, SttEvent::Retrying { reason, .. } if reason.contains("no message"))), "{ev:?}");
    assert_eq!(origins(&ev).len(), 2);
    assert_partition(&ev, 300 * 1600);
}

#[tokio::test]
async fn a_recording_that_ends_while_offline_ends_with_a_gap() {
    let fake = fake_stt::start(Config { refuse_all: Some(Refusal::TcpClose), ..Default::default() }).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..30).map(|k| speech_frame(id, k))).await;
    end(&link, id, 48_000).await;
    let ev = tokio::time::timeout(Duration::from_secs(2), until_ended(&mut link)).await.expect("no hang");
    assert_eq!(gaps(&ev), vec![Gap::new(id, 0, Some(48_000), GapKind::SttOffline)]);
    assert!(fake.state.attempts.load(SeqCst) > 1, "a transient failure is retried");
}

#[tokio::test]
async fn a_missing_transcript_done_leaves_the_unclosed_tail_as_a_gap() {
    let fake = fake_stt::start(Config { mute_after_audio_done: true, ..Default::default() }).await;
    let mut link = spawn(SttConfig { done_wait: ms(200), ..cfg(&fake) }).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..60).map(|k| speech_frame(id, k))).await;
    end(&link, id, 96_000).await;
    let ev = until_ended(&mut link).await;
    assert_eq!(gaps(&ev), vec![Gap::new(id, speech::endpoint(0), Some(96_000), GapKind::SttOffline)]);
    assert_partition(&ev, 96_000);
}
```

Replace `crates/core/src/stt/mod.rs` with:

```rust
pub mod probe;
pub mod protocol;
pub mod stream;
pub mod transcript;
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core --test stt_stream` and `cargo test -p lecturelive-core stream::tests`
Expected: compile errors (`spawn`, `SttConfig` not found).

- [ ] **Step 4: Implement**

Prepend to `crates/core/src/stt/stream.rs`:

```rust
//! The live STT connection (spec §5.1, §5.4): one worker per session writes every frame in order.
//! Each connection is an epoch whose server times count from the first frame sent on it.
use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{ensure, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

use super::protocol::{self, is_refusal, refusal_message, to_samples, Partial, ServerMsg, AUDIO_DONE, SERVER_RESOLUTION};
use super::transcript::{Transcript, Update, Utterance};
use crate::audio::frame::{Frame, FRAME_SAMPLES};
use crate::session::sidecar::{Gap, GapKind};

pub const STT_URL: &str = "wss://api.x.ai/v1/stt?model=grok-voice-transcribe-2.0&encoding=pcm&sample_rate=16000&interim_results=true&language=en";
/// Frames and control messages between the coordinator and the writer.
pub const INPUT_QUEUE: usize = 64;
const MAX_KEYTERMS: usize = 100;
const MAX_KEYTERM_CHARS: usize = 50;
/// Frames held while connecting (5 s): a short outage is streamed late instead of recovered.
const HOLD_FRAMES: usize = 50;
const MAX_BACKOFF_UNITS: u32 = 30;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone)]
pub struct SttConfig {
    pub url: String,
    pub api_key: String,
    pub keyterms: Vec<String>,
    /// Reconnect waits are 1, 2, 4 … 30 of these.
    pub backoff_unit: Duration,
    /// Through `transcript.created`.
    pub connect_timeout: Duration,
    /// One frame's send; longer means the connection is stuck.
    pub send_timeout: Duration,
    /// The server speaks every second or two while audio flows: this long without a message means the connection is dead.
    pub idle_timeout: Duration,
    /// After `audio.done`, the wait for `transcript.done`.
    pub done_wait: Duration,
}

impl SttConfig {
    pub fn new(api_key: String, keyterms: Vec<String>) -> Self {
        Self {
            url: STT_URL.into(),
            api_key,
            keyterms,
            backoff_unit: Duration::from_secs(1),
            connect_timeout: Duration::from_secs(10),
            send_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(5),
            done_wait: Duration::from_secs(5),
        }
    }

    /// The connection URL: every keyterm validated, then encoded as its own `keyterm` parameter.
    pub fn request_url(&self) -> Result<String> {
        validate_keyterms(&self.keyterms)?;
        if self.keyterms.is_empty() {
            return Ok(self.url.clone());
        }
        let mut url = reqwest::Url::parse(&self.url)?;
        url.query_pairs_mut().extend_pairs(self.keyterms.iter().map(|k| ("keyterm", k.trim())));
        Ok(url.into())
    }
}

pub fn validate_keyterms(terms: &[String]) -> Result<()> {
    ensure!(terms.len() <= MAX_KEYTERMS, "at most {MAX_KEYTERMS} keyterms, {} given", terms.len());
    for t in terms {
        let n = t.trim().chars().count();
        ensure!((1..=MAX_KEYTERM_CHARS).contains(&n), "keyterm {t:?} must have 1 to {MAX_KEYTERM_CHARS} characters");
    }
    Ok(())
}

#[derive(Debug)]
pub enum SttInput {
    Begin { recording_id: Uuid },
    Frame(Frame),
    End { recording_id: Uuid, samples: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SttEvent {
    /// A connection began streaming the recording at `origin`; `gap` is the audio before it that no connection transcribed.
    Connected { recording_id: Uuid, origin: u64, gap: Option<Gap> },
    /// The open utterance changed (display only).
    Open { recording_id: Uuid, stable: String, tentative: String },
    /// A closed utterance with text: commit it.
    Utterance { recording_id: Uuid, utterance: Utterance },
    /// The recording's live transcript is finished; `gap` is its tail that no connection transcribed.
    Ended { recording_id: Uuid, gap: Option<Gap> },
    Retrying { after: Duration, reason: String },
    /// An `error` message; the connection carries on.
    ServerError(String),
    /// A 4xx at the upgrade: no further connections this session.
    Refused(String),
}

pub struct SttLink {
    pub input: mpsc::Sender<SttInput>,
    pub events: mpsc::Receiver<SttEvent>,
}

/// Starts the session's STT worker. A bad keyterm fails here, before any recording starts.
pub fn spawn(cfg: SttConfig) -> Result<SttLink> {
    let url = cfg.request_url()?;
    let (input, rx) = mpsc::channel(INPUT_QUEUE);
    let (tx, events) = mpsc::channel(256);
    let worker = Worker { cfg, url, events: tx, rec: None, epoch: None, connecting: None, retry_at: None, failures: 0, refused: false };
    tokio::spawn(worker.run(rx));
    Ok(SttLink { input, events })
}

enum ConnectError {
    Refused(String),
    Transient(String),
}

struct Epoch {
    ws: Ws,
    /// The recording sample of the first frame sent: server time 0.
    origin: Option<u64>,
    transcript: Transcript,
    sent_to: u64,
    heard_at: Instant,
}

struct Rec {
    id: Uuid,
    /// The transcript is settled up to here: closed utterances, or audio a reported gap covers.
    settled: u64,
    /// The offset the next frame should have; another one means frames were dropped before the writer.
    next: u64,
    received_to: u64,
    held: VecDeque<Frame>,
    /// Why the audio after `settled` may lack a transcript: the kind of the gap that reports it.
    cause: Option<GapKind>,
}

struct Worker {
    cfg: SttConfig,
    url: String,
    events: mpsc::Sender<SttEvent>,
    rec: Option<Rec>,
    epoch: Option<Epoch>,
    connecting: Option<JoinHandle<Result<Ws, ConnectError>>>,
    retry_at: Option<Instant>,
    failures: u32,
    refused: bool,
}

impl Worker {
    async fn run(mut self, mut input: mpsc::Receiver<SttInput>) {
        loop {
            let (retry, idle) = (self.retry_at, self.idle_deadline());
            tokio::select! {
                i = input.recv() => match i {
                    Some(i) => self.on_input(i).await,
                    None => break,
                },
                m = next_message(&mut self.epoch) => self.on_message(m).await,
                r = connected(&mut self.connecting) => {
                    self.connecting = None;
                    self.on_connect(r).await;
                }
                _ = sleep_until(retry) => {
                    self.retry_at = None;
                    self.connect();
                }
                _ = sleep_until(idle) => self.lost(GapKind::SttOffline, format!("no message from the server for {:?}", self.cfg.idle_timeout)).await,
            }
        }
        if let Some((id, to)) = self.rec.as_ref().map(|r| (r.id, r.received_to)) {
            self.end(id, to).await;
        }
    }

    fn idle_deadline(&self) -> Option<Instant> {
        self.epoch.as_ref().filter(|e| e.origin.is_some()).map(|e| e.heard_at + self.cfg.idle_timeout)
    }

    async fn emit(&self, e: SttEvent) {
        let _ = self.events.send(e).await;
    }

    async fn on_input(&mut self, i: SttInput) {
        match i {
            SttInput::Begin { recording_id } => {
                let cause = self.refused.then_some(GapKind::SttRefused);
                self.rec = Some(Rec { id: recording_id, settled: 0, next: 0, received_to: 0, held: VecDeque::new(), cause });
                self.failures = 0;
                self.connect();
            }
            SttInput::Frame(f) => self.on_frame(f).await,
            SttInput::End { recording_id, samples } => {
                if self.rec.as_ref().is_some_and(|r| r.id == recording_id) {
                    self.end(recording_id, samples).await;
                }
            }
        }
    }

    fn connect(&mut self) {
        if self.refused || self.rec.is_none() || self.epoch.is_some() || self.connecting.is_some() {
            return;
        }
        let (url, key, limit) = (self.url.clone(), self.cfg.api_key.clone(), self.cfg.connect_timeout);
        self.connecting = Some(tokio::spawn(async move {
            match tokio::time::timeout(limit, open(&url, &key)).await {
                Ok(r) => r,
                Err(_) => Err(ConnectError::Transient(format!("no transcript.created within {limit:?}"))),
            }
        }));
    }

    async fn on_connect(&mut self, r: Result<Ws, ConnectError>) {
        match r {
            Ok(ws) => {
                self.failures = 0;
                if self.rec.is_none() {
                    return;
                }
                self.epoch = Some(Epoch { ws, origin: None, transcript: Transcript::new(0), sent_to: 0, heard_at: Instant::now() });
                let held: Vec<Frame> = self.rec.as_mut().map(|r| r.held.drain(..).collect()).unwrap_or_default();
                for f in held {
                    if !self.send(f).await {
                        break;
                    }
                }
            }
            Err(ConnectError::Refused(msg)) => {
                self.refused = true;
                if let Some(r) = self.rec.as_mut() {
                    r.cause = Some(GapKind::SttRefused);
                    r.held.clear();
                }
                self.emit(SttEvent::Refused(msg)).await;
            }
            Err(ConnectError::Transient(reason)) => self.retry(reason).await,
        }
    }

    async fn retry(&mut self, reason: String) {
        let units = 1u32.checked_shl(self.failures).unwrap_or(u32::MAX).min(MAX_BACKOFF_UNITS);
        self.failures = self.failures.saturating_add(1);
        let after = self.cfg.backoff_unit * units;
        self.retry_at = Some(Instant::now() + after);
        self.emit(SttEvent::Retrying { after, reason }).await;
    }

    async fn on_frame(&mut self, f: Frame) {
        let Some(rec) = self.rec.as_mut().filter(|r| r.id == f.recording_id) else { return };
        let dropped = f.sample_offset != rec.next;
        rec.next = f.sample_offset + FRAME_SAMPLES as u64;
        rec.received_to = f.sample_offset + f.valid_samples as u64;
        if dropped {
            rec.held.clear(); // what is held must stay contiguous
            rec.cause.get_or_insert(GapKind::SttOverflow);
            if self.epoch.is_some() {
                self.lost(GapKind::SttOverflow, format!("frames before sample {} did not reach the writer", f.sample_offset)).await;
            }
        }
        if self.refused {
            return;
        }
        if self.epoch.is_some() {
            self.send(f).await;
            return;
        }
        let Some(rec) = self.rec.as_mut() else { return };
        if rec.held.len() == HOLD_FRAMES {
            rec.held.pop_front();
        }
        rec.held.push_back(f);
    }

    /// Sends one frame on the live connection; the first fixes the epoch's origin. False if the connection was lost.
    async fn send(&mut self, f: Frame) -> bool {
        let (Some(rec), Some(epoch)) = (self.rec.as_mut(), self.epoch.as_mut()) else { return false };
        let mut started = None;
        if epoch.origin.is_none() {
            let origin = f.sample_offset;
            let gap = (origin > rec.settled).then(|| Gap::new(rec.id, rec.settled, Some(origin), rec.cause.unwrap_or(GapKind::SttOffline)));
            rec.cause = None;
            rec.settled = rec.settled.max(origin);
            epoch.origin = Some(origin);
            epoch.transcript = Transcript::new(origin);
            epoch.heard_at = Instant::now();
            started = Some(SttEvent::Connected { recording_id: rec.id, origin, gap });
        }
        let bytes: Vec<u8> = f.pcm().iter().flat_map(|s| s.to_le_bytes()).collect();
        let sent = tokio::time::timeout(self.cfg.send_timeout, epoch.ws.send(Message::Binary(bytes.into()))).await;
        if matches!(sent, Ok(Ok(()))) {
            epoch.sent_to = f.sample_offset + f.valid_samples as u64;
        }
        if let Some(e) = started {
            self.emit(e).await;
        }
        match sent {
            Ok(Ok(())) => true,
            Ok(Err(e)) => {
                self.lost(GapKind::SttOffline, e.to_string()).await;
                false
            }
            Err(_) => {
                self.lost(GapKind::SttOffline, format!("a frame took over {:?} to send", self.cfg.send_timeout)).await;
                false
            }
        }
    }

    /// The connection is gone: what it had not closed waits for the next gap; reconnect after backoff.
    async fn lost(&mut self, cause: GapKind, reason: String) {
        if self.epoch.take().is_none() {
            return;
        }
        if let Some(r) = self.rec.as_mut() {
            r.cause.get_or_insert(cause);
        }
        self.retry(reason).await;
    }

    async fn on_message(&mut self, m: Option<Result<Message, tungstenite::Error>>) {
        if let Some(e) = self.epoch.as_mut() {
            e.heard_at = Instant::now();
        }
        match m {
            Some(Ok(Message::Text(t))) => match protocol::parse(&t) {
                Ok(ServerMsg::Partial(p)) => self.on_partial(&p).await,
                Ok(ServerMsg::Error { message }) => self.emit(SttEvent::ServerError(message)).await,
                Ok(_) => {}
                Err(e) => self.emit(SttEvent::ServerError(format!("unreadable server message: {e}"))).await,
            },
            Some(Ok(Message::Close(_))) | None => self.lost(GapKind::SttOffline, "the server closed the connection".into()).await,
            Some(Err(e)) => self.lost(GapKind::SttOffline, e.to_string()).await,
            Some(Ok(_)) => {}
        }
    }

    async fn on_partial(&mut self, p: &Partial) {
        let (Some(rec), Some(epoch)) = (self.rec.as_mut(), self.epoch.as_mut()) else { return };
        if epoch.origin.is_none() {
            return;
        }
        let Some(update) = epoch.transcript.apply(p) else { return };
        let recording_id = rec.id;
        let event = match update {
            Update::Open { stable, tentative } => SttEvent::Open { recording_id, stable, tentative },
            Update::Closed(u) => {
                rec.settled = rec.settled.max(u.end_sample);
                if u.text.is_empty() {
                    SttEvent::Open { recording_id, stable: String::new(), tentative: String::new() }
                } else {
                    SttEvent::Utterance { recording_id, utterance: u }
                }
            }
        };
        self.emit(event).await;
    }

    /// The recording ended: flush what the server holds, then report the tail no connection transcribed.
    async fn end(&mut self, recording_id: Uuid, samples: u64) {
        if let Some(h) = self.connecting.take() {
            h.abort();
        }
        self.retry_at = None;
        if self.epoch.as_ref().is_some_and(|e| e.origin.is_some()) {
            self.finish().await;
        }
        self.epoch = None;
        let Some(rec) = self.rec.take() else { return };
        let cause = rec.cause.unwrap_or(if samples > rec.received_to { GapKind::SttOverflow } else { GapKind::SttOffline });
        let gap = (samples > rec.settled + SERVER_RESOLUTION).then(|| Gap::new(recording_id, rec.settled, Some(samples), cause));
        self.emit(SttEvent::Ended { recording_id, gap }).await;
    }

    /// Sends `audio.done` and reads the flushed finals until `transcript.done`.
    async fn finish(&mut self) {
        let deadline = Instant::now() + self.cfg.done_wait;
        let Some(epoch) = self.epoch.as_mut() else { return };
        if !matches!(tokio::time::timeout(self.cfg.send_timeout, epoch.ws.send(Message::Text(AUDIO_DONE.into()))).await, Ok(Ok(()))) {
            return;
        }
        loop {
            let Ok(m) = tokio::time::timeout_at(deadline, next_message(&mut self.epoch)).await else { return };
            match m {
                Some(Ok(Message::Text(t))) => match protocol::parse(&t) {
                    Ok(ServerMsg::Done { duration }) => {
                        let origin = self.epoch.as_ref().and_then(|e| e.origin);
                        if let (Some(rec), Some(origin)) = (self.rec.as_mut(), origin) {
                            rec.settled = rec.settled.max(origin + to_samples(duration));
                        }
                        return;
                    }
                    Ok(ServerMsg::Partial(p)) => self.on_partial(&p).await,
                    Ok(ServerMsg::Error { message }) => self.emit(SttEvent::ServerError(message)).await,
                    _ => {}
                },
                Some(Ok(_)) => {}
                _ => return, // closed or failed before transcript.done: the tail becomes a gap
            }
        }
    }
}

async fn open(url: &str, key: &str) -> Result<Ws, ConnectError> {
    let mut req = url.into_client_request().map_err(|e| ConnectError::Refused(format!("bad STT URL: {e}")))?;
    let auth = format!("Bearer {key}").parse().map_err(|_| ConnectError::Refused("the API key is not a valid header value".into()))?;
    req.headers_mut().insert("Authorization", auth);
    let mut ws = match tokio_tungstenite::connect_async(req).await {
        Ok((ws, _)) => ws,
        Err(tungstenite::Error::Http(resp)) => {
            let status = resp.status();
            let body = resp.body().as_deref().map(String::from_utf8_lossy).unwrap_or_default();
            let msg = format!("{status}: {}", refusal_message(&body));
            return Err(if is_refusal(status.as_u16()) { ConnectError::Refused(msg) } else { ConnectError::Transient(msg) });
        }
        Err(e) => return Err(ConnectError::Transient(e.to_string())),
    };
    while let Some(m) = ws.next().await {
        match m {
            Ok(Message::Text(t)) => {
                if let Ok(ServerMsg::Created) = protocol::parse(&t) {
                    return Ok(ws);
                }
            }
            Ok(_) => {}
            Err(e) => return Err(ConnectError::Transient(e.to_string())),
        }
    }
    Err(ConnectError::Transient("closed before transcript.created".into()))
}

async fn next_message(epoch: &mut Option<Epoch>) -> Option<Result<Message, tungstenite::Error>> {
    match epoch {
        Some(e) => e.ws.next().await,
        None => std::future::pending().await,
    }
}

async fn connected(h: &mut Option<JoinHandle<Result<Ws, ConnectError>>>) -> Result<Ws, ConnectError> {
    match h {
        Some(h) => h.await.unwrap_or_else(|e| Err(ConnectError::Transient(format!("the connect task failed: {e}")))),
        None => std::future::pending().await,
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core stream::tests` → 1 passed. Run: `cargo test -p lecturelive-core --test stt_stream` → 10 passed. Run each three times (`for i in 1 2 3; do cargo test -q -p lecturelive-core --test stt_stream || break; done`): the tests pace real time, so a flake here is a finding, not noise. Record the runs in the ledger.

- [ ] **Step 6: Commit**

```bash
git add crates/core/Cargo.toml Cargo.lock crates/core/src/stt/stream.rs crates/core/src/stt/mod.rs crates/core/tests/stt_stream.rs
git commit -m "Add the STT worker: ordered writer, epochs, live-first reconnect and gaps

Each connection is an epoch whose origin is its first frame; a new
recording gets a new connection. Frames arriving while connecting are
held (5 s) and sent first. A close, a failed or stalled send, five silent
seconds or dropped frames end the epoch, and the audio after the last
close becomes a gap reported with the next origin. A 4xx at the upgrade
stops STT for the session. At the end of a recording audio.done flushes
the server and the untranscribed tail is reported. reqwest 0.13.5 is
added for URL encoding, without a TLS provider of its own."
```

---
### Task 6: Snapshot cutoff (milestone task 4)

**Files:**
- Modify: `crates/core/src/stt/stream.rs`, `crates/core/tests/stt_stream.rs`
- Create: `crates/core/tests/stt_live.rs`

**Interfaces:**
- Consumes: Task 5's worker.
- Produces:
  - `SttConfig.finalize_wait: Duration` (3 s, spec §5.3)
  - `SttInput::Cutoff { id: u64, recording_id: Uuid, sample: u64 }`: `sample` is the end of the last frame forwarded before it.
  - `SttEvent::Cutoff { id: u64, confirmed: bool }`: emitted after the `Utterance` that settled it.

Rule: if the recording is already settled through the cutoff (within 1 ms), it is confirmed at once, and no finalize is sent. If the live connection has sent every frame through the cutoff, `{"type":"finalize"}` goes out, and the first close that settles the recording through the cutoff confirms it. Otherwise the cutoff is unconfirmed at once: its audio is not on this connection and is left to a gap. It is also unconfirmed after `finalize_wait`, or when the connection is lost first. At the end of the recording a pending cutoff is confirmed exactly when the recording is settled through it.

- [ ] **Step 1: Write the failing tests**

Append to `crates/core/tests/stt_stream.rs` (add `use lecturelive_core::stt::protocol::{AUDIO_DONE, FINALIZE};` to its imports):

```rust
async fn until_cutoff(link: &mut SttLink, id: u64) -> (Vec<SttEvent>, bool) {
    let mut seen = Vec::new();
    loop {
        let e = next_event(link).await;
        if let SttEvent::Cutoff { id: got, confirmed } = e {
            if got == id {
                return (seen, confirmed);
            }
        }
        seen.push(e);
    }
}

async fn cutoff(link: &SttLink, id: Uuid, cut: u64, sample: u64) {
    link.input.send(SttInput::Cutoff { id: cut, recording_id: id, sample }).await.unwrap();
}

#[tokio::test]
async fn a_cutoff_finalizes_after_its_frame_and_the_covering_final_confirms_it() {
    let fake = fake_stt::start(Config { mode: Mode::Replay(fixtures::load("finalize_json")), ..Default::default() }).await;
    let mut link = spawn(SttConfig { finalize_wait: ms(2_000), ..cfg(&fake) }).unwrap();
    let id = begin(&link).await;
    fake.state.wait_accepted(1).await;
    let frames = silent_frames(id, 137_838);
    feed(&link, &fake, frames[..30].to_vec()).await;
    cutoff(&link, id, 7, 48_000).await;
    feed(&link, &fake, frames[30..].to_vec()).await; // the recorded final came while these went out
    end(&link, id, 137_838).await;
    let ev = until_ended(&mut link).await;
    assert_eq!(
        spans(&ev),
        vec![
            (16, 48_000, "Transfer learning reuses pre-trained weights. The".to_string()),
            (48_000, 137_840, "losses cross entropy over the vocabulary, gradient descent updates the weights after every batch.".to_string()),
        ]
    );
    let at = |want: &dyn Fn(&SttEvent) -> bool| ev.iter().position(|e| want(e)).unwrap();
    let first = at(&|e| matches!(e, SttEvent::Utterance { utterance, .. } if utterance.end_sample == 48_000));
    let confirmed = at(&|e| *e == SttEvent::Cutoff { id: 7, confirmed: true });
    let second = at(&|e| matches!(e, SttEvent::Utterance { utterance, .. } if utterance.end_sample == 137_840));
    assert!(first < confirmed && confirmed < second, "{ev:?}");
    assert_eq!(fake.state.texts(), vec![FINALIZE.to_string(), AUDIO_DONE.to_string()]);
    assert_eq!(ev.last(), Some(&SttEvent::Ended { recording_id: id, gap: None }));
}

#[tokio::test]
async fn a_cutoff_without_a_covering_final_times_out_and_stays_pending() {
    let fake = fake_stt::start(Config { mode: Mode::Replay(fixtures::load("finalize_text")), ..Default::default() }).await;
    let mut link = spawn(SttConfig { finalize_wait: ms(300), ..cfg(&fake) }).unwrap();
    let id = begin(&link).await;
    fake.state.wait_accepted(1).await;
    let frames = silent_frames(id, 137_838);
    feed(&link, &fake, frames[..30].to_vec()).await;
    cutoff(&link, id, 1, 48_000).await;
    let (_, confirmed) = until_cutoff(&mut link, 1).await;
    assert!(!confirmed, "the recorded server never finalized: the cutoff times out");
    feed(&link, &fake, frames[30..].to_vec()).await;
    end(&link, id, 137_838).await;
    let ev = until_ended(&mut link).await;
    assert_eq!(
        spans(&ev),
        vec![(16, 137_840, "Transfer learning reuses pre-trained weights; the losses cross entropy over the vocabulary, gradient descent updates the weights after every batch.".to_string())]
    );
    assert!(ev.contains(&SttEvent::ServerError("Invalid message: expected ident at line 1 column 2".into())));
}

#[tokio::test]
async fn a_finalize_between_the_final_pair_is_confirmed_at_the_cutoff() {
    let fake = fake_stt::start(Config { mode: Mode::Replay(fixtures::load("finalize_between_pair")), ..Default::default() }).await;
    let mut link = spawn(SttConfig { finalize_wait: ms(2_000), ..cfg(&fake) }).unwrap();
    let id = begin(&link).await;
    fake.state.wait_accepted(1).await;
    let frames = silent_frames(id, 167_615);
    feed(&link, &fake, frames[..28].to_vec()).await;
    cutoff(&link, id, 2, 44_800).await;
    feed(&link, &fake, frames[28..].to_vec()).await;
    end(&link, id, 167_615).await;
    let ev = until_ended(&mut link).await;
    assert!(ev.contains(&SttEvent::Cutoff { id: 2, confirmed: true }));
    assert_eq!(spans(&ev)[0], (16, 44_800, "Gradient descent updates the weights.".to_string()));
    assert_eq!(spans(&ev).len(), 3);
}

#[tokio::test]
async fn a_cutoff_in_silence_is_confirmed_by_an_empty_final() {
    let fake = fake_stt::start(Config { mode: Mode::Replay(fixtures::load("finalize_silence")), ..Default::default() }).await;
    let mut link = spawn(SttConfig { finalize_wait: ms(2_000), ..cfg(&fake) }).unwrap();
    let id = begin(&link).await;
    fake.state.wait_accepted(1).await;
    let frames = silent_frames(id, 49_598);
    feed(&link, &fake, frames[..15].to_vec()).await;
    cutoff(&link, id, 3, 24_000).await;
    feed(&link, &fake, frames[15..].to_vec()).await;
    end(&link, id, 49_598).await;
    let ev = until_ended(&mut link).await;
    assert!(ev.contains(&SttEvent::Cutoff { id: 3, confirmed: true }));
    assert!(utterances(&ev).is_empty(), "an empty final commits nothing");
    assert_eq!(ev.last(), Some(&SttEvent::Ended { recording_id: id, gap: None }));
}

#[tokio::test]
async fn a_cutoff_already_settled_is_confirmed_without_a_finalize() {
    let fake = fake_stt::start(Config::default()).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..50).map(|k| speech_frame(id, k))).await;
    while !matches!(next_event(&mut link).await, SttEvent::Utterance { .. }) {}
    cutoff(&link, id, 4, 64_000).await; // utterance 0 closed at 65_600
    let (_, confirmed) = until_cutoff(&mut link, 4).await;
    assert!(confirmed);
    assert!(fake.state.texts().is_empty(), "no finalize was needed");
}

#[tokio::test]
async fn a_cutoff_while_offline_stays_pending() {
    let fake = fake_stt::start(Config { refuse_all: Some(Refusal::TcpClose), ..Default::default() }).await;
    let mut link = spawn(cfg(&fake)).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..10).map(|k| speech_frame(id, k))).await;
    cutoff(&link, id, 5, 16_000).await;
    let (_, confirmed) = until_cutoff(&mut link, 5).await;
    assert!(!confirmed);
}

#[tokio::test]
async fn a_disconnect_during_the_flush_leaves_the_cutoff_pending() {
    let fake = fake_stt::start(Config { drop_on_finalize: true, ..Default::default() }).await;
    let mut link = spawn(SttConfig { finalize_wait: ms(2_000), ..cfg(&fake) }).unwrap();
    let id = begin(&link).await;
    feed(&link, &fake, (0..60).map(|k| speech_frame(id, k))).await;
    cutoff(&link, id, 6, 96_000).await;
    let (seen, confirmed) = until_cutoff(&mut link, 6).await;
    assert!(!confirmed);
    feed(&link, &fake, (60..100).map(|k| speech_frame(id, k))).await;
    end(&link, id, 100 * 1600).await;
    let mut ev = seen;
    ev.extend(until_ended(&mut link).await);
    assert!(ev.iter().any(|e| matches!(e, SttEvent::Retrying { .. })));
    assert_partition(&ev, 100 * 1600);
}
```

Create `crates/core/tests/stt_live.rs` (run by hand; never in the default suite):

```rust
//! Live checks against api.x.ai (spec §5), with synthesised speech only, a few seconds each.
//! GROK_API_KEY comes from the environment or the repository's .env:
//!     cargo test -p lecturelive-core --test stt_live -- --ignored --nocapture
use std::time::Duration;

use lecturelive_core::audio::frame::Frame;
use lecturelive_core::session::sidecar::{Gap, GapKind};
use lecturelive_core::stt::stream::{spawn, SttConfig, SttEvent, SttInput, SttLink};
use uuid::Uuid;

fn api_key() -> String {
    if let Ok(k) = std::env::var("GROK_API_KEY") {
        return k;
    }
    let env = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env")).expect("GROK_API_KEY in the environment or the repository's .env");
    env.lines().find_map(|l| l.strip_prefix("GROK_API_KEY=")).map(|v| v.trim().trim_matches('"').to_string()).expect("GROK_API_KEY in .env")
}

fn speech_wav() -> Vec<i16> {
    hound::WavReader::open(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stt/speech.wav")).unwrap().samples::<i16>().map(|s| s.unwrap()).collect()
}

async fn next_event(link: &mut SttLink) -> SttEvent {
    tokio::time::timeout(Duration::from_secs(30), link.events.recv()).await.expect("an event within 30 s").expect("the worker is running")
}

async fn until_ended(link: &mut SttLink) -> Vec<SttEvent> {
    let mut seen = Vec::new();
    loop {
        let e = next_event(link).await;
        println!("{e:?}");
        let ended = matches!(e, SttEvent::Ended { .. });
        seen.push(e);
        if ended {
            return seen;
        }
    }
}

#[tokio::test]
#[ignore]
async fn live_stream_confirms_a_cutoff_and_flushes_at_the_end() {
    let pcm = speech_wav();
    let mut link = spawn(SttConfig::new(api_key(), vec!["cross-entropy".into()])).unwrap();
    let id = Uuid::new_v4();
    link.input.send(SttInput::Begin { recording_id: id }).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await; // let the handshake finish, so the stream starts at sample 0
    for (k, chunk) in pcm.chunks(1600).enumerate() {
        let mut pcm16 = [0i16; 1600];
        pcm16[..chunk.len()].copy_from_slice(chunk);
        let frame = Frame { recording_id: id, sample_offset: k as u64 * 1600, valid_samples: chunk.len() as u32, pcm16 };
        link.input.send(SttInput::Frame(frame)).await.unwrap();
        if k + 1 == 30 {
            link.input.send(SttInput::Cutoff { id: 1, recording_id: id, sample: 48_000 }).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(20)).await; // five times real time
    }
    link.input.send(SttInput::End { recording_id: id, samples: pcm.len() as u64 }).await.unwrap();
    let ev = until_ended(&mut link).await;
    assert!(matches!(ev.first(), Some(SttEvent::Connected { origin: 0, gap: None, .. })), "{:?}", ev.first());
    assert!(ev.contains(&SttEvent::Cutoff { id: 1, confirmed: true }));
    let u: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            SttEvent::Utterance { utterance, .. } => Some(utterance.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(u.len(), 2, "{u:?}");
    assert_eq!(u[0].end_sample, 48_000, "the finalize closes exactly at the cutoff");
    assert!(u[1].text.to_lowercase().contains("gradient descent"), "{}", u[1].text);
    assert_eq!(ev.last(), Some(&SttEvent::Ended { recording_id: id, gap: None }));
}

#[tokio::test]
#[ignore]
async fn live_refusal_of_a_bad_key_stops_stt() {
    let mut link = spawn(SttConfig::new("xai-not-a-real-key".into(), vec![])).unwrap();
    let id = Uuid::new_v4();
    link.input.send(SttInput::Begin { recording_id: id }).await.unwrap();
    link.input.send(SttInput::Frame(Frame { recording_id: id, sample_offset: 0, valid_samples: 1600, pcm16: [0; 1600] })).await.unwrap();
    loop {
        match next_event(&mut link).await {
            SttEvent::Refused(msg) => {
                println!("{msg}");
                assert!(msg.starts_with("400 Bad Request: Incorrect API key"), "{msg}");
                break;
            }
            SttEvent::Retrying { reason, .. } => panic!("a bad key must not be retried: {reason}"),
            _ => {}
        }
    }
    link.input.send(SttInput::End { recording_id: id, samples: 1600 }).await.unwrap();
    let ev = until_ended(&mut link).await;
    assert_eq!(ev.last(), Some(&SttEvent::Ended { recording_id: id, gap: Some(Gap::new(id, 0, Some(1600), GapKind::SttRefused)) }));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core --test stt_stream cutoff` (and `--test stt_live --no-run`)
Expected: compile errors (`SttInput::Cutoff`, `finalize_wait` not found).

- [ ] **Step 3: Implement**

In `crates/core/src/stt/stream.rs`:

Import `FINALIZE` from `super::protocol`. Add to `SttConfig` (after `idle_timeout`) and to `SttConfig::new` (`finalize_wait: Duration::from_secs(3)`):

```rust
    /// How long a cutoff waits for the final that settles it (spec §5.3).
    pub finalize_wait: Duration,
```

Add to `SttInput` and `SttEvent`:

```rust
    /// Finalize after the frames through `sample`, the end of the last frame forwarded before this (spec §5.3).
    Cutoff { id: u64, recording_id: Uuid, sample: u64 },
```

```rust
    /// The cutoff is settled: confirmed when the recording's transcript is committed through it.
    Cutoff { id: u64, confirmed: bool },
```

Add a pending-cutoff list to `Epoch` (`cutoffs: Vec<PendingCutoff>`, initialised to `Vec::new()` in `on_connect`) and the type:

```rust
struct PendingCutoff {
    id: u64,
    sample: u64,
    deadline: Instant,
}
```

In `run`, compute `let cutoff = self.cutoff_deadline();` with the other deadlines and add the branch:

```rust
                _ = sleep_until(cutoff) => self.expire_cutoffs().await,
```

In `on_input`, add the arm:

```rust
            SttInput::Cutoff { id, recording_id, sample } => self.cutoff(id, recording_id, sample).await,
```

Replace `on_partial` and `lost`, and in `end` replace the line `self.epoch = None;` with the settling block:

```rust
    async fn on_partial(&mut self, p: &Partial) {
        let (Some(rec), Some(epoch)) = (self.rec.as_mut(), self.epoch.as_mut()) else { return };
        if epoch.origin.is_none() {
            return;
        }
        let Some(update) = epoch.transcript.apply(p) else { return };
        let recording_id = rec.id;
        let mut out = Vec::new();
        match update {
            Update::Open { stable, tentative } => out.push(SttEvent::Open { recording_id, stable, tentative }),
            Update::Closed(u) => {
                rec.settled = rec.settled.max(u.end_sample);
                let settled = rec.settled;
                out.push(if u.text.is_empty() {
                    SttEvent::Open { recording_id, stable: String::new(), tentative: String::new() }
                } else {
                    SttEvent::Utterance { recording_id, utterance: u }
                });
                // A cutoff is confirmed by the close that settles the recording through it.
                epoch.cutoffs.retain(|c| {
                    let done = c.sample <= settled + SERVER_RESOLUTION;
                    if done {
                        out.push(SttEvent::Cutoff { id: c.id, confirmed: true });
                    }
                    !done
                });
            }
        }
        for e in out {
            self.emit(e).await;
        }
    }

    /// The connection is gone: what it had not closed waits for the next gap, and so do its cutoffs.
    async fn lost(&mut self, cause: GapKind, reason: String) {
        let Some(epoch) = self.epoch.take() else { return };
        if let Some(r) = self.rec.as_mut() {
            r.cause.get_or_insert(cause);
        }
        for c in epoch.cutoffs {
            self.emit(SttEvent::Cutoff { id: c.id, confirmed: false }).await;
        }
        self.retry(reason).await;
    }
```

```rust
        if let Some(epoch) = self.epoch.take() {
            let settled = self.rec.as_ref().map_or(0, |r| r.settled);
            for c in epoch.cutoffs {
                self.emit(SttEvent::Cutoff { id: c.id, confirmed: c.sample <= settled + SERVER_RESOLUTION }).await;
            }
        }
```

Add to `impl Worker`:

```rust
    fn cutoff_deadline(&self) -> Option<Instant> {
        self.epoch.as_ref().and_then(|e| e.cutoffs.iter().map(|c| c.deadline).min())
    }

    async fn cutoff(&mut self, id: u64, recording_id: Uuid, sample: u64) {
        let settled = match self.rec.as_ref().filter(|r| r.id == recording_id) {
            Some(r) => r.settled,
            None => return self.emit(SttEvent::Cutoff { id, confirmed: false }).await,
        };
        if sample <= settled + SERVER_RESOLUTION {
            return self.emit(SttEvent::Cutoff { id, confirmed: true }).await;
        }
        let (deadline, send_timeout) = (Instant::now() + self.cfg.finalize_wait, self.cfg.send_timeout);
        let Some(epoch) = self.epoch.as_mut().filter(|e| e.origin.is_some() && e.sent_to >= sample) else {
            // The cutoff's audio is not on this connection: it is left to a gap.
            return self.emit(SttEvent::Cutoff { id, confirmed: false }).await;
        };
        epoch.cutoffs.push(PendingCutoff { id, sample, deadline });
        let sent = tokio::time::timeout(send_timeout, epoch.ws.send(Message::Text(FINALIZE.into()))).await;
        if !matches!(sent, Ok(Ok(()))) {
            self.lost(GapKind::SttOffline, "the finalize could not be sent".into()).await;
        }
    }

    async fn expire_cutoffs(&mut self) {
        let now = Instant::now();
        let Some(epoch) = self.epoch.as_mut() else { return };
        let mut expired = Vec::new();
        epoch.cutoffs.retain(|c| {
            if c.deadline <= now {
                expired.push(c.id);
            }
            c.deadline > now
        });
        for id in expired {
            self.emit(SttEvent::Cutoff { id, confirmed: false }).await;
        }
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core --test stt_stream` → 17 passed, three runs in a row. Then run the live checks (authorised; about 10 s of audio): `cargo test -p lecturelive-core --test stt_live -- --ignored --nocapture` → 2 passed. Record the printed events in the ledger. If a live assertion fails on a fact about the server (for example the finalize final not ending exactly at 48 000), record the observation as a Ruling before changing anything.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/stt/stream.rs crates/core/tests/stt_stream.rs crates/core/tests/stt_live.rs
git commit -m "Finalize at snapshot cutoffs and correlate the final that settles them

The writer sends finalize after the frames through the cutoff and waits
for the close that settles the recording through it; the server always
answers a finalize with a speech_final ending at the cutoff, empty when
nothing is open. A cutoff already settled is confirmed at once, and one
whose audio is not on the live connection, or that times out, stays
pending. Live checks against the real endpoint are kept as ignored tests."
```

---

### Task 7: Segment log, transcript file, and the crash gap at launch (milestone task 3)

**Files:**
- Create: `crates/core/src/session/segments.rs`
- Modify: `crates/core/src/session/mod.rs`, `crates/core/src/session/launch.rs`

**Interfaces:**
- Consumes: `transcript::Word` (Task 2), `sidecar::{wall_time_at, OpenUtterance, Gap::new, GapKind::SttInterrupted}` (Task 3).
- Produces:
  - `session::segments::{Segment, SegmentSource::{Live, Recovered}, NewSegment, SegmentLog, segments_path, transcript_path, transcript_line, read}`
  - `Segment { id: u64, recording_id, start_sample, end_sample, said_at, start, end: DateTime<Local>, text, words: Vec<Word>, source }` (serde; one JSON line each)
  - `NewSegment { recording_id, start_sample, end_sample, text, words, source }`
  - `SegmentLog::{open(dir, stem) -> Result<Self>, len(&self) -> u64, is_empty, append(&mut self, NewSegment, anchor: DateTime<Local>) -> Result<Segment>, last_end(&self, Uuid) -> Option<u64>, committed_within(&self, Uuid, from, to) -> Option<u64>}`
  - `read(&Path) -> Result<Vec<Segment>>` (skips a torn last line; a corrupt complete line is an error)
  - `LaunchReport.untranscribed: Vec<Gap>`

A segment's `said_at` is the wall time of its first word, or of `start_sample` when it has no words, and it is the transcript line's time. `append` writes the log line and syncs it, then the transcript line and syncs it. After a crash between the two, the log holds a segment whose line is missing, never a line whose segment is missing: a missing line costs one line of a human file, while a line whose segment was lost would be recovered and written a second time.

- [ ] **Step 1: Write the failing tests**

Create `crates/core/src/session/segments.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::io::Write;

    const STEM: &str = "lecture_notes_20260925";

    fn anchor() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 0).unwrap()
    }

    fn word(text: &str, s: u64, e: u64) -> Word {
        Word { text: text.into(), start_sample: s, end_sample: e }
    }

    fn seg(rec: Uuid, s: u64, e: u64, text: &str, words: Vec<Word>, source: SegmentSource) -> NewSegment {
        NewSegment { recording_id: rec, start_sample: s, end_sample: e, text: text.into(), words, source }
    }

    #[test]
    fn segments_are_durable_lines_in_commit_order_with_cli_transcript_lines() {
        let dir = tempfile::tempdir().unwrap();
        let rec = Uuid::new_v4();
        let mut log = SegmentLog::open(dir.path(), STEM).unwrap();
        let a = log.append(seg(rec, 16, 48_000, "Transfer learning", vec![word("Transfer", 336, 7_744), word("learning", 8_704, 13_216)], SegmentSource::Live), anchor()).unwrap();
        let b = log.append(seg(rec, 48_000, 96_000, "losses", vec![], SegmentSource::Recovered), anchor()).unwrap();
        assert_eq!((a.id, b.id, log.len()), (0, 1, 2));
        assert_eq!(a.said_at, anchor() + chrono::Duration::microseconds(21_000));
        assert_eq!(b.said_at, anchor() + chrono::Duration::seconds(3));
        assert_eq!(read(&segments_path(dir.path(), STEM)).unwrap(), vec![a, b]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("lecture_transcript_20260925.txt")).unwrap(),
            "[10:00:00] Transfer learning\n[10:00:03] losses\n"
        );
    }

    #[test]
    fn a_line_cut_short_by_a_crash_is_dropped_and_appending_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let rec = Uuid::new_v4();
        let mut log = SegmentLog::open(dir.path(), STEM).unwrap();
        log.append(seg(rec, 0, 16_000, "one", vec![], SegmentSource::Live), anchor()).unwrap();
        drop(log);
        let path = segments_path(dir.path(), STEM);
        OpenOptions::new().append(true).open(&path).unwrap().write_all(br#"{"id":1,"recording_id":"#).unwrap();
        assert_eq!(read(&path).unwrap().len(), 1, "a torn line is skipped on read");
        let mut log = SegmentLog::open(dir.path(), STEM).unwrap();
        assert_eq!(log.len(), 1);
        log.append(seg(rec, 16_000, 32_000, "two", vec![], SegmentSource::Live), anchor()).unwrap();
        let back = read(&path).unwrap();
        assert_eq!(back.iter().map(|s| (s.id, s.text.as_str())).collect::<Vec<_>>(), vec![(0, "one"), (1, "two")]);
    }

    #[test]
    fn a_corrupt_complete_line_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = segments_path(dir.path(), STEM);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{not json}\n").unwrap();
        let err = SegmentLog::open(dir.path(), STEM).err().unwrap();
        assert!(format!("{err:#}").contains("corrupt line 1"), "{err:#}");
    }

    #[test]
    fn recovery_bounds_come_from_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut log = SegmentLog::open(dir.path(), STEM).unwrap();
        for (rec, s, e) in [(a, 0, 16_000), (a, 40_000, 56_000), (a, 56_000, 70_000), (b, 0, 8_000)] {
            log.append(seg(rec, s, e, "x", vec![], SegmentSource::Live), anchor()).unwrap();
        }
        assert_eq!(log.last_end(a), Some(70_000));
        assert_eq!(log.last_end(Uuid::new_v4()), None);
        assert_eq!(log.committed_within(a, 40_000, 100_000), Some(70_000));
        assert_eq!(log.committed_within(a, 16_000, 40_000), None);
        assert_eq!(log.committed_within(b, 0, 100_000), Some(8_000));
    }

    #[test]
    fn the_files_take_the_cli_names() {
        let dir = Path::new("/lecture");
        assert_eq!(transcript_path(dir, STEM), Path::new("/lecture/lecture_transcript_20260925.txt"));
        assert_eq!(segments_path(dir, STEM), Path::new("/lecture/.live_notes/lecture_notes_20260925.segments.jsonl"));
    }
}
```

Append to the test module of `crates/core/src/session/launch.rs` (and add `use crate::session::sidecar::OpenUtterance;` and `use crate::session::segments::{NewSegment, SegmentLog, SegmentSource};` to its imports):

```rust
    fn log_segment(dir: &Path, rec: Uuid, start: u64, end: u64) {
        let mut log = SegmentLog::open(dir, STEM).unwrap();
        let s = NewSegment { recording_id: rec, start_sample: start, end_sample: end, text: "x".into(), words: vec![], source: SegmentSource::Live };
        log.append(s, now()).unwrap();
    }

    #[test]
    fn a_crash_turns_the_open_utterance_after_the_last_segment_into_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let file = crashed_recording(dir.path());
        let id = Uuid::new_v4();
        let mut sc = Sidecar::default();
        sc.recordings.push(RecordingEntry { samples: None, ..entry(id, &file, 0, RecState::Open) });
        sc.open_utterance = Some(OpenUtterance { recording_id: id, from_sample: 0 });
        let path = sidecar_path(dir.path(), STEM);
        sc.save(&path).unwrap();
        log_segment(dir.path(), id, 16, 12_000);

        let report = recover(dir.path(), Retention::KeepAll, now()).unwrap();
        let len = report.repaired[0].1;
        let gap = Gap::new(id, 12_000, Some(len), GapKind::SttInterrupted);
        assert_eq!(report.untranscribed, vec![gap.clone()]);
        let sc = Sidecar::load(&path).unwrap().unwrap();
        assert_eq!(sc.gaps, vec![Gap::new(id, len, None, GapKind::Interrupted), gap]);
        assert_eq!(sc.open_utterance, None);
        assert!(recover(dir.path(), Retention::KeepAll, now()).unwrap().untranscribed.is_empty(), "a second launch adds nothing");
    }

    #[test]
    fn an_open_utterance_on_a_finalized_recording_gets_its_tail_from_the_later_of_marker_and_log() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let mut sc = Sidecar::default();
        sc.recordings.push(RecordingEntry { samples: Some(160_000), ..entry(id, "recordings/r.wav", 0, RecState::Finalized) });
        sc.open_utterance = Some(OpenUtterance { recording_id: id, from_sample: 96_000 });
        sc.save(&sidecar_path(dir.path(), STEM)).unwrap();
        log_segment(dir.path(), id, 16, 90_000);
        let report = recover(dir.path(), Retention::KeepAll, now()).unwrap();
        assert_eq!(report.untranscribed, vec![Gap::new(id, 96_000, Some(160_000), GapKind::SttInterrupted)]);
    }

    #[test]
    fn an_open_utterance_on_a_missing_recording_leaves_no_gap() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let mut sc = Sidecar::default();
        sc.recordings.push(entry(id, "recordings/gone.wav", 0, RecState::Open));
        sc.open_utterance = Some(OpenUtterance { recording_id: id, from_sample: 0 });
        sc.save(&sidecar_path(dir.path(), STEM)).unwrap();
        let report = recover(dir.path(), Retention::KeepAll, now()).unwrap();
        assert!(report.untranscribed.is_empty());
        let sc = Sidecar::load(&sidecar_path(dir.path(), STEM)).unwrap().unwrap();
        assert_eq!(sc.open_utterance, None);
        assert_eq!(sc.gaps.len(), 1, "only the missing recording's interrupted gap");
    }
```

Replace `crates/core/src/session/mod.rs` with:

```rust
pub mod coordinator;
pub mod launch;
pub mod lock;
pub mod segments;
pub mod sidecar;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core segments launch`
Expected: compile errors (`SegmentLog`, `untranscribed` not found).

- [ ] **Step 3: Implement**

Prepend to `crates/core/src/session/segments.rs`:

```rust
//! The segment log and the transcript file (spec §8): every committed utterance, in commit order.
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::session::sidecar::wall_time_at;
use crate::stt::transcript::Word;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentSource {
    Live,
    Recovered,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    /// Position in the log, from 0: the cursor a snapshot takes.
    pub id: u64,
    pub recording_id: Uuid,
    pub start_sample: u64,
    pub end_sample: u64,
    /// Wall time of the first word (of `start_sample` without words): the transcript line's time.
    pub said_at: DateTime<Local>,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub text: String,
    pub words: Vec<Word>,
    pub source: SegmentSource,
}

pub struct NewSegment {
    pub recording_id: Uuid,
    pub start_sample: u64,
    pub end_sample: u64,
    pub text: String,
    pub words: Vec<Word>,
    pub source: SegmentSource,
}

pub fn segments_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(".live_notes").join(format!("{stem}.segments.jsonl"))
}

/// `lecture_notes_YYYYMMDD` → `lecture_transcript_YYYYMMDD.txt`, the Python CLI's name.
pub fn transcript_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!("{}.txt", stem.replacen("lecture_notes", "lecture_transcript", 1)))
}

pub fn transcript_line(s: &Segment) -> String {
    format!("[{}] {}\n", s.said_at.format("%H:%M:%S"), s.text)
}

pub struct SegmentLog {
    log: File,
    transcript: File,
    /// Recording and interval of every logged segment, in log order.
    intervals: Vec<(Uuid, u64, u64)>,
}

impl SegmentLog {
    /// Opens the log and the transcript for appending, creating both. A last line cut short by a crash is removed.
    pub fn open(dir: &Path, stem: &str) -> Result<Self> {
        let path = segments_path(dir, stem);
        std::fs::create_dir_all(path.parent().expect("the log lives in .live_notes"))?;
        let (segments, torn_at) = read_complete(&path)?;
        let log = OpenOptions::new().create(true).append(true).open(&path).with_context(|| format!("open {}", path.display()))?;
        if let Some(len) = torn_at {
            log.set_len(len).with_context(|| format!("trim {}", path.display()))?;
        }
        let tpath = transcript_path(dir, stem);
        let transcript = OpenOptions::new().create(true).append(true).open(&tpath).with_context(|| format!("open {}", tpath.display()))?;
        Ok(Self { log, transcript, intervals: segments.iter().map(|s| (s.recording_id, s.start_sample, s.end_sample)).collect() })
    }

    pub fn len(&self) -> u64 {
        self.intervals.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.intervals.is_empty()
    }

    /// Appends the segment to the log and syncs, then its line to the transcript and syncs. `anchor` is its recording's.
    pub fn append(&mut self, s: NewSegment, anchor: DateTime<Local>) -> Result<Segment> {
        let said = s.words.first().map_or(s.start_sample, |w| w.start_sample);
        let seg = Segment {
            id: self.len(),
            recording_id: s.recording_id,
            start_sample: s.start_sample,
            end_sample: s.end_sample,
            said_at: wall_time_at(anchor, said),
            start: wall_time_at(anchor, s.start_sample),
            end: wall_time_at(anchor, s.end_sample),
            text: s.text,
            words: s.words,
            source: s.source,
        };
        let mut line = serde_json::to_vec(&seg)?;
        line.push(b'\n');
        self.log.write_all(&line)?;
        self.log.sync_data()?;
        self.transcript.write_all(transcript_line(&seg).as_bytes())?;
        self.transcript.sync_data()?;
        self.intervals.push((seg.recording_id, seg.start_sample, seg.end_sample));
        Ok(seg)
    }

    pub fn last_end(&self, recording: Uuid) -> Option<u64> {
        self.intervals.iter().filter(|(r, _, _)| *r == recording).map(|(_, _, e)| *e).max()
    }

    /// End of the last segment of `recording` that starts in [from, to): how far recovery of that gap has committed.
    pub fn committed_within(&self, recording: Uuid, from: u64, to: u64) -> Option<u64> {
        self.intervals.iter().filter(|(r, s, _)| *r == recording && (from..to).contains(s)).map(|(_, _, e)| *e).max()
    }
}

/// Every complete segment of the log; a last line without its newline (a crash mid-write) is skipped.
pub fn read(path: &Path) -> Result<Vec<Segment>> {
    Ok(read_complete(path)?.0)
}

/// The complete segments, and the length to trim the file to when its last line is torn.
fn read_complete(path: &Path) -> Result<(Vec<Segment>, Option<u64>)> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), None)),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let keep = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let segments = bytes[..keep]
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .enumerate()
        .map(|(i, l)| serde_json::from_slice(l).with_context(|| format!("corrupt line {} of {}", i + 1, path.display())))
        .collect::<Result<_>>()?;
    Ok((segments, (keep < bytes.len()).then_some(keep as u64)))
}
```

In `crates/core/src/session/launch.rs`, add `pub untranscribed: Vec<Gap>` to `LaunchReport` (doc: "Audio a crash left without a committed transcript (spec §5.2)"). Import `crate::session::segments::{self, segments_path}` and `crate::session::sidecar::OpenUtterance`. Replace the body of `recover_one` so that the save happens whenever the sidecar changed, and the open utterance is handled after repair:

```rust
fn recover_one(dir: &Path, path: &Path, retention: Retention, now: DateTime<Local>, report: &mut LaunchReport) -> Result<()> {
    let Some(mut sc) = Sidecar::load(path)? else { return Ok(()) };
    let before = sc.clone();
    let mut gaps = Vec::new();
    for r in sc.recordings.iter_mut().filter(|r| r.state == RecState::Open) {
        let wav = dir.join(&r.file);
        let start_sample = if wav.exists() {
            let samples = repair_header(&wav).with_context(|| format!("repair {}", wav.display()))? as u64;
            r.samples = Some(samples);
            r.state = RecState::Repaired;
            report.repaired.push((wav, samples));
            samples
        } else {
            r.state = RecState::Missing;
            report.missing.push(wav);
            0
        };
        gaps.push(Gap::new(r.id, start_sample, None, GapKind::Interrupted));
    }
    sc.gaps.extend(gaps);
    if let Some(open) = sc.open_utterance.take() {
        if let Some(g) = untranscribed(dir, path, &sc, open)? {
            report.untranscribed.push(g.clone());
            sc.gaps.push(g);
        }
    }
    for id in prunable(&sc, retention, now) {
        let r = sc.recording_mut(id).expect("prunable ids come from the sidecar");
        let wav = dir.join(&r.file);
        match std::fs::remove_file(&wav) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("delete {}", wav.display())),
        }
        r.state = RecState::Deleted;
        report.pruned.push(wav);
    }
    if sc != before {
        sc.save(path)?;
    }
    Ok(())
}

/// The crashed live epoch's audio after its last logged segment (spec §5.2): a gap for recovery.
fn untranscribed(dir: &Path, sidecar: &Path, sc: &Sidecar, open: OpenUtterance) -> Result<Option<Gap>> {
    let recording = sc.recordings.iter().find(|r| r.id == open.recording_id && r.state != RecState::Missing);
    let Some(len) = recording.and_then(|r| r.samples) else { return Ok(None) };
    let stem = sidecar.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".v2.json")).context("a sidecar is named <stem>.v2.json")?;
    let logged = segments::read(&segments_path(dir, stem))?.iter().filter(|s| s.recording_id == open.recording_id).map(|s| s.end_sample).max().unwrap_or(0);
    let from = open.from_sample.max(logged);
    Ok((len > from).then(|| Gap::new(open.recording_id, from, Some(len), GapKind::SttInterrupted)))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core segments launch` → all pass (5 segment tests, the M1 launch tests, 3 new launch tests).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/session/segments.rs crates/core/src/session/mod.rs crates/core/src/session/launch.rs
git commit -m "Add the segment log and transcript file; a crash's open utterance becomes a gap

Each committed utterance is one synced JSON line in <stem>.segments.jsonl
and one [HH:MM:SS] line in the transcript, timed by its first word. A
line torn by a crash is trimmed before appending. At launch, the live
epoch the sidecar still marks open becomes an stt_interrupted gap from
its last logged segment to the end of the recording."
```

---
### Task 8: REST recovery over recorded intervals (milestone task 5, recovery)

**Files:**
- Create: `crates/core/src/stt/rest.rs`, `crates/core/tests/support/fake_rest.rs`, `crates/core/tests/stt_rest.rs`
- Modify: `crates/core/src/stt/mod.rs`, `crates/core/src/audio/recorder.rs` (make `HEADER_LEN` public), `crates/core/tests/support/mod.rs`, `crates/core/tests/stt_live.rs`

**Interfaces:**
- Consumes: `protocol::{is_refusal, refusal_message, to_samples, ServerWord}` (Task 1), `transcript::Word` (Task 2), `recorder::{HEADER_LEN, SAMPLE_RATE}`.
- Produces:
  - `stt::rest::{REST_URL, RestConfig, RestTranscript, RestError, RestClient, pieces, RecoverJob, RecoverEvent, RecoveryLink, spawn_recovery}`
  - `RestConfig { url, api_key, keyterms, retry_unit, request_timeout, file_wait }`; `RestConfig::new(api_key, keyterms)` (1 s, 60 s, 10 s)
  - `RestClient::{new(RestConfig) -> Result<Self>, transcribe(&self, &[i16]) -> Result<RestTranscript, RestError>}`; `RestTranscript { text, words: Vec<ServerWord>, duration }`; `RestError::{Refused(String), Transient(String)}`
  - `pieces(&[i16]) -> Vec<Range<usize>>`
  - `RecoverJob { recording_id, gap_start, from, end, wav: PathBuf }` (Clone, PartialEq): `gap_start` names the gap, `from` skips what the log already holds
  - `RecoverEvent::{Piece { recording_id, gap_start, start_sample, end_sample, text, words: Vec<Word> }, Done { recording_id, gap_start }, Failed { recording_id, gap_start, message, refused }}`
  - `RecoveryLink { jobs: mpsc::UnboundedSender<RecoverJob>, events: mpsc::Receiver<RecoverEvent> }`; `spawn_recovery(RestClient) -> RecoveryLink`
  - test support: `fake_rest::{start(refuse: Option<u16>) -> FakeRest, FakeRest { url, state }, RestState { requests } + fields(), auth()}`

Behaviour: a job reads samples `[from, end)` of the recording, waiting up to `file_wait` for the recorder to write them, and cuts them into pieces. Each piece is one request, and each answer is one `Piece` event with word times mapped to recording samples. Its interval is the piece's, so the pieces of a gap tile it. A transient failure (network, 5xx, 408, 429) is retried with backoff while the session runs. Once the job queue is closed (the session is stopping), it fails the job instead, and the gap stays unresolved for a later session. A refusal fails the job with `refused: true` and ends the worker: recovery stops for the session.

- [ ] **Step 1: Write the failing tests**

Create `crates/core/src/stt/rest.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_intervals_are_cut_in_their_quietest_places() {
        let mut pcm = vec![1000i16; 70 * 16_000];
        let silences = [(384_000, 392_000), (800_000, 808_000)]; // 24.0–24.5 s and 50.0–50.5 s
        for (from, to) in silences {
            pcm[from..to].fill(0);
        }
        let p = pieces(&pcm);
        assert_eq!(p.len(), 3, "{p:?}");
        assert_eq!((p[0].start, p[2].end), (0, pcm.len()));
        assert!(p.windows(2).all(|w| w[0].end == w[1].start));
        assert!(p.iter().all(|r| r.len() <= 30 * 16_000));
        for (cut, (from, to)) in [p[0].end, p[1].end].into_iter().zip(silences) {
            assert!((from..to).contains(&cut), "cut at {cut}, outside {from}..{to}");
        }
    }

    #[test]
    fn short_intervals_are_one_piece_and_empty_ones_none() {
        assert_eq!(pieces(&vec![5; 16_000]), vec![0..16_000]);
        assert_eq!(pieces(&vec![5; 30 * 16_000]), vec![0..480_000]);
        assert!(pieces(&[]).is_empty());
    }

    #[test]
    fn silence_answers_without_words() {
        let t: RestTranscript = serde_json::from_str(r#"{"text":"","language":"","duration":3.1}"#).unwrap();
        assert_eq!(t, RestTranscript { text: String::new(), words: vec![], duration: 3.1 });
    }

    #[test]
    fn a_wav_is_the_recorders_format() {
        let bytes = wav_bytes(&[1, -2, 3]).unwrap();
        assert_eq!(bytes.len(), 44 + 6);
        let r = hound::WavReader::new(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!((r.spec().sample_rate, r.spec().channels, r.spec().bits_per_sample), (16_000, 1, 16));
    }
}
```

Create `crates/core/tests/stt_rest.rs`:

```rust
mod support;

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

use lecturelive_core::audio::frame::Frame;
use lecturelive_core::audio::recorder::Recorder;
use lecturelive_core::stt::protocol::ServerWord;
use lecturelive_core::stt::rest::{spawn_recovery, RecoverEvent, RecoverJob, RestClient, RestConfig, RestError};
use support::{fake_rest, speech};
use uuid::Uuid;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn cfg(url: &str) -> RestConfig {
    RestConfig { url: url.into(), retry_unit: ms(1), request_timeout: ms(5_000), file_wait: ms(2_000), ..RestConfig::new("test-key".into(), vec!["gradient descent".into()]) }
}

fn synthetic(frames: Range<u64>) -> Vec<i16> {
    frames.flat_map(speech::frame_pcm).collect()
}

fn write_frames(r: &mut Recorder, frames: Range<u64>) {
    let id = Uuid::new_v4();
    for k in frames {
        r.write_frame(&Frame { recording_id: id, sample_offset: k * 1600, valid_samples: 1600, pcm16: speech::frame_pcm(k) }).unwrap();
    }
}

fn record(dir: &Path, frames: Range<u64>) -> PathBuf {
    let mut r = Recorder::create(&dir.join("recordings"), "session_20260925_100000").unwrap();
    write_frames(&mut r, frames);
    r.finalize().unwrap()
}

async fn until_done(link: &mut lecturelive_core::stt::rest::RecoveryLink) -> Vec<(u64, u64, Vec<String>)> {
    let mut pieces = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(20), link.events.recv()).await.unwrap().unwrap() {
            RecoverEvent::Piece { start_sample, end_sample, words, .. } => pieces.push((start_sample, end_sample, words.into_iter().map(|w| w.text).collect())),
            RecoverEvent::Done { .. } => return pieces,
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn a_clip_is_posted_as_a_wav_with_the_cli_fields() {
    let fake = fake_rest::start(None).await;
    let t = RestClient::new(cfg(&fake.url)).unwrap().transcribe(&synthetic(0..40)).await.unwrap();
    assert_eq!(t.text, "w0 w1 w2 w3 w4 w5 w6 w7");
    assert_eq!(t.words[1], ServerWord { text: "w1".into(), start: 0.5, end: 0.9 });
    assert_eq!(fake.state.fields(), vec![("language".into(), "en".into()), ("format".into(), "true".into()), ("keyterm".into(), "gradient descent".into())]);
    assert_eq!(fake.state.auth(), vec!["Bearer test-key".to_string()]);
}

#[tokio::test]
async fn a_refusal_is_reported_as_one() {
    let fake = fake_rest::start(Some(400)).await;
    let err = RestClient::new(cfg(&fake.url)).unwrap().transcribe(&synthetic(0..10)).await.unwrap_err();
    assert_eq!(err, RestError::Refused("400 Bad Request: Incorrect API key provided. You can obtain an API key from https://console.x.ai.".into()));
}

#[tokio::test]
async fn recovery_reads_the_gap_from_the_recording_and_commits_it_in_pieces() {
    let dir = tempfile::tempdir().unwrap();
    let wav = record(dir.path(), 0..700);
    let fake = fake_rest::start(None).await;
    let mut link = spawn_recovery(RestClient::new(cfg(&fake.url)).unwrap());
    link.jobs.send(RecoverJob { recording_id: Uuid::new_v4(), gap_start: 80_000, from: 80_000, end: 1_120_000, wav }).unwrap();
    let pieces = until_done(&mut link).await;
    assert!(pieces.len() >= 3, "65 s go in at least three pieces: {}", pieces.len());
    assert_eq!((pieces[0].0, pieces.last().unwrap().1), (80_000, 1_120_000));
    assert!(pieces.windows(2).all(|w| w[0].1 == w[1].0), "the pieces tile the gap");
    let words: Vec<String> = pieces.iter().flat_map(|p| p.2.clone()).collect();
    let expected: Vec<String> = speech::words_starting(80_000, 1_120_000).into_iter().map(speech::word_text).collect();
    assert_eq!(words, expected);
    assert_eq!(fake.state.requests.load(SeqCst), pieces.len());
}

#[tokio::test]
async fn recovery_waits_for_the_recorder_to_write_the_gap() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Recorder::create(&dir.path().join("recordings"), "session").unwrap();
    write_frames(&mut r, 0..10);
    r.checkpoint().unwrap();
    let wav = r.path().to_path_buf();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(ms(300));
        write_frames(&mut r, 10..40);
        r.finalize().unwrap();
    });
    let fake = fake_rest::start(None).await;
    let mut link = spawn_recovery(RestClient::new(cfg(&fake.url)).unwrap());
    link.jobs.send(RecoverJob { recording_id: Uuid::new_v4(), gap_start: 0, from: 0, end: 40 * 1600, wav }).unwrap();
    let pieces = until_done(&mut link).await;
    writer.join().unwrap();
    assert_eq!(pieces.iter().flat_map(|p| p.2.clone()).collect::<Vec<_>>(), speech::expected_words(40 * 1600));
}

#[tokio::test]
async fn a_refused_recovery_ends_recovery_for_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let wav = record(dir.path(), 0..20);
    let fake = fake_rest::start(Some(400)).await;
    let mut link = spawn_recovery(RestClient::new(cfg(&fake.url)).unwrap());
    for gap_start in [0, 16_000] {
        let _ = link.jobs.send(RecoverJob { recording_id: Uuid::new_v4(), gap_start, from: gap_start, end: gap_start + 16_000, wav: wav.clone() });
    }
    assert!(matches!(link.events.recv().await, Some(RecoverEvent::Failed { refused: true, gap_start: 0, .. })));
    assert!(link.events.recv().await.is_none(), "the worker has ended");
    assert_eq!(fake.state.requests.load(SeqCst), 1);
}
```

Append to `crates/core/tests/stt_live.rs`:

```rust
#[tokio::test]
#[ignore]
async fn live_rest_transcribes_a_recorded_interval_with_word_times() {
    use lecturelive_core::stt::rest::{RestClient, RestConfig, RestError};
    let pcm = speech_wav();
    let t = RestClient::new(RestConfig::new(api_key(), vec![])).unwrap().transcribe(&pcm[48_000..]).await.unwrap();
    println!("{t:#?}");
    assert!(t.text.to_lowercase().contains("gradient descent"), "{}", t.text);
    assert!(!t.words.is_empty() && t.words.iter().all(|w| w.start <= w.end && w.end <= t.duration + 0.01));
    let refused = RestClient::new(RestConfig::new("xai-not-a-real-key".into(), vec![])).unwrap().transcribe(&pcm[..16_000]).await.unwrap_err();
    println!("{refused:?}");
    assert!(matches!(refused, RestError::Refused(ref m) if m.contains("Incorrect API key")));
}
```

Add `pub mod fake_rest;` to `crates/core/tests/support/mod.rs`, and `pub mod rest;` to `crates/core/src/stt/mod.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core rest` and `cargo test -p lecturelive-core --test stt_rest`
Expected: compile errors (`pieces`, `RestClient`, `fake_rest` not found).

- [ ] **Step 3: Implement**

In `crates/core/src/audio/recorder.rs`, change `const HEADER_LEN: u64 = 44;` to `pub const HEADER_LEN: u64 = 44;`.

`crates/core/tests/support/fake_rest.rs`:

```rust
//! A fake Grok REST transcription endpoint (spec §11): answers each clip of synthetic speech with
//! the words that start in it.
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::fake_stt::BAD_KEY_BODY;
use super::speech;

#[derive(Default)]
pub struct RestState {
    pub requests: AtomicUsize,
    fields: Mutex<Vec<(String, String)>>,
    auth: Mutex<Vec<String>>,
}

impl RestState {
    /// The form's text fields of every request, in order.
    pub fn fields(&self) -> Vec<(String, String)> {
        self.fields.lock().unwrap().clone()
    }

    pub fn auth(&self) -> Vec<String> {
        self.auth.lock().unwrap().clone()
    }
}

pub struct FakeRest {
    pub url: String,
    pub state: Arc<RestState>,
}

pub async fn start(refuse: Option<u16>) -> FakeRest {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/stt", listener.local_addr().unwrap());
    let state = Arc::new(RestState::default());
    let st = state.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let st = st.clone();
            tokio::spawn(async move { handle(tcp, refuse, &st).await });
        }
    });
    FakeRest { url, state }
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// The parts of a multipart/form-data body, as (field name, content).
fn multipart(body: &[u8], boundary: &str) -> Vec<(String, Vec<u8>)> {
    let delim = format!("--{boundary}");
    let mut parts = Vec::new();
    let mut at = find(body, delim.as_bytes(), 0).expect("a first boundary") + delim.len();
    while let Some(next) = find(body, delim.as_bytes(), at) {
        let part = &body[at..next]; // "\r\n<headers>\r\n\r\n<content>\r\n"
        let head_end = find(part, b"\r\n\r\n", 0).expect("part headers");
        let head = String::from_utf8_lossy(&part[..head_end]);
        let name = head.split("name=\"").nth(1).and_then(|s| s.split('"').next()).expect("a field name").to_string();
        parts.push((name, part[head_end + 4..part.len() - 2].to_vec()));
        at = next + delim.len();
    }
    parts
}

async fn read_more(tcp: &mut TcpStream, buf: &mut Vec<u8>) -> bool {
    let mut chunk = vec![0u8; 65_536];
    match tcp.read(&mut chunk).await {
        Ok(n) if n > 0 => {
            buf.extend_from_slice(&chunk[..n]);
            true
        }
        _ => false,
    }
}

async fn handle(mut tcp: TcpStream, refuse: Option<u16>, st: &RestState) {
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n", 0) {
            break i + 4;
        }
        if !read_more(&mut tcp, &mut buf).await {
            return;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let header = |name: &str| head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()));
    let len: usize = header("content-length").expect("reqwest sends the length of a form it can measure").parse().unwrap();
    while buf.len() < head_end + len {
        if !read_more(&mut tcp, &mut buf).await {
            return;
        }
    }
    st.requests.fetch_add(1, SeqCst);
    st.auth.lock().unwrap().extend(header("authorization"));
    let (status, reason, body) = match refuse {
        Some(code) => (code, "Bad Request", BAD_KEY_BODY.to_string()),
        None => {
            let ctype = header("content-type").expect("a multipart form");
            let boundary = ctype.split("boundary=").nth(1).expect("a boundary").trim_matches('"').to_string();
            let mut wav = Vec::new();
            for (name, value) in multipart(&buf[head_end..head_end + len], &boundary) {
                if name == "file" {
                    wav = value;
                } else {
                    st.fields.lock().unwrap().push((name, String::from_utf8(value).unwrap()));
                }
            }
            let pcm: Vec<i16> = hound::WavReader::new(std::io::Cursor::new(wav)).unwrap().samples::<i16>().map(|s| s.unwrap()).collect();
            (200, "OK", speech::transcribe_clip(&pcm).to_string())
        }
    };
    let resp = format!("HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
    let _ = tcp.write_all(resp.as_bytes()).await;
    let _ = tcp.shutdown().await;
}
```

Prepend to `crates/core/src/stt/rest.rs`:

```rust
//! Recovery of transcript gaps from the recording through the REST endpoint (spec §5.4).
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::Instant;
use uuid::Uuid;

use super::protocol::{is_refusal, refusal_message, to_samples, ServerWord};
use super::transcript::Word;
use crate::audio::frame::FRAME_SAMPLES;
use crate::audio::recorder::{HEADER_LEN, SAMPLE_RATE};

pub const REST_URL: &str = "https://api.x.ai/v1/stt";
/// The longest audio per request: the Python CLI's longest chunk.
const MAX_PIECE: usize = 30 * SAMPLE_RATE as usize;
/// Each piece ends at the quietest 100 ms of its last 10 s.
const CUT_WINDOW: usize = 10 * SAMPLE_RATE as usize;
const MAX_RETRY_UNITS: u32 = 30;

#[derive(Debug, Clone)]
pub struct RestConfig {
    pub url: String,
    pub api_key: String,
    pub keyterms: Vec<String>,
    /// Retry waits are 1, 2, 4 … 30 of these.
    pub retry_unit: Duration,
    pub request_timeout: Duration,
    /// How long a job waits for the recorder to write its interval.
    pub file_wait: Duration,
}

impl RestConfig {
    pub fn new(api_key: String, keyterms: Vec<String>) -> Self {
        Self { url: REST_URL.into(), api_key, keyterms, retry_unit: Duration::from_secs(1), request_timeout: Duration::from_secs(60), file_wait: Duration::from_secs(10) }
    }
}

/// The answer: word times are seconds from the clip's start; silence has no `words`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RestTranscript {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub words: Vec<ServerWord>,
    #[serde(default)]
    pub duration: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RestError {
    Refused(String),
    Transient(String),
}

impl std::fmt::Display for RestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestError::Refused(m) => write!(f, "refused: {m}"),
            RestError::Transient(m) => f.write_str(m),
        }
    }
}

pub struct RestClient {
    http: reqwest::Client,
    cfg: RestConfig,
}

impl RestClient {
    pub fn new(cfg: RestConfig) -> Result<Self> {
        // reqwest is built without a TLS provider of its own; rustls uses ring, as the websocket does.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder().timeout(cfg.request_timeout).build().context("build the HTTP client")?;
        Ok(Self { http, cfg })
    }

    pub async fn transcribe(&self, pcm: &[i16]) -> Result<RestTranscript, RestError> {
        let wav = wav_bytes(pcm).map_err(|e| RestError::Transient(format!("encode the WAV: {e}")))?;
        let file = reqwest::multipart::Part::bytes(wav).file_name("recovery.wav").mime_str("audio/wav").map_err(|e| RestError::Transient(e.to_string()))?;
        let mut form = reqwest::multipart::Form::new().part("file", file).text("language", "en").text("format", "true");
        for k in &self.cfg.keyterms {
            form = form.text("keyterm", k.trim().to_string());
        }
        let resp = self.http.post(&self.cfg.url).bearer_auth(&self.cfg.api_key).multipart(form).send().await.map_err(|e| RestError::Transient(e.to_string()))?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| RestError::Transient(e.to_string()))?;
        if status.is_success() {
            return serde_json::from_str(&body).map_err(|e| RestError::Transient(format!("unreadable answer: {e}")));
        }
        let msg = format!("{status}: {}", refusal_message(&body));
        Err(if is_refusal(status.as_u16()) { RestError::Refused(msg) } else { RestError::Transient(msg) })
    }
}

fn wav_bytes(pcm: &[i16]) -> hound::Result<Vec<u8>> {
    let spec = hound::WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut out = std::io::Cursor::new(Vec::with_capacity(44 + pcm.len() * 2));
    let mut w = hound::WavWriter::new(&mut out, spec)?;
    for &s in pcm {
        w.write_sample(s)?;
    }
    w.finalize()?;
    Ok(out.into_inner())
}

/// Splits an interval into requests of at most 30 s, each ending in the quietest 100 ms of its last 10 s.
pub fn pieces(pcm: &[i16]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    while pcm.len() - start > MAX_PIECE {
        let quietest = (start + MAX_PIECE - CUT_WINDOW..=start + MAX_PIECE - FRAME_SAMPLES)
            .step_by(FRAME_SAMPLES)
            .min_by_key(|&i| pcm[i..i + FRAME_SAMPLES].iter().map(|&s| (s as i64).pow(2)).sum::<i64>())
            .expect("the window holds whole frames");
        let cut = quietest + FRAME_SAMPLES / 2;
        out.push(start..cut);
        start = cut;
    }
    if start < pcm.len() {
        out.push(start..pcm.len());
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecoverJob {
    pub recording_id: Uuid,
    /// The gap's start as the sidecar records it: names the gap.
    pub gap_start: u64,
    /// Where recovery begins: past the pieces an earlier run already logged.
    pub from: u64,
    pub end: u64,
    pub wav: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecoverEvent {
    Piece { recording_id: Uuid, gap_start: u64, start_sample: u64, end_sample: u64, text: String, words: Vec<Word> },
    Done { recording_id: Uuid, gap_start: u64 },
    Failed { recording_id: Uuid, gap_start: u64, message: String, refused: bool },
}

pub struct RecoveryLink {
    pub jobs: mpsc::UnboundedSender<RecoverJob>,
    pub events: mpsc::Receiver<RecoverEvent>,
}

/// Starts the session's recovery worker: one job at a time, in order.
pub fn spawn_recovery(client: RestClient) -> RecoveryLink {
    let (jobs, mut rx) = mpsc::unbounded_channel::<RecoverJob>();
    let (tx, events) = mpsc::channel(64);
    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            let (recording_id, gap_start) = (job.recording_id, job.gap_start);
            let ev = match recover(&client, &job, &tx, &rx).await {
                Ok(()) => RecoverEvent::Done { recording_id, gap_start },
                Err(RestError::Refused(message)) => {
                    // The key or a parameter is refused: nothing later can succeed this session.
                    let _ = tx.send(RecoverEvent::Failed { recording_id, gap_start, message, refused: true }).await;
                    return;
                }
                Err(RestError::Transient(message)) => RecoverEvent::Failed { recording_id, gap_start, message, refused: false },
            };
            if tx.send(ev).await.is_err() {
                return;
            }
        }
    });
    RecoveryLink { jobs, events }
}

async fn recover(client: &RestClient, job: &RecoverJob, tx: &mpsc::Sender<RecoverEvent>, jobs: &mpsc::UnboundedReceiver<RecoverJob>) -> Result<(), RestError> {
    let pcm = read_interval(&job.wav, job.from, job.end, client.cfg.file_wait).await.map_err(|e| RestError::Transient(format!("{e:#}")))?;
    for piece in pieces(&pcm) {
        let start = job.from + piece.start as u64;
        let mut failures = 0u32;
        let t = loop {
            match client.transcribe(&pcm[piece.clone()]).await {
                Ok(t) => break t,
                // Retried while the session runs; once it is stopping, the gap waits for a later session.
                Err(RestError::Transient(m)) if !jobs.is_closed() => {
                    let units = 1u32.checked_shl(failures).unwrap_or(u32::MAX).min(MAX_RETRY_UNITS);
                    failures = failures.saturating_add(1);
                    if !pause(client.cfg.retry_unit * units, jobs).await {
                        return Err(RestError::Transient(m));
                    }
                }
                Err(e) => return Err(e),
            }
        };
        let words = t.words.iter().map(|w| Word { text: w.text.clone(), start_sample: start + to_samples(w.start), end_sample: start + to_samples(w.end) }).collect();
        let ev = RecoverEvent::Piece { recording_id: job.recording_id, gap_start: job.gap_start, start_sample: start, end_sample: job.from + piece.end as u64, text: t.text.trim().to_string(), words };
        if tx.send(ev).await.is_err() {
            return Err(RestError::Transient("the session has ended".into()));
        }
    }
    Ok(())
}

/// Waits `d`, or less if the session starts stopping; false if it did.
async fn pause(d: Duration, jobs: &mpsc::UnboundedReceiver<RecoverJob>) -> bool {
    let until = Instant::now() + d;
    while Instant::now() < until {
        if jobs.is_closed() {
            return false;
        }
        tokio::time::sleep((until - Instant::now()).min(Duration::from_millis(100))).await;
    }
    !jobs.is_closed()
}

/// Samples [from, end) of a recording, waiting while the recorder has not yet written through `end`.
async fn read_interval(wav: &Path, from: u64, end: u64, wait: Duration) -> Result<Vec<i16>> {
    let give_up = Instant::now() + wait;
    loop {
        let path = wav.to_path_buf();
        if let Some(pcm) = tokio::task::spawn_blocking(move || read_pcm(&path, from, end)).await?? {
            return Ok(pcm);
        }
        anyhow::ensure!(Instant::now() < give_up, "{} holds no audio through sample {end} after {wait:?}", wav.display());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn read_pcm(path: &Path, from: u64, end: u64) -> Result<Option<Vec<i16>>> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    if f.metadata()?.len() < HEADER_LEN + end * 2 {
        return Ok(None);
    }
    f.seek(SeekFrom::Start(HEADER_LEN + from * 2))?;
    let mut bytes = vec![0u8; ((end - from) * 2) as usize];
    f.read_exact(&mut bytes)?;
    Ok(Some(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core rest::tests` → 4 passed; `cargo test -p lecturelive-core --test stt_rest` → 5 passed. Live (authorised; 5.6 s of audio): `cargo test -p lecturelive-core --test stt_live live_rest -- --ignored --nocapture` → passes; record the printed transcript and the refusal in the ledger.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/stt/rest.rs crates/core/src/stt/mod.rs crates/core/src/audio/recorder.rs \
  crates/core/tests/support/fake_rest.rs crates/core/tests/support/mod.rs crates/core/tests/stt_rest.rs crates/core/tests/stt_live.rs
git commit -m "Recover transcript gaps from the recording through the REST endpoint

A job reads the gap's samples from the WAV, waiting for the recorder to
write them, and posts pieces of at most 30 s cut in their quietest
100 ms, with the fields the Python CLI sends. Each answer is one piece
whose words are mapped to recording samples, so the pieces tile the gap.
Transient failures are retried while the session runs; a refusal ends
recovery for the session. The client installs ring as rustls' provider,
because reqwest is built without one."
```

---

### Task 9: Coordinator: STT and recovery links, commits, gaps, the cutoff API (milestone tasks 3–6, session side)

**Files:**
- Modify: `crates/core/src/session/coordinator.rs`, `crates/cli/src/main.rs` (compile only: the new `SessionConfig` fields and `Notification` variants)

**Interfaces:**
- Consumes: `SttLink`, `SttInput`, `SttEvent` (Tasks 5–6), `RecoveryLink`, `RecoverJob`, `RecoverEvent` (Task 8), `SegmentLog`, `NewSegment`, `SegmentSource`, `Segment` (Task 7), `Gap::new`, `OpenUtterance` (Task 3).
- Produces:
  - `SessionConfig { dir, stem, stt: Option<SttLink>, recovery: Option<RecoveryLink> }` (`Default`)
  - `Notification::{…M1…, Stt(SttStatus), Open { stable, tentative }, Segment(Segment), Recovered(Gap), RecoveryFailed(String)}`; `SttStatus::{Connected, Retrying { after, reason }, Refused(String), ServerError(String), Stopped(String)}`
  - `StopReport { …M1…, segments: u64, unresolved: usize }`
  - `SessionHandle::cutoff(&self) -> Result<CutoffResult>`; `CutoffResult { confirmed: bool, segments: u64 }`

Behaviour:
- Frames go to the STT queue with `try_send`, but only while more than 8 slots are free. `Begin`, `End` and cutoffs always fit. A control message that does not fit, or a closed queue, means the worker is gone. When that happens the coordinator notifies once, stops listening to the worker, and keeps recording. At each later `End` it records `[max(marker origin, last logged end), samples)` as an `stt_interrupted` gap for recovery.
- `Connected` saves its gap and the new `open_utterance` in one sidecar write. `Ended` saves its gap and clears the marker in one write. Each new transcript gap is queued for recovery. So are the transcript gaps left unresolved in the sidecar at session start, each from where the segment log shows it already committed.
- `Utterance` and recovered `Piece`s with text are appended through the segment log on a blocking thread. `Done` marks its gap resolved. A refused recovery stops queueing jobs for the session.
- A cutoff has three cases. Without STT, it answers `confirmed: false`. With STT but no recording in progress, it answers `confirmed: true`. Otherwise it forwards `Cutoff { sample: end of the last frame forwarded }` and answers when the worker settles it, with the segment count at that moment.
- Stop: after the source ends, the recorder and STT queues close, and the recorder, STT and recovery events are drained. Recovery's queue closes once STT has ended, and recovery finishes what it holds. Pending cutoffs answer `confirmed: false`.
- Two deferred M1 minors are fixed here because this code is rewritten anyway. A closed recorder queue (the recorder thread panicked) now fails the session instead of being ignored. A failing final sidecar save no longer replaces the error that ended the session. Neither has a test: the first needs a panicking recorder thread and the second a save that fails only at the end, and neither can be injected without a seam that serves nothing else.

- [ ] **Step 1: Write the failing tests**

In the test module of `crates/core/src/session/coordinator.rs`, add `const STEM: &str = "lecture_notes_20260925";`, change `cfg` to `SessionConfig { dir: dir.to_path_buf(), stem: STEM.into(), ..Default::default() }`, and add these imports and tests:

```rust
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
        assert_eq!(sc.open_utterance, None);
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
```

In `crates/cli/src/main.rs`, change the `SessionConfig { dir, stem }` literal to `SessionConfig { dir, stem, ..Default::default() }`, and add `Some(_) => {}` as the last arm of the notification `match` (Task 11 prints the new variants).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p lecturelive-core coordinator`
Expected: compile errors (`SessionConfig` has no field `stt`; `cutoff`, `CutoffResult`, `SttStatus` not found).

- [ ] **Step 3: Implement**

Replace everything in `crates/core/src/session/coordinator.rs` above `enum RecMsg` with:

```rust
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
use crate::session::segments::{NewSegment, Segment, SegmentLog, SegmentSource};
use crate::session::sidecar::{sidecar_path, Gap, GapKind, OpenUtterance, RecState, RecordingEntry, Sidecar};
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

    /// Waits for the session to end (after `request_stop`, or when the source ends).
    pub async fn finish(self) -> Result<StopReport> {
        self.task.await?
    }
}

pub fn spawn(cfg: SessionConfig, source: Box<dyn Source>) -> (SessionHandle, mpsc::Receiver<Notification>) {
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let (notify_tx, notify_rx) = mpsc::channel(256);
    let task = tokio::spawn(run(cfg, source, cmd_rx, notify_tx));
    (SessionHandle { cmd: cmd_tx, task }, notify_rx)
}
```

Keep `RecMsg`, `RecorderEvent`, `finalize` and `recorder_worker` as they are. Replace `struct Coordinator`, its `impl`, and `run` with:

```rust
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
        let marker = self.sidecar.open_utterance.filter(|m| m.recording_id == recording_id);
        let logged = self.segments.as_ref().and_then(|l| l.last_end(recording_id)).unwrap_or(0);
        let from = marker.map_or(0, |m| m.from_sample).max(logged);
        if samples > from {
            let g = self.add_gap(Gap::new(recording_id, from, Some(samples), GapKind::SttInterrupted));
            self.recover(&g);
        }
        if marker.is_some() {
            self.sidecar.open_utterance = None;
        }
        self.save().await
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
                self.sidecar.open_utterance = Some(OpenUtterance { recording_id, from_sample: origin });
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
                if self.sidecar.open_utterance.is_some_and(|m| m.recording_id == recording_id) {
                    self.sidecar.open_utterance = None;
                }
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
            SttEvent::Retrying { after, reason } => self.notify(Notification::Stt(SttStatus::Retrying { after, reason })),
            SttEvent::ServerError(m) => self.notify(Notification::Stt(SttStatus::ServerError(m))),
            SttEvent::Refused(m) => self.notify(Notification::Stt(SttStatus::Refused(m))),
        }
        Ok(())
    }

    async fn on_recovery(&mut self, ev: RecoverEvent) -> Result<()> {
        match ev {
            RecoverEvent::Piece { recording_id, start_sample, end_sample, text, words, .. } => {
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
            // Without live STT nothing is confirmed; with it but between recordings, nothing is open.
            let _ = reply.send(CutoffResult { confirmed: self.stt.is_some(), segments });
            return;
        };
        let id = self.next_cutoff;
        self.next_cutoff += 1;
        self.cutoffs.insert(id, reply);
        self.stt_control(SttInput::Cutoff { id, recording_id, sample: self.forwarded_to });
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

async fn run(cfg: SessionConfig, source: Box<dyn Source>, mut cmd_rx: mpsc::Receiver<Command>, notify: mpsc::Sender<Notification>) -> Result<StopReport> {
    let sidecar_path = sidecar_path(&cfg.dir, &cfg.stem);
    let sidecar = Sidecar::load(&sidecar_path)?.unwrap_or_default();
    let segments = if cfg.stt.is_some() || cfg.recovery.is_some() {
        let (dir, stem) = (cfg.dir.clone(), cfg.stem.clone());
        Some(tokio::task::spawn_blocking(move || SegmentLog::open(&dir, &stem)).await??)
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
        }
    }
    // The source has ended: close the recorder and STT queues and drain every worker. Recovery's
    // queue closes once STT can raise no more gaps, and recovery finishes what it holds.
    c.rec_tx = None;
    c.stt = None;
    let mut recorder_open = true;
    loop {
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
            else => break,
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p lecturelive-core coordinator` → the five M1 tests and the six new ones pass. Run `cargo test -p lecturelive-core` → everything passes. Run `cargo build -p lecturelive-cli` → builds.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/session/coordinator.rs crates/cli/src/main.rs
git commit -m "Commit the STT worker's utterances and gaps, recover them, and take cutoffs

The coordinator forwards frames to the STT queue without waiting and
keeps room for control messages. It appends closed utterances to the
segment log and transcript, and records each transcript gap with the new
live origin in one sidecar write. Every unresolved transcript gap, new or
left by an earlier session, is queued for recovery from where the log
shows it already committed. A cutoff is answered once the writer settles
it. A lost worker leaves its audio as gaps while recording continues.
A panicked recorder thread now fails the session, and a failing last
save no longer hides the error that ended it."
```

---
### Task 10: The gate suite through the whole session (gate)

**Files:**
- Create: `crates/core/tests/support/sources.rs`, `crates/core/tests/stt_gate.rs`
- Modify: `crates/core/tests/support/mod.rs`

**Interfaces:**
- Consumes: everything above; the real STT worker, recovery worker, coordinator, recorder and segment log, against the fakes.
- Produces: `sources::{anchor(), Speech { frames, pace, fake }, Silence { samples, fake, pause_after }}` (test support).

Each test runs a whole session: a paced source, the coordinator with its recorder thread, the real STT worker against the fake websocket, and real recovery against the fake REST endpoint. Then it reads the files. "No duplicate or missing committed intervals" is checked two ways. Every synthetic word of the recording must appear in the segment log exactly once, whether committed live or recovered. And no two segments' intervals may overlap. Outages are measured in audio: the fake refuses connections until the source has produced `refuse_for` more samples. A 300 s outage therefore takes about 7 s of test time. Sessions run on a multi-thread runtime, so the fakes, the workers and the coordinator do not take turns on one thread.

- [ ] **Step 1: Write the gate tests**

`crates/core/tests/support/sources.rs`:

```rust
//! Sources that feed a session like a device would.
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, TimeZone};
use lecturelive_core::audio::frame::Frame;
use lecturelive_core::audio::source::{Source, SourceEvent};
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

use super::fake_stt::State;
use super::speech;

pub fn anchor() -> DateTime<Local> {
    Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 0).unwrap()
}

fn begin(id: Uuid) -> SourceEvent {
    SourceEvent::Begin { recording_id: id, anchor: anchor(), source_uid: "Test_UID".into(), input_rate: 16_000, channels: 1 }
}

/// Synthetic speech, paced, telling the fake STT server how far the audio has got.
pub struct Speech {
    pub frames: u64,
    pub pace: Duration,
    pub fake: Arc<State>,
}

impl Source for Speech {
    fn run(self: Box<Self>, out: Sender<SourceEvent>, _stop: Arc<AtomicBool>) {
        let id = Uuid::new_v4();
        out.blocking_send(begin(id)).unwrap();
        for k in 0..self.frames {
            let f = Frame { recording_id: id, sample_offset: k * 1600, valid_samples: 1600, pcm16: speech::frame_pcm(k) };
            out.blocking_send(SourceEvent::Frame(f)).unwrap();
            self.fake.feed.store((k + 1) * 1600, SeqCst);
            std::thread::sleep(self.pace);
        }
        out.blocking_send(SourceEvent::End { recording_id: id, samples: self.frames * 1600, stream_errors: 0 }).unwrap();
    }
}

/// Silence as long as a recorded fixture, started once the fake has accepted the connection, so
/// the stream begins at sample 0; it can pause after `pause_after.0` frames until `pause_after.1` is set.
pub struct Silence {
    pub samples: u64,
    pub fake: Arc<State>,
    pub pause_after: Option<(u64, Arc<AtomicBool>)>,
}

impl Source for Silence {
    fn run(self: Box<Self>, out: Sender<SourceEvent>, _stop: Arc<AtomicBool>) {
        let id = Uuid::new_v4();
        out.blocking_send(begin(id)).unwrap();
        for _ in 0..5_000 {
            if self.fake.accepted.load(SeqCst) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        for k in 0..self.samples.div_ceil(1600) {
            if let Some((after, go)) = &self.pause_after {
                while k == *after && !go.load(SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            let valid = (self.samples - k * 1600).min(1600) as u32;
            out.blocking_send(SourceEvent::Frame(Frame { recording_id: id, sample_offset: k * 1600, valid_samples: valid, pcm16: [0; 1600] })).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        out.blocking_send(SourceEvent::End { recording_id: id, samples: self.samples, stream_errors: 0 }).unwrap();
    }
}
```

Add `pub mod sources;` to `crates/core/tests/support/mod.rs`.

`crates/core/tests/stt_gate.rs`:

```rust
//! The M2 gate (docs/milestones.md): exact outputs on protocol fixtures; disconnects of 3, 15, 45
//! and 300 s with no duplicate or missing committed intervals; REST recovery exercised; a 4xx
//! stops STT without a reconnect loop while recording continues. No network.
mod support;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use lecturelive_core::audio::source::Source;
use lecturelive_core::session::coordinator::{self, CutoffResult, Notification, SessionConfig, StopReport, SttStatus};
use lecturelive_core::session::segments::{self, segments_path, transcript_path, Segment, SegmentSource};
use lecturelive_core::session::sidecar::{sidecar_path, Gap, GapKind, Sidecar};
use lecturelive_core::stt::protocol::FINALIZE;
use lecturelive_core::stt::rest::{spawn_recovery, RestClient, RestConfig};
use lecturelive_core::stt::stream::{self, SttConfig};
use support::fake_stt::{self, Config, Mode, Outage, Refusal};
use support::sources::{anchor, Silence, Speech};
use support::{fake_rest, fixtures, speech};

const STEM: &str = "lecture_notes_20260925";

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn links(stt_url: &str, rest_url: &str) -> SessionConfig {
    let stt = SttConfig {
        url: stt_url.into(),
        backoff_unit: ms(1),
        connect_timeout: ms(2_000),
        send_timeout: ms(2_000),
        idle_timeout: ms(2_000),
        finalize_wait: ms(2_000),
        done_wait: ms(2_000),
        ..SttConfig::new("test-key".into(), vec![])
    };
    let rest = RestConfig { url: rest_url.into(), retry_unit: ms(1), request_timeout: ms(5_000), file_wait: ms(5_000), ..RestConfig::new("test-key".into(), vec![]) };
    SessionConfig { stt: Some(stream::spawn(stt).unwrap()), recovery: Some(spawn_recovery(RestClient::new(rest).unwrap())), ..Default::default() }
}

struct Run {
    report: StopReport,
    notes: Vec<Notification>,
    segments: Vec<Segment>,
    sidecar: Sidecar,
    transcript: String,
}

fn read_back(dir: &Path, report: StopReport, notes: Vec<Notification>) -> Run {
    Run {
        report,
        notes,
        segments: segments::read(&segments_path(dir, STEM)).unwrap(),
        sidecar: Sidecar::load(&sidecar_path(dir, STEM)).unwrap().unwrap(),
        transcript: std::fs::read_to_string(transcript_path(dir, STEM)).unwrap(),
    }
}

async fn session(dir: &Path, source: impl Source, stt_url: &str, rest_url: &str) -> Run {
    let cfg = SessionConfig { dir: dir.to_path_buf(), stem: STEM.into(), ..links(stt_url, rest_url) };
    let (handle, mut notes) = coordinator::spawn(cfg, Box::new(source));
    let report = handle.finish().await.unwrap();
    let mut seen = Vec::new();
    while let Ok(n) = notes.try_recv() {
        seen.push(n);
    }
    read_back(dir, report, seen)
}

/// Every synthetic word exactly once, live or recovered, and no committed interval twice.
fn assert_committed_once(run: &Run, len: u64) {
    let mut words: Vec<(u64, String)> = run.segments.iter().flat_map(|s| s.words.iter().map(|w| (w.start_sample, w.text.clone()))).collect();
    words.sort();
    assert_eq!(words.into_iter().map(|w| w.1).collect::<Vec<_>>(), speech::expected_words(len), "missing or duplicate words");
    let mut spans: Vec<(u64, u64)> = run.segments.iter().map(|s| (s.start_sample, s.end_sample)).collect();
    spans.sort();
    assert!(spans.windows(2).all(|w| w[0].1 <= w[1].0), "overlapping segments: {spans:?}");
    assert_eq!(run.transcript.lines().count(), run.segments.len(), "one transcript line per segment");
}

async fn disconnect(outage_secs: u64, refusal: Refusal) {
    let dir = tempfile::tempdir().unwrap();
    let outage = Outage { at_sample: 30 * 16_000, refuse_for: outage_secs * 16_000, refusal, freeze: false };
    let fake = fake_stt::start(Config { outages: vec![outage], ..Default::default() }).await;
    let rest = fake_rest::start(None).await;
    let frames = (60 + outage_secs) * 10;
    let run = session(dir.path(), Speech { frames, pace: ms(2), fake: fake.state.clone() }, &fake.url, &rest.url).await;
    let len = frames * 1600;
    assert_eq!(run.report.recordings.len(), 1);
    assert_eq!(run.report.recordings[0].1, len, "the recording is whole");
    assert_committed_once(&run, len);
    let stt_gaps: Vec<&Gap> = run.sidecar.gaps.iter().filter(|g| g.kind.is_transcript()).collect();
    assert!(!stt_gaps.is_empty() && stt_gaps.iter().all(|g| g.resolved), "{stt_gaps:?}");
    assert!(rest.state.requests.load(SeqCst) >= 1, "REST recovery was exercised");
    assert!(run.segments.iter().any(|s| s.source == SegmentSource::Recovered));
    assert_eq!(run.report.unresolved, 0);
    assert_eq!(run.sidecar.open_utterance, None);
    if outage_secs > 5 {
        let covered = stt_gaps.iter().any(|g| g.start_sample < 30 * 16_000 && g.end_sample.unwrap() >= (30 + outage_secs - 5) * 16_000);
        assert!(covered, "a gap spans the outage, less the 5 s held for the new connection: {stt_gaps:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_3_s_disconnect_commits_every_interval_once() {
    disconnect(3, Refusal::TcpClose).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_15_s_disconnect_commits_every_interval_once() {
    disconnect(15, Refusal::Status(503)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_45_s_disconnect_commits_every_interval_once() {
    disconnect(45, Refusal::TcpClose).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_300_s_disconnect_commits_every_interval_once() {
    disconnect(300, Refusal::Status(503)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refusal_stops_stt_without_a_reconnect_loop_while_recording_continues() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_stt::start(Config { refuse_all: Some(Refusal::Status(400)), ..Default::default() }).await;
    let rest = fake_rest::start(Some(400)).await;
    let run = session(dir.path(), Speech { frames: 200, pace: ms(2), fake: fake.state.clone() }, &fake.url, &rest.url).await;
    assert_eq!(run.report.recordings[0].1, 200 * 1600, "recording continued");
    assert_eq!(fake.state.attempts.load(SeqCst), 1, "one attempt, no reconnect loop");
    assert_eq!(rest.state.requests.load(SeqCst), 1, "recovery tried once, then stopped");
    let refusals = run.notes.iter().filter(|n| matches!(n, Notification::Stt(SttStatus::Refused(_)))).count();
    assert_eq!(refusals, 1);
    let id = run.sidecar.recordings[0].id;
    assert_eq!(run.sidecar.gaps, vec![Gap::new(id, 0, Some(200 * 1600), GapKind::SttRefused)]);
    assert!(run.segments.is_empty());
    assert_eq!(run.report.unresolved, 1, "the gap waits for a session with a working key");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refusal_after_a_disconnect_stops_reconnecting_and_recovery_fills_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let outage = Outage { at_sample: 10 * 16_000, refuse_for: 0, refusal: Refusal::Status(503), freeze: false };
    let fake = fake_stt::start(Config { outages: vec![outage], refuse_after_drop: Some(Refusal::Status(401)), ..Default::default() }).await;
    let rest = fake_rest::start(None).await;
    let run = session(dir.path(), Speech { frames: 300, pace: ms(2), fake: fake.state.clone() }, &fake.url, &rest.url).await;
    assert_eq!(fake.state.attempts.load(SeqCst), 2);
    assert_eq!(run.report.recordings[0].1, 300 * 1600);
    assert_committed_once(&run, 300 * 1600);
    assert!(run.sidecar.gaps.iter().any(|g| g.kind == GapKind::SttRefused && g.resolved));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_endpoint_fixture_writes_exact_segments_and_transcript_lines() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_stt::start(Config { mode: Mode::Replay(fixtures::load("endpoint_pauses")), ..Default::default() }).await;
    let rest = fake_rest::start(None).await;
    let run = session(dir.path(), Silence { samples: 167_615, fake: fake.state.clone(), pause_after: None }, &fake.url, &rest.url).await;
    assert_eq!(
        run.transcript,
        "[10:00:00] Gradient descent updates the weights.\n[10:00:04] The learning rate controls the step size.\n[10:00:08] Momentum smooths the updates.\n"
    );
    assert_eq!(
        run.segments.iter().map(|s| (s.id, s.start_sample, s.end_sample, s.source)).collect::<Vec<_>>(),
        vec![(0, 16, 49_280, SegmentSource::Live), (1, 59_520, 117_760, SegmentSource::Live), (2, 128_640, 167_616, SegmentSource::Live)]
    );
    assert_eq!(run.segments[1].said_at, anchor() + chrono::Duration::milliseconds(4_143));
    assert!(run.sidecar.gaps.is_empty(), "{:?}", run.sidecar.gaps);
    assert_eq!(rest.state.requests.load(SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cutoff_through_the_session_is_confirmed_on_the_json_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_stt::start(Config { mode: Mode::Replay(fixtures::load("finalize_json")), ..Default::default() }).await;
    let rest = fake_rest::start(None).await;
    let go = Arc::new(AtomicBool::new(false));
    let source = Silence { samples: 137_838, fake: fake.state.clone(), pause_after: Some((30, go.clone())) };
    let cfg = SessionConfig { dir: dir.path().to_path_buf(), stem: STEM.into(), ..links(&fake.url, &rest.url) };
    let (handle, _notes) = coordinator::spawn(cfg, Box::new(source));
    fake.state.wait_frames(30).await;
    let resume = async {
        // The recorded final came while the next frames went out: resume once the finalize is sent.
        while !fake.state.texts().iter().any(|t| t == FINALIZE) {
            tokio::time::sleep(ms(1)).await;
        }
        go.store(true, SeqCst);
    };
    let (cut, ()) = tokio::join!(handle.cutoff(), resume);
    assert_eq!(cut.unwrap(), CutoffResult { confirmed: true, segments: 1 });
    let report = handle.finish().await.unwrap();
    let run = read_back(dir.path(), report, vec![]);
    assert_eq!(
        run.transcript,
        "[10:00:00] Transfer learning reuses pre-trained weights. The\n[10:00:03] losses cross entropy over the vocabulary, gradient descent updates the weights after every batch.\n"
    );
}
```

- [ ] **Step 2: Run the gate**

Run: `cargo test -p lecturelive-core --test stt_gate -- --nocapture` → 8 passed. Then run it three times in a row (`for i in 1 2 3; do cargo test -q -p lecturelive-core --test stt_gate || break; done`). Record the wall time of the 300 s test and any recorder-overflow gap in the ledger. The tests pass with or without such a gap, but a regular one means the pace is too fast for this disk, and the pace goes to 3 ms. A failure is a defect in Tasks 5–9: diagnose it there with superpowers:systematic-debugging, fix it test-first in the owning task's test file, and keep these tests unchanged.

- [ ] **Step 3: Commit**

```bash
git add crates/core/tests/stt_gate.rs crates/core/tests/support/sources.rs crates/core/tests/support/mod.rs
git commit -m "Add the M2 gate suite: fixtures, disconnects, recovery and refusals end to end

Each test runs a whole session against the fake servers and reads the
files back. Disconnects of 3, 15, 45 and 300 s commit every word of the
recording exactly once and no interval twice, with the outage recovered
through REST. A refusal is tried once and the recording continues. The
endpoint fixture writes exact transcript lines, and a cutoff through the
session is confirmed on the recorded finalize."
```

---

### Task 11: CLI `record --stt --keyterm` (milestone tasks 1 and 6, CLI side)

**Files:**
- Modify: `crates/cli/src/main.rs`

**Interfaces:**
- Consumes: `stt::stream::{spawn, SttConfig}`, `stt::rest::{spawn_recovery, RestClient, RestConfig}`, `SessionConfig`, `Notification`, `SttStatus`, `SegmentSource`, `LaunchReport.untranscribed`, `StopReport.{segments, unresolved}`.
- Produces: `lecturelive record … --stt [--keyterm T]…`. Without `--stt` nothing changes.

- [ ] **Step 1: Implement**

Add to the `Record` variant:

```rust
        /// Stream to Grok speech-to-text and write the transcript (GROK_API_KEY from the repository's .env or the environment)
        #[arg(long)]
        stt: bool,
        /// A term to bias recognition toward (repeatable; up to 100, each at most 50 characters)
        #[arg(long = "keyterm", requires = "stt")]
        keyterms: Vec<String>,
```

Pass them through: `Cmd::Record { loopback, device, dir, secs, keep_days, stt, keyterms } => record(loopback, device, dir, secs, keep_days, stt, keyterms).await?`.

Change the imports to `use lecturelive_core::session::coordinator::{self, Notification, SessionConfig, SttStatus};`, `use lecturelive_core::session::segments::SegmentSource;` and `use lecturelive_core::stt::{probe, rest, stream};`, and add:

```rust
fn secs(samples: u64) -> f64 {
    samples as f64 / 16_000.0
}
```

In `record`, add the two parameters (`stt: bool, keyterms: Vec<String>`). Right after the microphone check, before the folder is touched, build the links, so that a missing key or a bad keyterm stops the command before anything is recorded:

```rust
    let (stt_link, recovery) = if stt {
        dotenvy::dotenv().ok();
        let key = std::env::var("GROK_API_KEY").context("GROK_API_KEY is not set (the repository's .env, or the environment)")?;
        let link = stream::spawn(stream::SttConfig::new(key.clone(), keyterms.clone()))?;
        let client = rest::RestClient::new(rest::RestConfig::new(key, keyterms))?;
        (Some(link), Some(rest::spawn_recovery(client)))
    } else {
        (None, None)
    };
```

After the launch report's other lines, print what a crash left untranscribed:

```rust
    for g in &report.untranscribed {
        println!("transcript to recover: {:.1}–{:.1} s of recording {} (recovered by the next --stt session of that day)", secs(g.start_sample), g.end_sample.map_or(0.0, secs), g.recording_id);
    }
```

Spawn with the links: `coordinator::spawn(SessionConfig { dir, stem, stt: stt_link, recovery }, Box::new(DeviceSource { uid }))`. Replace the `Some(_) => {}` arm from Task 9 with:

```rust
                Some(Notification::Stt(s)) => match s {
                    SttStatus::Connected => println!("transcribing"),
                    SttStatus::Retrying { after, reason } => println!("transcription interrupted ({reason}); reconnecting in {} s", after.as_secs()),
                    SttStatus::Refused(m) => eprintln!("transcription refused: {m}. Recording continues without it."),
                    SttStatus::ServerError(m) => eprintln!("transcription server: {m}"),
                    SttStatus::Stopped(m) => eprintln!("transcription stopped: {m}"),
                },
                Some(Notification::Open { .. }) => {} // the terminal shows closed utterances only
                Some(Notification::Segment(s)) => {
                    let tag = if s.source == SegmentSource::Recovered { "   (recovered)" } else { "" };
                    println!("[{}] {}{tag}", s.said_at.format("%H:%M:%S"), s.text);
                }
                Some(Notification::Recovered(g)) => println!("recovered the transcript of {:.1}–{:.1} s of recording {}", secs(g.start_sample), g.end_sample.map_or(0.0, secs), g.recording_id),
                Some(Notification::RecoveryFailed(m)) => eprintln!("recovery: {m}"),
```

After the existing end-of-session lines:

```rust
    if stt {
        println!("segments: {}, transcript gaps still to recover: {}", report.segments, report.unresolved);
    }
```

- [ ] **Step 2: Build and check the offline paths**

Run: `cargo build -p lecturelive-cli` → builds with no warnings. Then:

```bash
L=./target/debug/lecturelive
$L record --keyterm x --device none 2>&1 | head -3                      # clap: --keyterm requires --stt
GROK_API_KEY=x $L record --stt --keyterm "$(printf 'k%.0s' $(seq 51))" --device none 2>&1 | tail -1   # "keyterm … must have 1 to 50 characters", nothing recorded
env -u GROK_API_KEY sh -c "cd /tmp && $PWD/target/debug/lecturelive record --stt --device none" 2>&1 | tail -1   # "GROK_API_KEY is not set …"
```

Record the three outputs in the ledger. The live runs are Task 13's.

- [ ] **Step 3: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "Add --stt and --keyterm to the record command

The command streams the recording to Grok speech-to-text, prints each
committed line as it lands (recovered ones marked), reports interruptions,
refusals and recoveries, and ends with the segments written and the gaps
still to recover. A missing key or a bad keyterm stops it before anything
is recorded."
```

---

### Task 12: Spec §5 to the recorded protocol and M2's design

**Files:**
- Modify: `docs/spec.md` §5.1–5.4 only

State the design as it now is, with no "previously" trail. Replace §5.1–5.4 (from `### 5.1 Connection` up to `## 6. Notes`) with:

````markdown
### 5.1 Connection

`wss://api.x.ai/v1/stt?model=grok-voice-transcribe-2.0&encoding=pcm&sample_rate=16000&interim_results=true&language=en`,
plus `&keyterm=…` once per keyterm, with `Authorization: Bearer`. Keyterms are validated (at most 100, each
1–50 characters) and URL-encoded one by one. The server endpoints by itself about 0.5 s after speech stops,
so no endpointing parameter is sent (`endpointing=400` is accepted and changes nothing measurable).

A refusal comes at the websocket upgrade as HTTP 4xx: an unknown key is 400 with a JSON body
(`{"code":…,"error":"Incorrect API key provided…"}`), an unknown model 404, a malformed parameter 400 with a
plain-text body. After `transcript.created`, audio goes as binary little-endian PCM16 frames of 100 ms, in
order, through a single writer. The server takes audio faster than real time: 10.5 s sent unpaced returned
the same finals within 0.7 s. Control messages are JSON text, `{"type":"finalize"}` and
`{"type":"audio.done"}`; bare text is answered with an `error` message and otherwise ignored, and the
connection stays open.

Times in server messages (`start`, `duration`, word `start`/`end`) are seconds of audio, to the
millisecond, from the first frame sent on that connection. Each connection is therefore an epoch anchored
at (recording, sample of its first frame), and a new recording always gets a new connection. While audio
flows the server sends a message at least every two seconds, speech or not: through 12 s of silence, an
empty interim every second and an empty chunk-final every two. Five seconds without one means the
connection is dead even if it has not closed.

### 5.2 Transcript state machine

Messages are `transcript.partial` with `is_final` and `speech_final`. Interims (`is_final: false`) arrive
about once a second with `words: []`; their text is the whole open utterance so far and `start` is its
start. Finals carry words. At a natural endpoint a chunk-final (`is_final: true, speech_final: false`)
comes about 0.5 s after speech stops and a `speech_final` about 0.5 s later, with the same text and words
and a duration that runs on through the endpoint silence; after a finalize or `audio.done` both arrive at
once. In silence the server sends empty interims and empty chunk-finals, which close nothing.
`transcript.done` has empty text and the total duration: the client owns the whole transcript.

State: closed utterances + open utterance (stable chunks + tentative tail).

- `is_final=false`: replaces the tentative tail only (less the stable text it repeats). An empty hypothesis never erases stable content.
- `is_final=true`: its range `[start, start+duration)` replaces the stable chunks it overlaps; an empty final adds nothing.
- `speech_final=true`: closes the open utterance. Its text is the stable part after that replacement, which with the recorded protocol is the `speech_final`'s own text. The utterance is its `[start, start+duration)` of the recording and produces exactly one segment, appended once to the segment log and the transcript file (an empty one produces none). A final ending at or before the last close is a repeat and is ignored.

The transcript is settled through the end of the last close. UI stability is not disk durability: the
sidecar records the open interval (the live epoch's origin), so after a crash the audio after the last
logged segment becomes a gap recoverable from the recording.

### 5.3 Snapshot cutoff

`snapshot` records an audio cutoff (the end of the last frame forwarded to the STT writer), and the writer
sends all frames through the cutoff, then `{"type":"finalize"}`, then continues with live audio. The server
answers every finalize with a `speech_final` that ends exactly at the cutoff, within 40–250 ms: the open
utterance, or with nothing open an empty one of duration 0. The coordinator waits (3 s) for the close that
settles the transcript through the cutoff; when it is already settled it does not ask. On timeout, or when
the cutoff's audio is not on the live connection (connecting, disconnected), the unconfirmed interval stays
pending and the UI reports "snapshot of confirmed material; transcription still catching up". Recording
never pauses and no lock is held while waiting.

### 5.4 Reconnection and recovery

Frames reach the writer through a bounded queue that keeps room for control messages: a full queue drops
frames and never waits, and the writer, seeing the hole in sample offsets, ends the epoch. On an unexpected
close, a send that fails or stalls (5 s), five silent seconds, or dropped frames, the writer reconnects
live-first with backoff (1, 2, 4 … 30 s). While connecting it holds the newest 5 s of frames and sends them
first, so a short outage costs only its unclosed utterance; the new epoch begins at the oldest held frame.
The gap is the interval from the last close to the new epoch's origin (including handshake time and any
unclosed utterance), written to the sidecar together with the new origin. Gap kinds name the cause:
`stt_offline`, `stt_overflow`, `stt_refused`, `stt_interrupted` (the session stopped first).

Gaps are recovered by the REST endpoint over the recorded interval, owned by the recovery path alone, so
live and recovery commits are disjoint: `POST https://api.x.ai/v1/stt`, multipart `file` (a WAV of the
interval), `language=en`, `format=true`, `keyterm` per keyterm (not observed to take effect). The answer is
`{text, language, duration, words:[{text,start,end}]}` with times from the clip's start (silence: empty
text, no `words`). The interval goes in pieces of at most 30 s, each cut at the quietest 100 ms of its last
10 s; each piece with speech is one segment, and the pieces tile the gap. Recovery resumes after the pieces
already in the segment log, so a crash mid-recovery writes no line twice. Recovered segments enter the
segment log when they arrive and are eligible for the next snapshot even though their speech times precede
earlier snapshots.

A gap is resolved when nothing more can be done for its interval: a transcript gap once recovery has
committed all of it, a gap with no audio behind it (capture or recorder overflow, device gone, rate change,
interrupted) when it is recorded. A recording is therefore kept (§4.4) exactly while it holds a transcript
not yet recovered. Auth and parameter errors (4xx) stop STT without a reconnect loop, and stop recovery for
the session; recording continues and their gaps wait for a later session. On stop: drain the framer, send
`audio.done`, process remaining finals until `transcript.done` (5 s), ignore `transcript.done`'s text for
commit purposes, finalize the recording, recover what is pending, take the last snapshot.
````

- [ ] **Step 1: Replace the section**

Edit `docs/spec.md` as above. Nothing outside §5.1–5.4 changes.

- [ ] **Step 2: Check it against the code**

Check the following by reading `stream.rs`, `rest.rs` and `coordinator.rs` side by side with the new text: every number (0.5 s, 5 s hold, 5 s idle, 5 s send, 3 s cutoff, 5 s done, 30 s pieces, 10 s window, 1–30 s backoff, 100 keyterms of 50 characters), every message spelling, and every gap kind. Note any mismatch in the ledger, and fix the text or the code (test-first).

- [ ] **Step 3: Commit**

```bash
git add docs/spec.md
git commit -m "Rewrite spec 5.1-5.4 to the recorded protocol and the M2 design

The connection, endpointing, refusals and server timing follow the M0
and M2 recordings. Cutoffs are confirmed by the final that ends at them,
reconnection holds five seconds of frames, and recovery goes in pieces
that resume after what is logged. Resolved means nothing more can be done
for a gap's interval."
```

---

### Task 13: Acceptance run, review and findings

**Files:**
- Modify: `docs/milestones.md` (the M2 section and the M2 row of Status only)

Nothing here needs a person or a live lecture. The live checks play synthesised speech into BlackHole with `say -a "BlackHole 2ch"`, which changes no output and makes no sound, and record it with `record --loopback`. The terminal already has microphone permission from M1. If macOS asks again, that single click is the only sitting, and it is asked for once, after everything else.

Define in every shell, from the repository root:

```bash
LL() { cargo run -q -p lecturelive-cli -- "$@"; }
AS="$HOME/Library/Application Support/LectureLive"
SC() { cat "$1/.live_notes/lecture_notes_$(date +%Y%m%d).v2.json"; }
TR() { cat "$1/lecture_transcript_$(date +%Y%m%d).txt"; }
```

- [ ] **Step 1: The suite**

```bash
cargo test -p lecturelive-core 2>&1 | grep -E "^test result|FAILED|panicked"   # sum passed / failed / ignored over all targets
cargo test -p lecturelive-core --test stt_live -- --ignored --nocapture 2>&1 | tail -40
```

Report the summed counts and each live test's result. Paste any failure verbatim.

- [ ] **Step 2: Live session with speech** (about 30 s of audio)

```bash
LL canary route status | grep default_output_uid       # note it; it must not change
D="$AS/m2-live"; rm -rf "$D"
TEXT="Transfer learning reuses pre-trained weights. [[slnc 1500]] The loss is cross-entropy over the vocabulary. [[slnc 1500]] Gradient descent updates the weights after every batch. [[slnc 1500]] Momentum smooths the updates, and the learning rate sets the step size."
(sleep 3; say -a "BlackHole 2ch" "$TEXT") &
LL record --loopback --stt --keyterm cross-entropy --dir "$D" --secs 30
TR "$D"                                                 # about four lines, one per sentence
SC "$D" | jq -c '{gaps, open_utterance}'                # gaps: [] (or only resolved ones); no open_utterance
LL canary route status | grep default_output_uid       # unchanged
```

- [ ] **Step 3: Crash mid-utterance, then recovery at the next launch** (about 40 s of audio)

```bash
D="$AS/m2-crash"; rm -rf "$D"
LONG="Backpropagation applies the chain rule layer by layer, multiplying local gradients from the loss back to every weight, so that each parameter learns how much it contributed to the error, and the optimizer then moves every weight a small step against its gradient before the next batch arrives."
(sleep 3; say -a "BlackHole 2ch" -r 150 "$LONG") & SAY=$!
LL record --loopback --stt --dir "$D" --secs 60 & REC=$!
sleep 14; pkill -9 -f "target/debug/lecturelive record"; wait $REC 2>/dev/null; wait $SAY
SC "$D" | jq -c '.open_utterance'                       # set: the live epoch was open
LL record --loopback --stt --dir "$D" --secs 8          # "transcript to recover: …", then lines ending "(recovered)", "recovered the transcript of …"
TR "$D"
SC "$D" | jq -c '.gaps'                                 # interrupted (resolved), stt_interrupted (resolved)
```

If `pkill` matches nothing, find the process with `pgrep -fl lecturelive`.

- [ ] **Step 4: A refused key** (no audio is billed)

```bash
GROK_API_KEY=xai-not-a-real-key LL record --loopback --stt --dir "$AS/m2-refused" --secs 5
SC "$AS/m2-refused" | jq -c '.gaps'                     # one stt_refused gap, unresolved; the WAV is 5 s
```

Expected: one "transcription refused: 400 Bad Request: Incorrect API key provided…", one "recovery: refused: …", no reconnect lines, and "transcript gaps still to recover: 1".

- [ ] **Step 5: Versions**

`cargo tree -p lecturelive-core --depth 1 | grep -E "reqwest|tokio-tungstenite|rustls"` and `cargo tree -p lecturelive-core -i aws-lc-rs` (no match), into the Findings.

- [ ] **Step 6: Whole-branch review**

Dispatch one fresh reviewer on the most capable model (`opus`). Give it the diff `git diff m1-recording-loopback...m2-streaming-recovery`, spec §5, this plan and the M2 gate. Ask for Critical, Important and Minor findings, each with a file:line reference. Ask it to look at gap arithmetic and disjointness, the shutdown drain, the cutoff paths, the refusal paths, recovery resumption, and blocking I/O on the coordinator task. Fix every Critical and Important finding test-first: add the failing test to the owning task's test file, then fix, then run the whole suite. Commit each fix on its own. Minor findings go into the Findings as open threads.

- [ ] **Step 7: Findings**

In the M2 section of `docs/milestones.md`, turn the **Gate** paragraph into four checkbox lines:

```markdown
**Gate** (checked in `cargo test` without network, plus live checks with synthesised speech; plan Task 13):

- [ ] Exact outputs on protocol fixtures
- [ ] Disconnects of 3, 15, 45 and 300 s with no duplicate or missing committed intervals
- [ ] REST recovery exercised
- [ ] 4xx stops STT without a reconnect loop while recording continues
```

Tick each line that held. Then write **Findings**:
- the resolved versions of the new crates;
- the protocol behaviour the fake server encodes;
- the disconnect results per interval (3, 15, 45, 300 s) with test times;
- the REST recovery evidence (fake and live);
- what "resolved" means for each gap kind;
- the live runs' observations;
- review findings and fixes;
- every failed line with "no §14.1 fallback applies" (§14.1 covers loopback only);
- open threads with the milestone that owns each.

Set M2's Status to `done` only if every gate line holds, otherwise `in progress`, and link the plan in the Status table.

- [ ] **Step 8: Commit**

```bash
git add docs/milestones.md
git commit -m "Record M2 findings

Findings cover the protocol the fake server encodes, the disconnect
suite, REST recovery against the fake and live endpoints, crash recovery
and a refused key live, the meaning of resolved per gap kind, the review
and the crate versions added in M2."
```

Leave the branch unmerged and unpushed.

