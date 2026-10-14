use std::path::Path;

use model::ScannerFileEntry;

pub use crate::errors::ScannerError;

mod errors;

/// Lists every file below `path`. `on_file_found` receives the number of files found so far after
/// each new one, so a caller can show progress while a large tree is walked.
pub fn crawl_and_collect_files(
    path: impl AsRef<Path>,
    mut on_file_found: impl FnMut(usize),
) -> Result<Vec<ScannerFileEntry>, ScannerError> {
    let mut files_collector = Vec::<ScannerFileEntry>::new();
    let mut stack = vec![path.as_ref().to_path_buf()];

    while let Some(current_path) = stack.pop() {
        for entry in std::fs::read_dir(&current_path)? {
            let entry = entry?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path);
            } else {
                let metadata = entry.metadata()?;
                files_collector.push(ScannerFileEntry {
                    path: entry_path,
                    file_size: metadata.len(),
                    modified: metadata.modified()?,
                });
                on_file_found(files_collector.len());
            }
        }
    }

    Ok(files_collector)
}

#[cfg(test)]
mod tests {
    use std::{
        fs::File,
        time::{Duration, SystemTime},
    };

    use model::ScannerFileEntry;
    use tempfile::{TempDir, tempdir};

    use crate::crawl_and_collect_files;

    #[test]
    fn test_list_files_in_empty_folder() -> anyhow::Result<()> {
        let tempdir = tempdir()?;
        let files = crawl_and_collect_files(tempdir.path(), |_| {})?;
        assert!(files.is_empty());
        Ok(())
    }

    #[test]
    fn test_list_files_in_non_empty_folder() -> anyhow::Result<()> {
        let temp_dir = TempDir::new()?;
        let file_path = temp_dir.path().join("test.txt");

        std::fs::write(&file_path, "contenu")?;

        let files = crawl_and_collect_files(temp_dir.path(), |_| {})?;
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

        let files = crawl_and_collect_files(temp_dir.path(), |_| {})?;
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

        let files = crawl_and_collect_files(root_dir.path(), |_| {})?;
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

        let mut listed_files = crawl_and_collect_files(root_dir.path(), |_| {})?;
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
}
