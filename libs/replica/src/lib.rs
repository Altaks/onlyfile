use std::collections::HashMap;

use itertools::Itertools;
use model::{HashedFileEntry, ScannerFileEntry};

use crate::{errors::ReplicaError, signature::quick_hash_file};

mod errors;
mod signature;

pub async fn detect_duplicates(
    files: &mut [ScannerFileEntry],
) -> Result<HashMap<HashedFileEntry, Vec<HashedFileEntry>>, ReplicaError> {
    // Presort files list by their sizes
    glidesort::sort_by_key(files, |it| it.file_size);

    // Find files that have duplicates by size (appear more than once)
    let size_counts: HashMap<u64, usize> = files.iter().map(|it| it.file_size).counts();

    let non_unique_files = files
        .iter()
        .filter(|it| size_counts[&it.file_size] > 1)
        .collect_vec();

    let mut non_unique_hashed_files = Vec::<HashedFileEntry>::with_capacity(non_unique_files.len());
    for ScannerFileEntry { path, file_size } in non_unique_files {
        non_unique_hashed_files.push(HashedFileEntry {
            path: path.to_owned(),
            file_size: *file_size,
            hash: quick_hash_file(&path, *file_size).await?,
        });
    }

    // Find files that have duplicates by hash (appear more than once)
    let hash_counts: HashMap<u64, usize> =
        non_unique_hashed_files.iter().map(|it| it.hash).counts();

    let non_unique_grouped_files_by_size = non_unique_hashed_files
        .iter()
        .filter(|it| hash_counts[&it.hash] > 1) // Only files with duplicate hashes
        .chunk_by(|it| it.file_size);

    let non_unique_grouped_files_by_size_and_hashes = non_unique_grouped_files_by_size
        .into_iter()
        .map(|size_group| {
            size_group
                .1
                .sorted_by_key(|hashed_entry| hashed_entry.hash)
                .chunk_by(|hashed_entry| hashed_entry.hash)
        });

    let mut duplicates_registry = HashMap::<HashedFileEntry, Vec<HashedFileEntry>>::new();

    non_unique_grouped_files_by_size_and_hashes.for_each(|size_group| {
        size_group.into_iter().for_each(|hash_group| {
            let mut hash_group_iter = hash_group.1;

            if let Some(first_file_entry) = hash_group_iter.next() {
                let mut other_hashed_files = Vec::<HashedFileEntry>::new();
                for entry in hash_group_iter {
                    other_hashed_files.push(entry.clone());
                }

                duplicates_registry.insert(first_file_entry.clone(), other_hashed_files);
            }
        })
    });

    Ok(duplicates_registry)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use tempfile::{NamedTempFile, TempDir};
    use tokio::fs::File;
    use tokio::io::AsyncWriteExt;

    use super::*;

    /// Helper: Create test file with given content
    async fn create_file(path: &PathBuf, content: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        let mut file = File::create(path).await?;
        file.write_all(content).await?;
        file.flush().await?;
        Ok(())
    }

    /// Test: Empty input returns empty result
    #[tokio::test]
    async fn empty_input() -> Result<(), Box<dyn std::error::Error>> {
        let mut files = Vec::new();
        let duplicates = detect_duplicates(&mut files).await?;
        assert!(duplicates.is_empty());
        Ok(())
    }

    /// Test: Single file returns empty result
    #[tokio::test]
    async fn single_file() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let path = temp_file.path().to_path_buf();
        create_file(&path, b"unique").await?;

        let mut files = vec![ScannerFileEntry { path, file_size: 6 }];
        let duplicates = detect_duplicates(&mut files).await?;
        assert!(duplicates.is_empty());
        Ok(())
    }

    /// Test: Files with different sizes - no duplicates
    #[tokio::test]
    async fn different_sizes() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let file1 = temp_dir.path().join("file1");
        let file2 = temp_dir.path().join("file2");

        create_file(&file1, b"small").await?;
        create_file(&file2, b"much larger content").await?;

        let mut files = vec![
            ScannerFileEntry {
                path: file1,
                file_size: 5,
            },
            ScannerFileEntry {
                path: file2,
                file_size: 20,
            },
        ];

        let duplicates = detect_duplicates(&mut files).await?;
        assert!(duplicates.is_empty());
        Ok(())
    }

    /// Test: Same size, different content - no duplicates
    #[tokio::test]
    async fn same_size_different_content() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let file1 = temp_dir.path().join("file1");
        let file2 = temp_dir.path().join("file2");

        create_file(&file1, b"content A").await?;
        create_file(&file2, b"content B").await?;

        let mut files = vec![
            ScannerFileEntry {
                path: file1,
                file_size: 9,
            },
            ScannerFileEntry {
                path: file2,
                file_size: 9,
            },
        ];

        let duplicates = detect_duplicates(&mut files).await?;
        assert!(duplicates.is_empty());
        Ok(())
    }

    /// Test: Two identical files - detects one duplicate
    #[tokio::test]
    async fn two_identical_files() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let file1 = temp_dir.path().join("file1");
        let file2 = temp_dir.path().join("file2");

        let content = b"identical content";
        create_file(&file1, content).await?;
        create_file(&file2, content).await?;

        let mut files = vec![
            ScannerFileEntry {
                path: file1.clone(),
                file_size: content.len() as u64,
            },
            ScannerFileEntry {
                path: file2.clone(),
                file_size: content.len() as u64,
            },
        ];

        let duplicates = detect_duplicates(&mut files).await?;

        // With 2 identical files, should have 1 duplicate group
        assert_eq!(duplicates.len(), 1);
        let (key, dup_files) = duplicates
            .iter()
            .next()
            .expect("exactly one duplicate group");
        assert_eq!(dup_files.len(), 1); // One duplicate file

        // Verify paths are in the result
        let all_paths: Vec<_> = std::iter::once(&key.path)
            .chain(dup_files.iter().map(|f| &f.path))
            .collect();
        assert!(all_paths.contains(&&file1));
        assert!(all_paths.contains(&&file2));

        Ok(())
    }

    /// Test: Three identical files - detects two duplicates
    #[tokio::test]
    async fn three_identical_files() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let files: Vec<PathBuf> = (1..=3)
            .map(|i| temp_dir.path().join(format!("file{}", i)))
            .collect();

        let content = b"triple duplicate";
        for path in &files {
            create_file(path, content).await?;
        }

        let mut scanner_files: Vec<_> = files
            .iter()
            .map(|path| ScannerFileEntry {
                path: path.clone(),
                file_size: content.len() as u64,
            })
            .collect();

        let duplicates = detect_duplicates(&mut scanner_files).await?;

        // Should have one duplicate group
        assert_eq!(duplicates.len(), 1);
        let dup_files = duplicates
            .values()
            .next()
            .expect("exactly one duplicate group");
        assert_eq!(dup_files.len(), 2); // Two duplicate files

        Ok(())
    }

    /// Test: Multiple duplicate groups
    #[tokio::test]
    async fn multiple_duplicate_groups() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;

        // First duplicate pair
        let file1 = temp_dir.path().join("file1");
        let file2 = temp_dir.path().join("file2");
        let content1 = b"first content";

        // Second duplicate pair
        let file3 = temp_dir.path().join("file3");
        let file4 = temp_dir.path().join("file4");
        let content2 = b"second content";

        // Unique file
        let file5 = temp_dir.path().join("file5");
        let content3 = b"unique content";

        create_file(&file1, content1).await?;
        create_file(&file2, content1).await?;
        create_file(&file3, content2).await?;
        create_file(&file4, content2).await?;
        create_file(&file5, content3).await?;

        let mut files = vec![
            ScannerFileEntry {
                path: file1,
                file_size: content1.len() as u64,
            },
            ScannerFileEntry {
                path: file2,
                file_size: content1.len() as u64,
            },
            ScannerFileEntry {
                path: file3,
                file_size: content2.len() as u64,
            },
            ScannerFileEntry {
                path: file4,
                file_size: content2.len() as u64,
            },
            ScannerFileEntry {
                path: file5,
                file_size: content3.len() as u64,
            },
        ];

        let duplicates = detect_duplicates(&mut files).await?;

        // Should have two duplicate groups
        assert_eq!(duplicates.len(), 2);

        // Each group should have exactly one duplicate
        for dup_files in duplicates.values() {
            assert_eq!(dup_files.len(), 1);
        }

        Ok(())
    }

    /// Test: Empty files as duplicates
    #[tokio::test]
    async fn empty_file_duplicates() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let file1 = temp_dir.path().join("empty1");
        let file2 = temp_dir.path().join("empty2");
        let file3 = temp_dir.path().join("nonempty");

        create_file(&file1, b"").await?;
        create_file(&file2, b"").await?;
        create_file(&file3, b"content").await?;

        let mut files = vec![
            ScannerFileEntry {
                path: file1,
                file_size: 0,
            },
            ScannerFileEntry {
                path: file2,
                file_size: 0,
            },
            ScannerFileEntry {
                path: file3,
                file_size: 7,
            },
        ];

        let duplicates = detect_duplicates(&mut files).await?;

        // Should detect empty files as duplicates
        assert_eq!(duplicates.len(), 1);
        let (key, dup_files) = duplicates
            .iter()
            .next()
            .expect("exactly one duplicate group");
        assert_eq!(key.file_size, 0);
        assert_eq!(dup_files.len(), 1);

        Ok(())
    }

    /// Test: Binary content duplicates
    #[tokio::test]
    async fn binary_content_duplicates() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let file1 = temp_dir.path().join("binary1");
        let file2 = temp_dir.path().join("binary2");

        let binary_content = vec![0x00, 0x01, 0x02, 0xFF, 0xFE];

        create_file(&file1, &binary_content).await?;
        create_file(&file2, &binary_content).await?;

        let mut files = vec![
            ScannerFileEntry {
                path: file1,
                file_size: binary_content.len() as u64,
            },
            ScannerFileEntry {
                path: file2,
                file_size: binary_content.len() as u64,
            },
        ];

        let duplicates = detect_duplicates(&mut files).await?;

        // Should detect binary files as duplicates
        assert_eq!(duplicates.len(), 1);
        let dup_files = duplicates
            .values()
            .next()
            .expect("exactly one duplicate group");
        assert_eq!(dup_files.len(), 1);

        Ok(())
    }

    /// Test: Large files (bigger than FILE_CHUNK_SIZE)
    #[tokio::test]
    async fn large_file_duplicates() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let file1 = temp_dir.path().join("large1");
        let file2 = temp_dir.path().join("large2");

        // Create content larger than FILE_CHUNK_SIZE (1024)
        let large_content = vec![0x42u8; 4096];

        create_file(&file1, &large_content).await?;
        create_file(&file2, &large_content).await?;

        let mut files = vec![
            ScannerFileEntry {
                path: file1,
                file_size: large_content.len() as u64,
            },
            ScannerFileEntry {
                path: file2,
                file_size: large_content.len() as u64,
            },
        ];

        let duplicates = detect_duplicates(&mut files).await?;

        // Should detect large files as duplicates
        assert_eq!(duplicates.len(), 1);
        let dup_files = duplicates
            .values()
            .next()
            .expect("exactly one duplicate group");
        assert_eq!(dup_files.len(), 1);

        Ok(())
    }

    /// Test: Error handling for non-existent file
    #[tokio::test]
    async fn non_existent_file_error() {
        let mut files = vec![
            ScannerFileEntry {
                path: PathBuf::from("/tmp/does_not_exist_12345.txt"),
                file_size: 100,
            },
            ScannerFileEntry {
                path: PathBuf::from("/tmp/does_not_exist_67890.txt"),
                file_size: 100, // Same size to ensure processing
            },
        ];

        let result = detect_duplicates(&mut files).await;
        assert!(matches!(result, Err(ReplicaError::FileSystemError(_))));
    }

    /// Test: Mixed scenario with duplicates and unique files
    #[tokio::test]
    async fn mixed_scenario() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;

        // Create multiple files with different duplication patterns
        let scenarios: Vec<(&str, &str, &[u8])> = vec![
            ("dup1_a", "dup1_b", b"duplicate content 1"),
            ("dup2_a", "dup2_b", b"duplicate content 2"),
            ("unique1", "", b"unique content A"),
            ("unique2", "", b"unique content B"),
        ];

        let mut files = Vec::new();

        for (name1, name2, content) in scenarios {
            let path1 = temp_dir.path().join(name1);
            create_file(&path1, content).await?;
            files.push(ScannerFileEntry {
                path: path1,
                file_size: content.len() as u64,
            });

            if !name2.is_empty() {
                let path2 = temp_dir.path().join(name2);
                create_file(&path2, content).await?;
                files.push(ScannerFileEntry {
                    path: path2,
                    file_size: content.len() as u64,
                });
            }
        }

        let duplicates = detect_duplicates(&mut files).await?;

        // Should have exactly 2 duplicate groups
        assert_eq!(duplicates.len(), 2);

        // Each group should have exactly one duplicate file
        for dup_files in duplicates.values() {
            assert_eq!(dup_files.len(), 1);
        }

        Ok(())
    }

    /// Test: Function works regardless of input order (it sorts internally)
    #[tokio::test]
    async fn input_order_independence() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let file1 = temp_dir.path().join("file1");
        let file2 = temp_dir.path().join("file2");

        let content = b"test content";
        create_file(&file1, content).await?;
        create_file(&file2, content).await?;

        // Test with files in reverse order
        let mut files = vec![
            ScannerFileEntry {
                path: file2.clone(),
                file_size: content.len() as u64,
            },
            ScannerFileEntry {
                path: file1.clone(),
                file_size: content.len() as u64,
            },
        ];

        let duplicates = detect_duplicates(&mut files).await?;

        // Should still detect duplicates correctly
        assert_eq!(duplicates.len(), 1);
        let dup_files = duplicates
            .values()
            .next()
            .expect("exactly one duplicate group");
        assert_eq!(dup_files.len(), 1);

        Ok(())
    }
}
