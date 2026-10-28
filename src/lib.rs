pub mod config;
pub mod filter;
pub mod github;
pub mod store;

use anyhow::{Context, Result, ensure};
use directories::ProjectDirs;
use std::path::PathBuf;

pub fn database_path() -> Result<PathBuf> {
    let dirs =
        ProjectDirs::from("", "", "gh-comments").context("Cannot determine data directory")?;
    std::fs::create_dir_all(dirs.data_local_dir())?;
    Ok(dirs.data_local_dir().join("inbox.sqlite3"))
}

pub fn open_github(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url)?;
    ensure!(
        parsed.scheme() == "https" && parsed.host_str() == Some("github.com"),
        "Refusing to open a URL outside https://github.com"
    );
    webbrowser::open(url)?;
    Ok(())
}
