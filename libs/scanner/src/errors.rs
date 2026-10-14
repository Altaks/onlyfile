use std::io;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ScannerError {
    #[error("File system error : {0}")]
    FileSystemError(#[from] io::Error),
}
