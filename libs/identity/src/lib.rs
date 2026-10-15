//! Which file on disk a path or an open handle names, so two names of one file are recognised:
//! hardlinks, or a path reached through a symlinked folder.

use std::{fs::File, io, path::Path};

/// Equal for every name of one file: the same volume, the same index on that volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    volume: u64,
    index: u64,
}

#[cfg(unix)]
impl FileIdentity {
    pub fn of_path(path: &Path) -> io::Result<Self> {
        std::fs::metadata(path).map(|metadata| Self::of_metadata(&metadata))
    }

    pub fn of_file(file: &File) -> io::Result<Self> {
        file.metadata().map(|metadata| Self::of_metadata(&metadata))
    }

    fn of_metadata(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;

        Self {
            volume: metadata.dev(),
            index: metadata.ino(),
        }
    }
}

/// Windows only reports the volume and the index through an open handle.
#[cfg(windows)]
impl FileIdentity {
    pub fn of_path(path: &Path) -> io::Result<Self> {
        Self::of_file(&File::open(path)?)
    }

    pub fn of_file(file: &File) -> io::Result<Self> {
        let information = winapi_util::file::information(file)?;
        Ok(Self {
            volume: information.volume_serial_number(),
            index: information.file_index(),
        })
    }
}

#[cfg(not(any(unix, windows)))]
compile_error!("onlyfile recognises one file under two names on Unix and Windows only");

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn two_names_of_one_file_share_an_identity() -> io::Result<()> {
        let folder = TempDir::new()?;
        let original = folder.path().join("original");
        let other_name = folder.path().join("other_name");
        std::fs::write(&original, b"content")?;
        std::fs::hard_link(&original, &other_name)?;

        assert_eq!(
            FileIdentity::of_path(&original)?,
            FileIdentity::of_path(&other_name)?
        );
        Ok(())
    }

    #[test]
    fn two_files_with_the_same_content_do_not() -> io::Result<()> {
        let folder = TempDir::new()?;
        let first = folder.path().join("first");
        let second = folder.path().join("second");
        std::fs::write(&first, b"content")?;
        std::fs::write(&second, b"content")?;

        assert_ne!(
            FileIdentity::of_path(&first)?,
            FileIdentity::of_path(&second)?
        );
        Ok(())
    }

    #[test]
    fn a_path_and_its_open_handle_agree() -> io::Result<()> {
        let folder = TempDir::new()?;
        let path = folder.path().join("file");
        std::fs::write(&path, b"content")?;

        assert_eq!(
            FileIdentity::of_path(&path)?,
            FileIdentity::of_file(&File::open(&path)?)?
        );
        Ok(())
    }
}
