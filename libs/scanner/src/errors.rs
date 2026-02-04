use std::io;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ScannerError {
    #[error("Provided path : {0} does not exist.")]
    ProvidedPathDoesNotExist(String),
    #[error("File system error : {0}")]
    FileSystemError(#[from] io::Error),
}
