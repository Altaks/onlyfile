use std::{
    fs::{DirEntry, ReadDir},
    path::{Path, PathBuf},
};

use model::{ScannerFileEntry, SkipReason, SkippedEntry};

pub use crate::errors::ScannerError;

mod errors;

/// What a walk found: the files worth comparing, and every entry left out with the reason.
#[derive(Debug, Default)]
pub struct ScanReport {
    pub files: Vec<ScannerFileEntry>,
    pub skipped: Vec<SkippedEntry>,
}

/// Lists every regular, non-empty file below `root`. Symlinks are never followed. A subfolder or
/// an entry that cannot be read is recorded in `skipped` and the walk goes on; only an unreadable
/// `root` is an error.
///
/// `on_file_found` receives the number of files found so far after each new one, so a caller can
/// show progress while a large tree is walked.
pub fn crawl_and_collect_files(
    root: impl AsRef<Path>,
    on_file_found: impl FnMut(usize),
) -> Result<ScanReport, ScannerError> {
    let root = root.as_ref();
    let root_listing = std::fs::read_dir(root)?;

    let mut crawler = Crawler {
        report: ScanReport::default(),
        pending_folders: Vec::new(),
        on_file_found,
    };
    crawler.list(root, root_listing);
    // Folders wait as paths rather than open listings, so a wide tree never holds thousands of
    // folder handles at once.
    while let Some(folder) = crawler.pending_folders.pop() {
        match std::fs::read_dir(&folder) {
            Ok(listing) => crawler.list(&folder, listing),
            Err(error) => crawler.skip(folder, SkipReason::Unreadable(error.kind())),
        }
    }

    Ok(crawler.report)
}

struct Crawler<OnFileFound> {
    report: ScanReport,
    pending_folders: Vec<PathBuf>,
    on_file_found: OnFileFound,
}

impl<OnFileFound: FnMut(usize)> Crawler<OnFileFound> {
    fn list(&mut self, folder: &Path, listing: ReadDir) {
        for entry in listing {
            match entry {
                Ok(entry) => self.visit(entry),
                // The listing failed partway, and the entry it could not read has no name yet.
                Err(error) => self.skip(folder.to_path_buf(), SkipReason::Unreadable(error.kind())),
            }
        }
    }

    fn visit(&mut self, entry: DirEntry) {
        let path = entry.path();
        // The entry's own type: a symlink reports itself, not what it points to.
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => return self.skip(path, SkipReason::Unreadable(error.kind())),
        };

        if file_type.is_symlink() {
            self.skip(path, SkipReason::Symlink);
        } else if file_type.is_dir() {
            self.pending_folders.push(path);
        } else if file_type.is_file() {
            self.collect(path, &entry);
        } else {
            self.skip(path, SkipReason::SpecialFile);
        }
    }

    fn collect(&mut self, path: PathBuf, entry: &DirEntry) {
        let size_and_modified = entry
            .metadata()
            .and_then(|metadata| Ok((metadata.len(), metadata.modified()?)));
        let (file_size, modified) = match size_and_modified {
            Ok(size_and_modified) => size_and_modified,
            Err(error) => return self.skip(path, SkipReason::Unreadable(error.kind())),
        };
        if file_size == 0 {
            return self.skip(path, SkipReason::EmptyFile);
        }

        self.report.files.push(ScannerFileEntry {
            path,
            file_size,
            modified,
        });
        (self.on_file_found)(self.report.files.len());
    }

    fn skip(&mut self, path: PathBuf, reason: SkipReason) {
        self.report.skipped.push(SkippedEntry { path, reason });
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::File,
        io,
        path::Path,
        time::{Duration, SystemTime},
    };

    use model::{ScannerFileEntry, SkipReason, SkippedEntry};
    use tempfile::{TempDir, tempdir};

    use crate::{ScannerError, crawl_and_collect_files};

    #[test]
    fn test_list_files_in_empty_folder() -> anyhow::Result<()> {
        let tempdir = tempdir()?;
        let files = crawl_and_collect_files(tempdir.path(), |_| {})?.files;
        assert!(files.is_empty());
        Ok(())
    }

    #[test]
    fn test_list_files_in_non_empty_folder() -> anyhow::Result<()> {
        let temp_dir = TempDir::new()?;
        let file_path = temp_dir.path().join("test.txt");

        std::fs::write(&file_path, "contenu")?;

        let files = crawl_and_collect_files(temp_dir.path(), |_| {})?.files;
        assert_eq!(files.len(), 1);
        Ok(())
    }

    #[test]
    fn test_list_files_in_a_folder_with_multiple_files() -> anyhow::Result<()> {
        let temp_dir = TempDir::new()?;

        const AMOUNT_OF_FILES: usize = 10;

        for idx in 0..AMOUNT_OF_FILES {
            let file_path = temp_dir.path().join(format!("content_{}.txt", idx));
            std::fs::write(&file_path, format!("content-{}", idx))?;
        }

        let files = crawl_and_collect_files(temp_dir.path(), |_| {})?.files;
        assert_eq!(files.len(), AMOUNT_OF_FILES);
        Ok(())
    }

    #[test]
    fn test_list_files_in_a_folder_with_multiple_nested_files() -> anyhow::Result<()> {
        let root_dir = TempDir::new()?;

        const AMOUNT_OF_BRANCHES: usize = 10;
        const AMOUNT_OF_LEAFS: usize = 10;

        for idx in 0..AMOUNT_OF_BRANCHES {
            let current_dir_path = root_dir.path().join(format!("dir_{}", idx));
            std::fs::create_dir(&current_dir_path)?;

            for inner_idx in 0..AMOUNT_OF_LEAFS {
                let file_path = current_dir_path.join(format!("content-{}.tmp", inner_idx));
                std::fs::write(&file_path, format!("content-{}", inner_idx))?;
            }
        }

        let files = crawl_and_collect_files(root_dir.path(), |_| {})?.files;
        assert_eq!(files.len(), AMOUNT_OF_BRANCHES * AMOUNT_OF_LEAFS);
        Ok(())
    }

    #[test]
    fn reports_the_running_count_of_files_found() -> anyhow::Result<()> {
        let root_dir = TempDir::new()?;
        let nested_dir = root_dir.path().join("nested");
        std::fs::create_dir(&nested_dir)?;
        std::fs::write(root_dir.path().join("first.tmp"), "first")?;
        std::fs::write(nested_dir.join("second.tmp"), "second")?;
        std::fs::write(nested_dir.join("third.tmp"), "third")?;

        let mut reported_counts = Vec::new();
        crawl_and_collect_files(root_dir.path(), |files_found| {
            reported_counts.push(files_found)
        })?;

        assert_eq!(reported_counts, [1, 2, 3]);
        Ok(())
    }

    #[test]
    fn records_each_file_size_and_modification_time() -> anyhow::Result<()> {
        let root_dir = TempDir::new()?;
        let first_modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let second_modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        let first_path = root_dir.path().join("file1.tmp");
        let second_path = root_dir.path().join("file2.tmp");
        std::fs::write(&first_path, "Some content")?;
        std::fs::write(&second_path, "Some content 2")?;
        File::options()
            .write(true)
            .open(&first_path)?
            .set_modified(first_modified)?;
        File::options()
            .write(true)
            .open(&second_path)?
            .set_modified(second_modified)?;

        let mut listed_files = crawl_and_collect_files(root_dir.path(), |_| {})?.files;
        // read_dir order is filesystem-defined, so compare in path order.
        listed_files.sort_by(|left, right| left.path.cmp(&right.path));

        assert_eq!(
            listed_files,
            [
                ScannerFileEntry {
                    path: first_path,
                    file_size: 12,
                    modified: first_modified,
                },
                ScannerFileEntry {
                    path: second_path,
                    file_size: 14,
                    modified: second_modified,
                },
            ]
        );
        Ok(())
    }

    fn sorted_skips(mut skipped: Vec<SkippedEntry>) -> Vec<SkippedEntry> {
        skipped.sort_by(|left, right| left.path.cmp(&right.path));
        skipped
    }

    fn file_paths(files: &[ScannerFileEntry]) -> Vec<&Path> {
        files.iter().map(|file| file.path.as_path()).collect()
    }

    #[test]
    fn empty_files_are_skipped() -> anyhow::Result<()> {
        let root = TempDir::new()?;
        std::fs::write(root.path().join("empty"), "")?;
        std::fs::write(root.path().join("full"), "x")?;

        let report = crawl_and_collect_files(root.path(), |_| {})?;

        assert_eq!(file_paths(&report.files), [root.path().join("full")]);
        assert_eq!(
            report.skipped,
            [SkippedEntry {
                path: root.path().join("empty"),
                reason: SkipReason::EmptyFile,
            }]
        );
        Ok(())
    }

    #[cfg(unix)]
    fn symlink_folder(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(unix)]
    fn symlink_file(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn symlink_folder(target: &Path, link: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    #[cfg(windows)]
    fn symlink_file(target: &Path, link: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    /// On Windows, creating a symlink needs administrator rights or developer mode, which the CI
    /// runners have.
    #[test]
    fn symlinks_are_skipped_and_never_followed() -> anyhow::Result<()> {
        let root = TempDir::new()?;
        let real = root.path().join("real");
        std::fs::create_dir(&real)?;
        std::fs::write(real.join("file.txt"), "content")?;
        symlink_folder(&real, &root.path().join("link_to_folder"))?;
        symlink_file(&real.join("file.txt"), &root.path().join("link_to_file"))?;
        // Followed, this one would walk the tree again and again.
        symlink_folder(root.path(), &real.join("link_to_root"))?;

        let report = crawl_and_collect_files(root.path(), |_| {})?;

        assert_eq!(file_paths(&report.files), [real.join("file.txt")]);
        assert_eq!(
            sorted_skips(report.skipped),
            [
                SkippedEntry {
                    path: root.path().join("link_to_file"),
                    reason: SkipReason::Symlink,
                },
                SkippedEntry {
                    path: root.path().join("link_to_folder"),
                    reason: SkipReason::Symlink,
                },
                SkippedEntry {
                    path: real.join("link_to_root"),
                    reason: SkipReason::Symlink,
                },
            ]
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn special_files_are_skipped() -> anyhow::Result<()> {
        use std::os::unix::net::UnixListener;

        let root = TempDir::new()?;
        let socket_path = root.path().join("socket");
        let _listener = UnixListener::bind(&socket_path)?;

        let report = crawl_and_collect_files(root.path(), |_| {})?;

        assert!(report.files.is_empty());
        assert_eq!(
            report.skipped,
            [SkippedEntry {
                path: socket_path,
                reason: SkipReason::SpecialFile,
            }]
        );
        Ok(())
    }

    /// Needs a user that permissions apply to: root reads the locked folder anyway.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_is_skipped_with_its_reason_and_the_scan_goes_on() -> anyhow::Result<()>
    {
        use std::{fs::Permissions, os::unix::fs::PermissionsExt};

        const NO_ACCESS: u32 = 0o000;
        const OWNER_FULL_ACCESS: u32 = 0o700;

        let root = TempDir::new()?;
        let locked = root.path().join("locked");
        let open = root.path().join("open");
        std::fs::create_dir(&locked)?;
        std::fs::create_dir(&open)?;
        std::fs::write(locked.join("hidden"), "hidden")?;
        std::fs::write(open.join("visible"), "visible")?;
        std::fs::set_permissions(&locked, Permissions::from_mode(NO_ACCESS))?;

        let report = crawl_and_collect_files(root.path(), |_| {});
        // Restore access first so the temporary folder can always be deleted.
        std::fs::set_permissions(&locked, Permissions::from_mode(OWNER_FULL_ACCESS))?;
        let report = report?;

        assert_eq!(file_paths(&report.files), [open.join("visible")]);
        assert_eq!(
            report.skipped,
            [SkippedEntry {
                path: locked,
                reason: SkipReason::Unreadable(io::ErrorKind::PermissionDenied),
            }]
        );
        Ok(())
    }

    #[test]
    fn a_missing_root_is_an_error() -> anyhow::Result<()> {
        let parent = TempDir::new()?;

        let result = crawl_and_collect_files(parent.path().join("missing"), |_| {});

        assert!(
            matches!(&result, Err(ScannerError::FileSystemError(error)) if error.kind() == io::ErrorKind::NotFound),
            "{result:?}"
        );
        Ok(())
    }
}
