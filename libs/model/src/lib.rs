use std::{fmt::Debug, hash::Hash, path::PathBuf};

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct ScannerFileEntry {
    pub path: PathBuf,
    pub file_size: u64,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct HashedFileEntry {
    pub path: PathBuf,
    pub file_size: u64,
    pub hash: u64,
}
