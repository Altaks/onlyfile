use std::path::Path;

use crate::errors::ScannerError;
use model::ScannerFileEntry;

mod errors;

pub async fn crawl_and_collect_files(
    path: impl AsRef<Path>,
) -> Result<Vec<ScannerFileEntry>, ScannerError> {
    let mut files_collector = Vec::<ScannerFileEntry>::new();
    let mut stack = Vec::new();
    stack.push(path.as_ref().to_path_buf());

    while let Some(current_path) = stack.pop() {
        let mut directory_entries = tokio::fs::read_dir(&current_path).await?;

        while let Ok(Some(entry)) = directory_entries.next_entry().await {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path);
            } else {
                let file_size = entry.metadata().await?.len();

                files_collector.push(ScannerFileEntry {
                    path: entry_path,
                    file_size,
                });
            }
        }
    }

    Ok(files_collector)
}

#[cfg(test)]
mod tests {

    use model::ScannerFileEntry;
    use tempfile::{TempDir, tempdir};

    use crate::crawl_and_collect_files;

    #[tokio::test]
    async fn test_list_files_in_empty_folder() -> anyhow::Result<()> {
        let tempdir = tempdir()?;
        let files = crawl_and_collect_files(tempdir.path()).await?;
        assert!(files.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_list_files_in_non_empty_folder() -> anyhow::Result<()> {
        let temp_dir = TempDir::new()?;
        let file_path = temp_dir.path().join("test.txt");

        tokio::fs::write(&file_path, "contenu").await?;

        let files = crawl_and_collect_files(temp_dir.path()).await?;
        assert_eq!(files.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_list_files_in_a_folder_with_multiple_files() -> anyhow::Result<()> {
        let temp_dir = TempDir::new()?;

        const AMOUNT_OF_FILES: usize = 10;

        for idx in 0..AMOUNT_OF_FILES {
            let file_path = temp_dir.path().join(format!("content_{}.txt", idx));
            tokio::fs::write(&file_path, format!("content-{}", idx)).await?;
        }

        let files = crawl_and_collect_files(temp_dir.path()).await?;
        assert_eq!(files.len(), AMOUNT_OF_FILES);
        Ok(())
    }

    #[tokio::test]
    async fn test_list_files_in_a_folder_with_multiple_nested_files() -> anyhow::Result<()> {
        let root_dir = TempDir::new()?;

        const AMOUNT_OF_BRANCHES: usize = 10;
        const AMOUNT_OF_LEAFS: usize = 10;

        for idx in 0..AMOUNT_OF_BRANCHES {
            let current_dir_path = root_dir.path().join(format!("dir_{}", idx));
            tokio::fs::create_dir(&current_dir_path).await?;

            for inner_idx in 0..AMOUNT_OF_LEAFS {
                let file_path = current_dir_path.join(format!("content-{}.tmp", inner_idx));
                tokio::fs::write(&file_path, format!("content-{}", inner_idx)).await?;
            }
        }

        let files = crawl_and_collect_files(root_dir.path()).await?;
        assert_eq!(files.len(), AMOUNT_OF_BRANCHES * AMOUNT_OF_LEAFS);
        Ok(())
    }

    #[tokio::test]
    async fn test_aggregates_files_sizes_correctly() -> anyhow::Result<()> {
        let root_dir = TempDir::new()?;

        tokio::fs::write(root_dir.path().join("file1.tmp"), "Some content").await?;
        tokio::fs::write(root_dir.path().join("file2.tmp"), "Some content 2").await?;

        let mut listed_files = crawl_and_collect_files(root_dir.path()).await?;
        // read_dir order is filesystem-defined, so compare in path order.
        listed_files.sort_by(|left, right| left.path.cmp(&right.path));
        assert_eq!(
            listed_files[0],
            ScannerFileEntry {
                path: root_dir.path().join("file1.tmp"),
                file_size: 12u64,
            }
        );
        assert_eq!(
            listed_files[1],
            ScannerFileEntry {
                path: root_dir.path().join("file2.tmp"),
                file_size: 14u64,
            }
        );

        Ok(())
    }
}
