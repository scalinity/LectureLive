//! `lecture tidy` as the person runs it (spec §7.7): a dry run writes nothing, `--apply` needs the folder to itself,
//! and slide numbers name one lecture day.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use lecturelive_core::session::files::LectureFiles;
use lecturelive_core::session::lock::FolderLock;
use lecturelive_core::session::sidecar::Sidecar;

fn tidy(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lecturelive")).args(["lecture", "tidy", "--dir"]).arg(dir).args(args).output().unwrap()
}

/// Every file under `dir` with its bytes: what a run that writes nothing leaves exactly as it was.
fn contents(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() { todo.push(p) } else { out.insert(p.clone(), std::fs::read(&p).unwrap()); }
        }
    }
    out
}

fn lecture(dir: &Path, y: i32, m: u32, d: u32) -> LectureFiles {
    let files = LectureFiles::standard(dir, chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap());
    std::fs::create_dir_all(files.state_dir()).unwrap();
    std::fs::write(&files.notes, "# T\n").unwrap();
    Sidecar::default().save(&files.sidecar()).unwrap();
    files
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn a_dry_run_writes_nothing_even_while_the_folder_is_in_use() {
    let dir = tempfile::tempdir().unwrap();
    lecture(dir.path(), 2026, 11, 5);
    let _held = FolderLock::acquire(dir.path()).unwrap();
    let before = contents(dir.path());
    let o = tidy(dir.path(), &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(contents(dir.path()), before);
}

#[test]
fn apply_needs_the_folder_to_itself() {
    let dir = tempfile::tempdir().unwrap();
    lecture(dir.path(), 2026, 11, 5);
    let _held = FolderLock::acquire(dir.path()).unwrap();
    let o = tidy(dir.path(), &["--apply"]);
    assert!(!o.status.success() && stderr(&o).contains("another LectureLive session"), "{}", stderr(&o));
}

#[test]
fn slide_numbers_name_one_day_and_a_range_runs_upward() {
    // R12: --swap and --drop went to every day of the folder; R30: 5-3 named nothing, silently.
    let dir = tempfile::tempdir().unwrap();
    lecture(dir.path(), 2026, 11, 3);
    lecture(dir.path(), 2026, 11, 5);
    let o = tidy(dir.path(), &["--apply", "--drop", "9"]);
    assert!(!o.status.success() && stderr(&o).contains("--day"), "{}", stderr(&o));
    let o = tidy(dir.path(), &["--apply", "--drop", "9", "--day", "20261105"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("lecture_notes_20261105") && !out.contains("lecture_notes_20261103"), "{out}");
    let o = tidy(dir.path(), &["--apply", "--drop", "5-3", "--day", "20261105"]);
    assert!(!o.status.success() && stderr(&o).contains("A at most B"), "{}", stderr(&o));
    let o = tidy(dir.path(), &["--day", "20261201"]);
    assert!(!o.status.success() && stderr(&o).contains("no lecture day"), "{}", stderr(&o));
}

#[test]
fn days_are_told_in_date_order() {
    let dir = tempfile::tempdir().unwrap();
    for (m, d) in [(11, 5), (10, 1), (11, 3)] {
        lecture(dir.path(), 2026, m, d);
    }
    let out = String::from_utf8_lossy(&tidy(dir.path(), &[]).stdout).into_owned();
    let at = |stem: &str| out.find(stem).unwrap();
    assert!(at("lecture_notes_20261001") < at("lecture_notes_20261103") && at("lecture_notes_20261103") < at("lecture_notes_20261105"), "{out}");
}

#[test]
fn notes_that_keep_every_capture_say_so() {
    // R33: notes with a style or a script turn tidying off; the dry run says why.
    let dir = tempfile::tempdir().unwrap();
    let files = lecture(dir.path(), 2026, 11, 5);
    std::fs::write(&files.notes, "# T\n\n<div style=\"color:red\">x</div>\n").unwrap();
    let out = String::from_utf8_lossy(&tidy(dir.path(), &[]).stdout).into_owned();
    assert!(out.contains("styles or scripts"), "{out}");
}
