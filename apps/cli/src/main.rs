//! onlyfile: finds duplicate files in a folder.

use std::{io, path::PathBuf, process::ExitCode};

use clap::Parser;
use directories::BaseDirs;
use jiff::tz::TimeZone;

use purge::Disposal;

use crate::{
    app::{App, AppExit},
    format::format_path,
    logging::LogFileError,
    pipeline::PipelineError,
    settings::RunSettings,
};

mod app;
mod format;
mod logging;
mod pipeline;
mod removal;
mod screens;
mod settings;

const ERROR_PREFIX: &str = "onlyfile:";

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Find duplicate files in a folder and move the extra copies to the trash.",
    bin_name = "onlyfile"
)]
struct Arguments {
    /// Folder to scan. Defaults to the current folder.
    path: Option<PathBuf>,

    /// Delete marked copies permanently instead of moving them to the trash.
    #[arg(long)]
    permanent: bool,
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
    #[error(
        "the removal stopped without a result; every copy it already removed is listed in {}",
        .0.display()
    )]
    RemovalInterrupted(PathBuf),
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
    let log_path = logging::init(base_dirs.data_local_dir())?;
    let home = base_dirs.home_dir();

    let settings = RunSettings {
        root_label: format_path(&root, home),
        time_zone: TimeZone::system(),
        home: home.to_path_buf(),
        disposal: if arguments.permanent {
            Disposal::Permanent
        } else {
            Disposal::Trash
        },
        log_label: format_path(&log_path, home),
    };
    let events = pipeline::spawn(root);
    let app = App::new(settings);
    let exit = ratatui::try_init().and_then(|mut terminal| app.run(&mut terminal, &events));
    ratatui::restore();

    match exit.map_err(AppError::Terminal)? {
        AppExit::Quit => Ok(()),
        AppExit::ScanFailed(error) => Err(AppError::ScanFailed(error)),
        AppExit::RemovalInterrupted => Err(AppError::RemovalInterrupted(log_path)),
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
