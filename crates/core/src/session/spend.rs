//! The spend ledger (spec §8, §9.1): one line per paid request, in the Python CLI's exact format,
//! kept in the app's data directory and taking over the CLI's ledger.
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::fsutil::write_atomic;
use crate::pyjson::{dumps, round_int, round_to};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpendKind {
    Transcribe,
    Notes,
    Polish,
    Page,
}

impl SpendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transcribe => "transcribe",
            Self::Notes => "notes",
            Self::Polish => "polish",
            Self::Page => "page",
        }
    }
}

/// Speech-to-text responses carry no cost, so these published rates are used, `billed: false` (spec §8).
pub const STREAM_USD_PER_SECOND: f64 = 0.20 / 3600.0;
/// The Python CLI's `STT_USD_PER_SECOND`: REST, as recovery uses it.
pub const BATCH_USD_PER_SECOND: f64 = 0.10 / 3600.0;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SpendEntry {
    pub at: String,
    #[serde(default = "unknown")]
    pub course: String,
    #[serde(default = "unknown")]
    pub lecture: String,
    pub what: String,
    pub usd: f64,
    #[serde(default)]
    pub billed: bool,
    #[serde(default)]
    pub audio_s: Option<f64>,
}

fn unknown() -> String {
    "?".into()
}

/// One ledger line as the Python CLI's `spend.add` writes it.
pub fn line(at: NaiveDateTime, course: &str, lecture: &str, what: SpendKind, usd: f64, billed: bool, audio_s: Option<f64>) -> String {
    let mut v = json!({"at": at.format("%Y-%m-%dT%H:%M:%S").to_string(), "course": course, "lecture": lecture, "what": what.as_str(), "usd": round_to(usd, 6), "billed": billed});
    if let Some(s) = audio_s {
        v["audio_s"] = json!(round_to(s, 1));
    }
    format!("{}\n", dumps(&v))
}

/// Every line that parses; a last line cut short by a crash is skipped (spec §8).
pub fn read(path: &Path) -> Result<Vec<SpendEntry>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    Ok(text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
}

/// Appends and syncs; after a line cut short, the new one starts on a line of its own.
fn append(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let torn = std::fs::read(path).map(|b| b.last().is_some_and(|&c| c != b'\n')).unwrap_or(false);
    let mut f = OpenOptions::new().create(true).append(true).open(path).with_context(|| format!("open {}", path.display()))?;
    if torn {
        f.write_all(b"\n")?;
    }
    f.write_all(bytes)?;
    f.sync_data().with_context(|| format!("sync {}", path.display()))
}

struct Inner {
    path: PathBuf,
    course: String,
    lecture: String,
    lecture_total: f64,
    kinds: HashMap<SpendKind, f64>,
}

/// The ledger, as one lecture writes to it. Clones share it.
#[derive(Clone)]
pub struct Spend {
    inner: Arc<Mutex<Inner>>,
}

impl Spend {
    /// `today` picks this lecture's spend so far today, the figure the CLI reports.
    pub fn open(path: &Path, course: &str, lecture: &str, today: NaiveDate) -> Result<Self> {
        let day = today.format("%Y-%m-%d").to_string();
        let lecture_total = read(path)?.iter().filter(|e| e.course == course && e.lecture == lecture && e.at.starts_with(&day)).map(|e| e.usd).sum();
        let inner = Inner { path: path.to_path_buf(), course: course.into(), lecture: lecture.into(), lecture_total, kinds: HashMap::new() };
        Ok(Self { inner: Arc::new(Mutex::new(inner)) })
    }

    pub fn add(&self, what: SpendKind, usd: f64, billed: bool, audio_s: Option<f64>) -> Result<()> {
        self.add_at(Local::now().naive_local(), what, usd, billed, audio_s)
    }

    pub fn add_at(&self, at: NaiveDateTime, what: SpendKind, usd: f64, billed: bool, audio_s: Option<f64>) -> Result<()> {
        let mut g = self.inner.lock().expect("the ledger lock");
        g.lecture_total += usd;
        *g.kinds.entry(what).or_default() += usd;
        let l = line(at, &g.course, &g.lecture, what, usd, billed, audio_s);
        append(&g.path, l.as_bytes())
    }

    pub fn lecture_total(&self) -> f64 {
        self.inner.lock().expect("the ledger lock").lecture_total
    }

    /// What this process has spent on one kind of request.
    pub fn kind_total(&self, what: SpendKind) -> f64 {
        self.inner.lock().expect("the ledger lock").kinds.get(&what).copied().unwrap_or(0.0)
    }
}

/// Where take-over records how much of the CLI's ledger it has imported.
pub fn import_mark(app: &Path) -> PathBuf {
    app.with_file_name("spend.import.json")
}

/// Takes over the Python CLI's ledger (spec §8): appends the complete lines it gained since the last
/// take-over. A crash between that append and the mark is caught by the app ledger already ending
/// with exactly those bytes, so nothing is imported twice.
pub fn take_over(app: &Path, cli: &Path) -> Result<usize> {
    let cli_bytes = match std::fs::read(cli) {
        Ok(b) => b,
        // Not there, or not the app's to read (a packaged app launched by the system, in the person's Documents folder): nothing to take over.
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied) => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("read {}", cli.display())),
    };
    let end = cli_bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let mark_path = import_mark(app);
    let mark = std::fs::read(&mark_path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v["cli_bytes"].as_u64()).unwrap_or(0) as usize;
    let mut imported = 0;
    if end > mark {
        let new = &cli_bytes[mark..end];
        if !std::fs::read(app).unwrap_or_default().ends_with(new) {
            append(app, new)?;
        }
        imported = new.iter().filter(|&&b| b == b'\n').count();
    }
    if end != mark {
        // A CLI ledger shorter than the mark was rewritten by hand: what it holds now is where to continue from.
        write_atomic(&mark_path, json!({"cli_bytes": end}).to_string().as_bytes())?;
    }
    Ok(imported)
}

pub fn money(usd: f64) -> String {
    if usd > 0.0 && usd < 0.005 {
        return "<$0.01".into();
    }
    let fixed = format!("{usd:.2}");
    let (int, frac) = fixed.split_once('.').expect("two decimals");
    let digits: Vec<char> = int.chars().collect();
    let mut grouped = String::new();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(*c);
    }
    format!("${grouped}.{frac}")
}

pub fn bar(fraction: f64, width: usize) -> String {
    let eighths = round_int(fraction.clamp(0.0, 1.0) * width as f64 * 8.0) as usize;
    let partial = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"][eighths % 8];
    let s = format!("{}{partial}", "█".repeat(eighths / 8));
    format!("{:<width$}", s.trim_end())
}

/// The CLI's terminal styles: its study page's signal red and teal when the terminal has true colour.
#[derive(Debug, Clone, Copy)]
pub struct Paint {
    pub color: bool,
    pub truecolor: bool,
}

impl Paint {
    pub fn paint(&self, text: &str, styles: &[&str]) -> String {
        if !self.color || styles.is_empty() || text.is_empty() {
            return text.to_string();
        }
        let codes: Vec<&str> = styles
            .iter()
            .map(|s| match *s {
                "bold" => "1",
                "dim" => "2",
                "red" if self.truecolor => "38;2;242;118;107",
                "red" => "31",
                "teal" if self.truecolor => "38;2;93;184;192",
                _ => "36",
            })
            .collect();
        format!("\x1b[{}m{text}\x1b[0m", codes.join(";"))
    }
}

/// Sums in first-seen order, as a Python dict keeps them.
fn add_to(v: &mut Vec<(String, f64)>, key: &str, usd: f64) {
    match v.iter_mut().find(|(k, _)| k == key) {
        Some((_, total)) => *total += usd,
        None => v.push((key.to_string(), usd)),
    }
}

/// Largest first; ties keep first-seen order (Python's stable sort).
fn by_amount(v: &mut [(String, f64)]) {
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
}

/// The spend view's figures (spec §9.1): all-time totals, the last three months by course, and the
/// eight most recent lectures by kind.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub total: f64,
    /// The part computed from published rates rather than billed.
    pub estimated: f64,
    pub calls: usize,
    /// Oldest first; each month's courses largest first.
    pub months: Vec<Month>,
    /// Newest first; each lecture's kinds largest first.
    pub recent: Vec<Recent>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Month {
    /// `YYYY-MM`.
    pub key: String,
    /// `September 2026`.
    pub label: String,
    pub total: f64,
    pub courses: Vec<(String, f64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Recent {
    /// `YYYY-MM-DD`.
    pub day: String,
    /// `25 Sep`.
    pub label: String,
    pub course: String,
    pub lecture: String,
    pub total: f64,
    pub kinds: Vec<(String, f64)>,
}

/// The CLI's `show_spend` aggregation: sums in first-seen order, ties kept in that order.
pub fn summary(entries: &[SpendEntry]) -> Summary {
    let mut months: Vec<(String, Vec<(String, f64)>)> = Vec::new();
    let mut lectures: Vec<((String, String, String), Vec<(String, f64)>)> = Vec::new();
    for e in entries {
        let month: String = e.at.chars().take(7).collect();
        let i = months.iter().position(|(m, _)| *m == month).unwrap_or_else(|| {
            months.push((month, Vec::new()));
            months.len() - 1
        });
        add_to(&mut months[i].1, &e.course, e.usd);
        let key = (e.at.chars().take(10).collect::<String>(), e.course.clone(), e.lecture.clone());
        let j = lectures.iter().position(|(k, _)| *k == key).unwrap_or_else(|| {
            lectures.push((key, Vec::new()));
            lectures.len() - 1
        });
        add_to(&mut lectures[j].1, &e.what, e.usd);
    }
    months.sort_by(|a, b| a.0.cmp(&b.0));
    let recent_months = months.len().saturating_sub(3);
    let months = months
        .drain(recent_months..)
        .map(|(key, mut courses)| {
            let total = courses.iter().map(|(_, u)| u).sum();
            by_amount(&mut courses);
            let label = NaiveDate::parse_from_str(&format!("{key}-01"), "%Y-%m-%d").map_or_else(|_| key.clone(), |d| d.format("%B %Y").to_string());
            Month { key, label, total, courses }
        })
        .collect();
    lectures.sort_by(|a, b| b.0.cmp(&a.0));
    let recent = lectures
        .into_iter()
        .take(8)
        .map(|((day, course, lecture), mut kinds)| {
            let total = kinds.iter().map(|(_, u)| u).sum();
            by_amount(&mut kinds);
            let label = NaiveDate::parse_from_str(&day, "%Y-%m-%d").map_or_else(|_| day.clone(), |d| d.format("%-d %b").to_string());
            Recent { day, label, course, lecture, total, kinds }
        })
        .collect();
    Summary { total: entries.iter().map(|e| e.usd).sum(), estimated: entries.iter().filter(|e| !e.billed).map(|e| e.usd).sum(), calls: entries.len(), months, recent }
}

/// The Python CLI's `lecture spend` view, line for line.
pub fn render(entries: &[SpendEntry], columns: usize, p: Paint, ledger: &Path) -> String {
    if entries.is_empty() {
        return format!("\n  Nothing spent yet. Every paid call is logged in {} from the next run.\n\n", ledger.display());
    }
    let s = summary(entries);
    let width = columns.min(92) as isize - 2;
    let name_w = s.months.iter().flat_map(|m| m.courses.iter().map(|(n, _)| n.chars().count())).max().unwrap_or(0).min(34);
    let len = |s: &str| s.chars().count() as isize;
    // Every amount ends at the same right edge; `fill` (a bar) sits between the name and the amount.
    let row = |left: &str, right: &str, left_style: &[&str], right_style: &[&str], fill: &str| {
        let w = (width - len(left) - len(fill)).max(0) as usize;
        format!("{}{}{}\n", p.paint(left, left_style), p.paint(fill, &["teal"]), p.paint(&format!("{right:>w$}"), right_style))
    };
    let mut out = String::from("\n");
    out += &row("  Spend", &format!("all time {}", money(s.total)), &["bold"], &["dim"], "");
    for m in &s.months {
        out += "\n";
        out += &row(&format!("  {}", m.label), &money(m.total), &["bold"], &[], "");
        let top = m.courses.iter().map(|(_, u)| *u).fold(0.0, f64::max);
        for (course, usd) in &m.courses {
            let short: String = course.chars().take(name_w).collect();
            out += &row(&format!("    {short:<name_w$}  "), &money(*usd), &[], &[], &bar(if top > 0.0 { usd / top } else { 0.0 }, 20));
        }
    }
    out += "\n";
    out += &row("  Recent lectures", "", &["bold"], &[], "");
    for r in &s.recent {
        let label: String = format!("{}  ›  {}", r.course, r.lecture).chars().take((width - 22).max(0) as usize).collect();
        out += &row(&format!("    {:<7} {label}", r.label), &money(r.total), &[], &[], "");
        let parts: Vec<String> = r.kinds.iter().map(|(k, v)| format!("{k} {}", money(*v))).collect();
        out += &format!("{}\n", p.paint(&format!("            {}", parts.join("   ")), &["dim"]));
    }
    let share = if s.total > 0.0 { round_int(100.0 * s.estimated / s.total) } else { 0 };
    let note = if s.estimated == 0.0 { "all billed by xAI".to_string() } else { format!("{share}% estimated from published rates, the rest billed by xAI") };
    out += &format!("\n{}\n\n", p.paint(&format!("  {} paid calls; {note}.", s.calls), &["dim"]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap().and_hms_opt(h, m, s).unwrap()
    }

    /// The Python CLI's real ledger line, read with `od -c` (plan research), with the names replaced.
    #[test]
    fn a_line_is_byte_identical_to_the_python_clis() {
        assert_eq!(
            line(at(24, 16, 50, 11), "Machine Learning", "Week 06 — Optimisation", SpendKind::Page, 0.265_218_000_1, true, None),
            "{\"at\": \"2026-09-24T16:50:11\", \"course\": \"Machine Learning\", \"lecture\": \"Week 06 \\u2014 Optimisation\", \"what\": \"page\", \"usd\": 0.265218, \"billed\": true}\n"
        );
        assert_eq!(
            line(at(24, 10, 0, 0), "ML", "W", SpendKind::Transcribe, 97.24 * BATCH_USD_PER_SECOND, false, Some(97.24)),
            "{\"at\": \"2026-09-24T10:00:00\", \"course\": \"ML\", \"lecture\": \"W\", \"what\": \"transcribe\", \"usd\": 0.002701, \"billed\": false, \"audio_s\": 97.2}\n"
        );
    }

    #[test]
    fn every_complete_line_is_read_one_cut_short_is_skipped_and_the_next_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spend.jsonl");
        std::fs::write(&path, format!("{}{{\"at\": \"2026-09-2", line(at(24, 9, 0, 0), "ML", "W", SpendKind::Notes, 0.01, true, None))).unwrap();
        assert_eq!(read(&path).unwrap().len(), 1);
        let spend = Spend::open(&path, "ML", "W", NaiveDate::from_ymd_opt(2026, 9, 24).unwrap()).unwrap();
        spend.add_at(at(24, 9, 5, 0), SpendKind::Polish, 0.02, true, None).unwrap();
        let entries = read(&path).unwrap();
        assert_eq!(entries.iter().map(|e| e.what.as_str()).collect::<Vec<_>>(), vec!["notes", "polish"]);
        assert!(std::fs::read_to_string(&path).unwrap().ends_with("\"billed\": true}\n"));
    }

    #[test]
    fn open_sums_this_lectures_spend_today_and_add_keeps_the_totals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spend.jsonl");
        let mut text = String::new();
        text += &line(at(24, 9, 0, 0), "ML", "W", SpendKind::Notes, 0.5, true, None); // yesterday
        text += &line(at(25, 9, 0, 0), "ML", "W", SpendKind::Notes, 0.25, true, None);
        text += &line(at(25, 9, 1, 0), "ML", "Other", SpendKind::Notes, 1.0, true, None);
        std::fs::write(&path, text).unwrap();
        let spend = Spend::open(&path, "ML", "W", NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()).unwrap();
        assert_eq!(spend.lecture_total(), 0.25);
        spend.add_at(at(25, 10, 0, 0), SpendKind::Page, 0.125, true, None).unwrap();
        assert_eq!((spend.lecture_total(), spend.kind_total(SpendKind::Page), spend.kind_total(SpendKind::Notes)), (0.375, 0.125, 0.0));
    }

    /// Live evidence (M7.1): a packaged app launched by the system may not read the Python CLI's ledger, which lives in the
    /// person's Documents folder ("Operation not permitted"), and the lecture then refused to start. The ledger is a
    /// convenience: one that cannot be read is imported as nothing, as one that is not there is.
    #[test]
    fn a_cli_ledger_that_cannot_be_read_imports_nothing_instead_of_stopping_the_lecture() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (app, cli) = (dir.path().join("app/spend.jsonl"), dir.path().join("spend.jsonl"));
        std::fs::write(&cli, line(at(24, 9, 0, 0), "ML", "W", SpendKind::Notes, 0.1, true, None)).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&cli).is_ok() {
            return; // running as a user who can read anything (root): there is nothing to refuse
        }
        assert_eq!(take_over(&app, &cli).unwrap(), 0, "unreadable is not fatal");
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(take_over(&app, &cli).unwrap(), 1, "and it is imported when it can be read again");
    }

    #[test]
    fn take_over_imports_the_clis_new_lines_once_even_across_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let (app, cli) = (dir.path().join("app/spend.jsonl"), dir.path().join("spend.jsonl"));
        let (a, b, c) = (line(at(24, 9, 0, 0), "ML", "W", SpendKind::Notes, 0.1, true, None), line(at(24, 9, 1, 0), "ML", "W", SpendKind::Page, 0.2, true, None), line(at(25, 9, 0, 0), "ML", "W", SpendKind::Polish, 0.3, true, None));
        assert_eq!(take_over(&app, &cli).unwrap(), 0, "no CLI ledger yet");
        std::fs::write(&cli, format!("{a}{b}{{\"at\": \"2026")).unwrap(); // the CLI's last line is still being written
        assert_eq!(take_over(&app, &cli).unwrap(), 2);
        assert_eq!(take_over(&app, &cli).unwrap(), 0, "nothing twice");
        std::fs::write(&cli, format!("{a}{b}{c}")).unwrap();
        // A crash after the append but before the mark: the next take-over sees its own tail and only moves the mark.
        let mark = std::fs::read(import_mark(&app)).unwrap();
        assert_eq!(take_over(&app, &cli).unwrap(), 1);
        std::fs::write(import_mark(&app), mark).unwrap();
        assert_eq!(take_over(&app, &cli).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(&app).unwrap(), format!("{a}{b}{c}"));
        let spend = Spend::open(&app, "ML", "W", NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()).unwrap();
        spend.add_at(at(25, 10, 0, 0), SpendKind::Notes, 0.4, true, None).unwrap();
        assert_eq!(take_over(&app, &cli).unwrap(), 0);
        assert_eq!(read(&app).unwrap().len(), 4);
    }

    #[test]
    fn money_and_bars_are_the_clis() {
        assert_eq!([money(1234.5), money(0.004), money(0.0), money(0.005), money(0.271918)], ["$1,234.50", "<$0.01", "$0.00", "$0.01", "$0.27"]);
        assert_eq!(bar(0.5, 4), "██  ");
        assert_eq!(bar(1.0, 3), "███");
        assert_eq!(bar(0.0149299, 20), format!("▎{}", " ".repeat(19)));
        assert_eq!(bar(0.0, 2), "  ");
    }

    /// Hand golden from live_notes.py's show_spend with 94 columns and no colour.
    #[test]
    fn the_spend_view_is_the_python_clis() {
        let e = |at: &str, course: &str, lecture: &str, what: &str, usd: f64, billed: bool| SpendEntry { at: at.into(), course: course.into(), lecture: lecture.into(), what: what.into(), usd, billed, audio_s: None };
        let entries = [
            e("2026-09-24T16:50:11", "Machine Learning", "Week 06 — Optimisation", "page", 0.265218, true),
            e("2026-09-24T10:00:00", "Machine Learning", "Week 06 — Optimisation", "transcribe", 0.0027, false),
            e("2026-09-25T11:00:00", "Biology", "Week 02", "notes", 0.004, true),
        ];
        let sp = |n: usize| " ".repeat(n);
        let expected = [
            String::new(),
            format!("  Spend{}all time $0.27", sp(69)),
            String::new(),
            format!("  September 2026{}$0.27", sp(69)),
            format!("    Machine Learning  {}{}$0.27", "█".repeat(20), sp(43)),
            format!("    Biology           ▎{}{}<$0.01", sp(19), sp(42)),
            String::new(),
            format!("  Recent lectures{}", sp(73)),
            format!("    25 Sep  Biology  ›  Week 02{}<$0.01", sp(53)),
            "            notes <$0.01".to_string(),
            format!("    24 Sep  Machine Learning  ›  Week 06 — Optimisation{}$0.27", sp(30)),
            "            page $0.27   transcribe <$0.01".to_string(),
            String::new(),
            "  3 paid calls; 1% estimated from published rates, the rest billed by xAI.".to_string(),
            String::new(),
        ]
        .map(|l| l + "\n")
        .concat();
        let plain = Paint { color: false, truecolor: false };
        assert_eq!(render(&entries, 94, plain, Path::new("/ledger/spend.jsonl")), expected);
        assert_eq!(render(&[], 94, plain, Path::new("/ledger/spend.jsonl")), "\n  Nothing spent yet. Every paid call is logged in /ledger/spend.jsonl from the next run.\n\n");
    }

    #[test]
    fn the_summary_holds_what_the_spend_view_shows() {
        let e = |at: &str, course: &str, lecture: &str, what: &str, usd: f64, billed: bool| SpendEntry { at: at.into(), course: course.into(), lecture: lecture.into(), what: what.into(), usd, billed, audio_s: None };
        let s = summary(&[
            e("2026-09-24T16:50:11", "Machine Learning", "Week 06 — Optimisation", "page", 0.265218, true),
            e("2026-09-24T10:00:00", "Machine Learning", "Week 06 — Optimisation", "transcribe", 0.0027, false),
            e("2026-09-25T11:00:00", "Biology", "Week 02", "notes", 0.004, true),
        ]);
        assert_eq!(s.calls, 3);
        assert!((s.total - 0.271918).abs() < 1e-9 && (s.estimated - 0.0027).abs() < 1e-9);
        assert_eq!(s.months.len(), 1);
        assert_eq!((s.months[0].key.as_str(), s.months[0].label.as_str()), ("2026-09", "September 2026"));
        assert_eq!(s.months[0].courses.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(), ["Machine Learning", "Biology"]);
        assert_eq!(s.recent.iter().map(|r| (r.label.as_str(), r.course.as_str())).collect::<Vec<_>>(), [("25 Sep", "Biology"), ("24 Sep", "Machine Learning")]);
        assert_eq!(s.recent[1].kinds.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["page", "transcribe"]);
    }
}
