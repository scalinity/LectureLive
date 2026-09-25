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
