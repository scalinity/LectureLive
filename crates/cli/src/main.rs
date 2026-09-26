use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

mod args;
mod lecture;
mod plain;
mod utility;

use args::{Cli, Cmd};

pub(crate) fn data_dir() -> Result<PathBuf> {
    let dir = dirs::data_dir().context("no Application Support dir")?.join("LectureLive");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Inputs => utility::inputs()?,
        Cmd::Outputs => utility::outputs()?,
        Cmd::Loopback(l) => utility::loopback_cmd(l)?,
        Cmd::Record { loopback, mixed, device, dir, secs, keep_days, stt, keyterms } => utility::record(loopback, mixed, device, dir, secs, keep_days, stt, keyterms).await?,
        Cmd::Lecture(args) => lecture::lecture_cmd(args).await?,
        Cmd::Canary(c) => utility::canary(c).await?,
    }
    Ok(())
}
