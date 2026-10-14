//! What makes two paths the same file on disk, so hardlinks are counted once.

use std::{io, path::Path};

/// Equal for every name of one file: the same volume, the same index on that volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FileIdentity {
    volume: u64,
    index: u64,
}

#[cfg(unix)]
pub(crate) fn file_identity(path: &Path) -> io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path)?;
    Ok(FileIdentity {
        volume: metadata.dev(),
        index: metadata.ino(),
    })
}

/// Opens the file: Windows only reports the volume and the index through a handle.
#[cfg(windows)]
pub(crate) fn file_identity(path: &Path) -> io::Result<FileIdentity> {
    let information = winapi_util::file::information(std::fs::File::open(path)?)?;
    Ok(FileIdentity {
        volume: information.volume_serial_number(),
        index: information.file_index(),
    })
}

#[cfg(not(any(unix, windows)))]
compile_error!("onlyfile recognises hardlinks on Unix and Windows only");
