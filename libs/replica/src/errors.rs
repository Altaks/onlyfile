use std::io;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ReplicaError {
    #[error("File system error : {0}")]
    FileSystemError(#[from] io::Error),
}
