use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use crate::errors::ScannerError;

mod errors;

#[derive(Debug, PartialEq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub file_size: u64,
    pub hash: Option<blake3::Hash>,
}

pub async fn list_all_non_unique_files_in_folder(
    path: impl AsRef<Path>,
) -> Result<Vec<FileEntry>, ScannerError> {
    let mut collector = Vec::new();
    let mut stack = Vec::new();
    stack.push(path.as_ref().to_path_buf());

    while let Some(current_path) = stack.pop() {
        let mut directory_entries = tokio::fs::read_dir(&current_path).await?;

        while let Ok(Some(entry)) = directory_entries.next_entry().await {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path);
            } else {
                let file_size = get_file_size(&entry_path).await?;

                collector.push(FileEntry {
                    path: entry_path,
                    file_size: file_size,
                    hash: None,
                });
            }
        }
    }

    Ok(collector)
}

async fn remove_all_unique_files_from_list(files_list: &mut Vec<FileEntry>) {
    // Prepare a by-size bloom filter
    // Prepare a by-hash bloom filter

    // use the by-size bloom filter first,
    // use the by-hash bloom filter next
}

async fn hash_file(path: impl AsRef<Path>) -> Result<blake3::Hash, ScannerError> {
    let file_content = tokio::fs::read(path).await?;
    let hash = blake3::hash(&file_content);
    Ok(hash)
}

async fn get_file_size(path: impl AsRef<Path>) -> Result<u64, ScannerError> {
    let file_metadata = tokio::fs::metadata(path).await?;
    let file_size = file_metadata.size();
    Ok(file_size)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::{TempDir, tempdir};

    use crate::{FileEntry, list_all_non_unique_files_in_folder};

    #[tokio::test]
    async fn test_list_files_in_empty_folder() -> anyhow::Result<()> {
        let tempdir = tempdir()?;
        let files = list_all_non_unique_files_in_folder(tempdir.path()).await?;
        assert!(files.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_list_files_in_non_empty_folder() -> anyhow::Result<()> {
        let temp_dir = TempDir::new()?;
        let file_path = temp_dir.path().join("test.txt");

        tokio::fs::write(&file_path, "contenu").await?;

        let files = list_all_non_unique_files_in_folder(temp_dir.path()).await?;
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

        let files = list_all_non_unique_files_in_folder(temp_dir.path()).await?;
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

        let files = list_all_non_unique_files_in_folder(root_dir.path()).await?;
        assert_eq!(files.len(), AMOUNT_OF_BRANCHES * AMOUNT_OF_LEAFS);
        Ok(())
    }

    #[tokio::test]
    async fn test_aggregates_files_sizes_correctly() -> anyhow::Result<()> {
        let root_dir = TempDir::new()?;

        tokio::fs::write(root_dir.path().join("file1.tmp"), "Some content").await?;
        tokio::fs::write(root_dir.path().join("file2.tmp"), "Some content 2").await?;

        let listed_files = list_all_non_unique_files_in_folder(root_dir.path()).await?;
        assert_eq!(
            listed_files[0],
            FileEntry {
                path: root_dir.path().join("file1.tmp"),
                file_size: 12u64,
                hash: None
            }
        );
        assert_eq!(
            listed_files[1],
            FileEntry {
                path: root_dir.path().join("file2.tmp"),
                file_size: 14u64,
                hash: None
            }
        );

        Ok(())
    }
}
