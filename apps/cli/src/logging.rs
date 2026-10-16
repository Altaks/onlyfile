//! Sends every log line to an append-only file, since the interface owns the terminal.

use std::{
    fs::OpenOptions,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

const LOG_FOLDER_NAME: &str = "onlyfile";
const LOG_FILE_NAME: &str = "onlyfile.log";

#[derive(Debug, thiserror::Error)]
#[error("cannot open the log file {}: {source}", path.display())]
pub struct LogFileError {
    path: PathBuf,
    source: io::Error,
}

/// Appends every log line to `onlyfile/onlyfile.log` under `data_local_dir`, creating both if
/// needed.
pub fn init(data_local_dir: &Path) -> Result<(), LogFileError> {
    let folder = data_local_dir.join(LOG_FOLDER_NAME);
    let path = folder.join(LOG_FILE_NAME);
    let file = std::fs::create_dir_all(&folder)
        .and_then(|()| OpenOptions::new().create(true).append(true).open(&path))
        .map_err(|source| LogFileError { path, source })?;

    tracing_subscriber::fmt()
        .with_writer(Arc::new(file))
        .with_ansi(false)
        .with_target(false)
        .init();
    Ok(())
}
