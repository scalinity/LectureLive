//! A lecture's start-up (spec §4.4, §5.4, §8): repair and retention, the folder's initialisation,
//! other days' transcript recovery, and the session marker. The CLI's `lecture` command and the app
//! run it under the folder lock, after undoing a canary route left behind.
use anyhow::Result;
use chrono::Local;

use crate::session::coordinator::StopReport;
use crate::session::files::LectureFiles;
use crate::session::folder::{self, InitReport};
use crate::session::launch::{self, LaunchReport, Retention};
use crate::session::segments;
use crate::session::spend::Spend;
use crate::stt::rest::RecoveryLink;

#[derive(Debug)]
pub struct Prepared {
    pub launch: LaunchReport,
    pub init: InitReport,
    /// Each other day recovered before this session, with what its recovery left.
    pub other_days: Vec<(String, StopReport)>,
}

/// Everything before a session starts, in order; `on_other_day` hears each other day's stem before
/// its recovery runs, which can take minutes.
pub async fn prepare(files: &LectureFiles, title: &str, rebuild: bool, retention: Retention, recovery: impl Fn() -> Result<RecoveryLink>, spend: Option<Spend>, on_other_day: &mut (dyn FnMut(&str) + Send)) -> Result<Prepared> {
    let launch = launch::recover(&files.dir, retention, Local::now())?;
    let (_, init) = folder::open(files, title, rebuild)?;
    let other_days = folder::recover_other_days(&files.dir, &files.stem, recovery, spend, on_other_day).await?;
    segments::session_marker(&files.transcript, Local::now())?;
    Ok(Prepared { launch, init, other_days })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::folder::How;

    #[tokio::test]
    async fn prepare_opens_a_fresh_folder_and_marks_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let files = LectureFiles::standard(dir.path(), chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap());
        let no_recovery = || -> Result<RecoveryLink> { anyhow::bail!("no recovery in this test") };
        let mut seen = Vec::new();
        let p = prepare(&files, "# T", false, Retention::KeepAll, no_recovery, None, &mut |s: &str| seen.push(s.to_string())).await.unwrap();
        assert_eq!(p.init.how, How::Created);
        assert!(p.other_days.is_empty() && seen.is_empty());
        assert!(std::fs::read_to_string(&files.transcript).unwrap().starts_with("--- started "));
        assert_eq!(std::fs::read_to_string(&files.notes).unwrap(), "# T\n");
    }
}
