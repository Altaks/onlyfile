//! onlyfile: finds duplicate files in a folder.

use std::{io, path::PathBuf, process::ExitCode};

use clap::Parser;
use directories::BaseDirs;
use jiff::tz::TimeZone;

use crate::{
    app::{App, AppExit},
    format::format_path,
    logging::LogFileError,
    pipeline::PipelineError,
};

mod app;
mod format;
mod logging;
mod pipeline;
mod screens;

const ERROR_PREFIX: &str = "onlyfile:";

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Find duplicate files in a folder.",
    bin_name = "onlyfile"
)]
struct Arguments {
    /// Folder to scan. Defaults to the current folder.
    path: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error("cannot read the current folder: {0}")]
    CurrentFolder(#[source] io::Error),
    #[error("cannot read {}: {source}", path.display())]
    UnreadableRoot { path: PathBuf, source: io::Error },
    #[error("{} is not a folder", .0.display())]
    NotAFolder(PathBuf),
    #[error("cannot find the home folder, which holds the log file")]
    NoHomeFolder,
    #[error(transparent)]
    LogFile(#[from] LogFileError),
    #[error("cannot drive the terminal: {0}")]
    Terminal(#[source] io::Error),
    #[error("the scan stopped: {0}")]
    ScanFailed(#[source] PipelineError),
}

fn main() -> ExitCode {
    match run(Arguments::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{ERROR_PREFIX} {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: Arguments) -> Result<(), AppError> {
    let root = scan_root(arguments.path)?;
    let base_dirs = BaseDirs::new().ok_or(AppError::NoHomeFolder)?;
    logging::init(base_dirs.data_local_dir())?;

    let root_label = format_path(&root, base_dirs.home_dir());
    let events = pipeline::spawn(root);
    let app = App::new(
        root_label,
        TimeZone::system(),
        base_dirs.home_dir().to_path_buf(),
    );
    let exit = ratatui::try_init().and_then(|mut terminal| app.run(&mut terminal, &events));
    ratatui::restore();

    match exit.map_err(AppError::Terminal)? {
        AppExit::Quit => Ok(()),
        AppExit::ScanFailed(error) => Err(AppError::ScanFailed(error)),
    }
}

/// The folder to scan, made absolute and checked to be a folder this process can list.
fn scan_root(path: Option<PathBuf>) -> Result<PathBuf, AppError> {
    let path = match path {
        Some(path) => path,
        None => std::env::current_dir().map_err(AppError::CurrentFolder)?,
    };
    let unreadable = |source| AppError::UnreadableRoot {
        path: path.clone(),
        source,
    };

    let root = std::path::absolute(&path).map_err(unreadable)?;
    if !std::fs::metadata(&root).map_err(unreadable)?.is_dir() {
        return Err(AppError::NotAFolder(root));
    }
    std::fs::read_dir(&root).map_err(unreadable)?;
    Ok(root)
}
