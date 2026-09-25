//! M0's packaged canary checks, kept so the canary bundle's `--check` mode builds from this source:
//! `--check route on|off|status`, `--check record DEVICE SECS`, `--check windows`, `--check capture ID`.
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use lecturelive_core::audio::{input, routing};
use lecturelive_core::capture::window;

fn app_dir(sub: &str) -> Result<PathBuf, String> {
    let d = dirs::data_dir().ok_or("no Application Support dir")?.join("LectureLive").join(sub);
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    Ok(d)
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn route_sync(action: &str) -> Result<String, String> {
    let p = app_dir("")?.join("route.json");
    match action {
        "on" => routing::enable_loopback(&p).map(|s| format!("routed; previous output {}", s.previous_output_uid)).map_err(err),
        "off" => routing::disable_loopback(&p).map(|r| format!("restored: {r}")).map_err(err),
        _ => routing::route_status(&p).map(|s| format!("{s:#?}")).map_err(err),
    }
}

fn record_sync(device: &str, secs: u64) -> Result<String, String> {
    let mut loudest = 0f32;
    let r = input::record_for(device, Duration::from_secs(secs), &app_dir("canary")?, |l| loudest = loudest.max(l)).map_err(err)?;
    Ok(format!(
        "{} — {:.1} s, dropped {}, loudest second {:.1} dBFS",
        r.path.display(),
        r.samples as f64 / 16_000.0,
        r.dropped_callbacks,
        20.0 * loudest.max(1e-6).log10()
    ))
}

fn windows_sync() -> Result<Vec<(u32, String)>, String> {
    window::list_windows().map(|v| v.into_iter().map(|w| (w.id, format!("{} — {}", w.app, w.title))).collect()).map_err(err)
}

fn capture_sync(id: u32) -> Result<String, String> {
    let out = app_dir("canary")?.join(format!("window_{id}.png"));
    window::capture_window(id, &out).map(|(w, h)| format!("{} {w}x{h}", out.display())).map_err(err)
}

/// One check without the UI: `--check route on|off|status`, `--check record DEVICE SECS`,
/// `--check windows`, `--check capture ID`.
pub fn run_check(args: &[String]) -> Result<String, String> {
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["route", action] => route_sync(action),
        ["record", device, secs] => record_sync(device, secs.parse().map_err(err)?),
        ["windows"] => windows_sync().map(|v| v.iter().map(|(id, l)| format!("{id} {l}")).collect::<Vec<_>>().join("\n")),
        ["capture", id] => capture_sync(id.parse().map_err(err)?),
        other => Err(format!("unknown check {other:?}")),
    }
}

/// Appends one line to checks.log. A "started" line precedes every check, so a check that
/// is killed or aborts is visible as a start with no result.
pub fn log_line(line: &str) -> std::io::Result<()> {
    let path = app_dir("canary").map_err(std::io::Error::other)?.join("checks.log");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{} {line}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"))
}

pub fn log_check(args: &[String], result: &Result<String, String>) -> std::io::Result<()> {
    match result {
        Ok(s) => log_line(&format!("{} ok\n{s}\n", args.join(" "))),
        Err(e) => log_line(&format!("{} FAILED: {e}\n", args.join(" "))),
    }
}
