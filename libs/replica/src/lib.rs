use std::collections::HashMap;

use itertools::Itertools;
use model::{HashedFileEntry, ScannerFileEntry};

use crate::{
    errors::ReplicaError,
    signature::{content_hash_file, quick_hash_file},
};

mod errors;
mod signature;

/// Groups files whose whole content is identical. Each group is keyed by its first file in input
/// order and lists the other copies.
///
/// Reading every byte of every file is the expensive part, so it only happens for files that
/// already share a size and a sampled hash with another file.
pub async fn detect_duplicates(
    files: &[ScannerFileEntry],
) -> Result<HashMap<HashedFileEntry, Vec<HashedFileEntry>>, ReplicaError> {
    let size_counts: HashMap<u64, usize> = files.iter().map(|it| it.file_size).counts();

    let mut candidates_by_sample = HashMap::<(u64, u64), Vec<HashedFileEntry>>::new();
    for ScannerFileEntry { path, file_size } in
        files.iter().filter(|it| size_counts[&it.file_size] > 1)
    {
        let hash = quick_hash_file(path, *file_size).await?;
        candidates_by_sample
            .entry((*file_size, hash))
            .or_default()
            .push(HashedFileEntry {
                path: path.clone(),
                file_size: *file_size,
                hash,
            });
    }

    let mut duplicates_registry = HashMap::<HashedFileEntry, Vec<HashedFileEntry>>::new();
    for candidates in candidates_by_sample
        .into_values()
        .filter(|candidates| candidates.len() > 1)
    {
        let mut copies_by_content = HashMap::<u128, Vec<HashedFileEntry>>::new();
        for candidate in candidates {
            let content_hash = content_hash_file(&candidate.path).await?;
            copies_by_content
                .entry(content_hash)
                .or_default()
                .push(candidate);
        }

        for mut copies in copies_by_content
            .into_values()
            .filter(|copies| copies.len() > 1)
        {
            let first_copy = copies.remove(0);
            duplicates_registry.insert(first_copy, copies);
        }
    }

    Ok(duplicates_registry)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
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

    /// Same length and same first, middle and last KiB as any other output of this function,
    /// but `filler` fills every byte the sampled hash never reads.
    fn content_sharing_sampled_chunks(filler: u8) -> Vec<u8> {
        const CHUNK: usize = 1024;
        [
            vec![0x01; CHUNK],
            vec![filler; 4 * CHUNK],
            vec![0x02; CHUNK],
            vec![filler; 4 * CHUNK],
            vec![0x03; CHUNK],
        ]
        .concat()
    }

    fn scanner_entry(path: &Path, content: &[u8]) -> ScannerFileEntry {
        ScannerFileEntry {
            path: path.to_path_buf(),
            file_size: content.len() as u64,
        }
    }

    #[tokio::test]
    async fn files_matching_only_on_sampled_chunks_are_not_duplicates()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let original = temp_dir.path().join("original");
        let lookalike = temp_dir.path().join("lookalike");
        let original_content = content_sharing_sampled_chunks(0x00);
        let lookalike_content = content_sharing_sampled_chunks(0xFF);
        create_file(&original, &original_content).await?;
        create_file(&lookalike, &lookalike_content).await?;

        let files = vec![
            scanner_entry(&original, &original_content),
            scanner_entry(&lookalike, &lookalike_content),
        ];
        let duplicates = detect_duplicates(&files).await?;

        assert!(duplicates.is_empty(), "reported {duplicates:?}");
        Ok(())
    }

    #[tokio::test]
    async fn content_splits_files_sharing_sampled_chunks() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp_dir = TempDir::new()?;
        let original = temp_dir.path().join("original");
        let copy = temp_dir.path().join("copy");
        let lookalike = temp_dir.path().join("lookalike");
        let shared_content = content_sharing_sampled_chunks(0x00);
        let lookalike_content = content_sharing_sampled_chunks(0xFF);
        create_file(&original, &shared_content).await?;
        create_file(&copy, &shared_content).await?;
        create_file(&lookalike, &lookalike_content).await?;

        let files = vec![
            scanner_entry(&original, &shared_content),
            scanner_entry(&lookalike, &lookalike_content),
            scanner_entry(&copy, &shared_content),
        ];
        let duplicates = detect_duplicates(&files).await?;

        let mut grouped_paths: Vec<Vec<PathBuf>> = duplicates
            .iter()
            .map(|(first, others)| {
                let mut paths: Vec<PathBuf> = std::iter::once(first)
                    .chain(others)
                    .map(|entry| entry.path.clone())
                    .collect();
                paths.sort();
                paths
            })
            .collect();
        grouped_paths.sort();
        let mut expected_group = vec![original, copy];
        expected_group.sort();
        assert_eq!(grouped_paths, vec![expected_group]);
        Ok(())
    }

    /// Test: Empty input returns empty result
    #[tokio::test]
    async fn empty_input() -> Result<(), Box<dyn std::error::Error>> {
        let files = Vec::new();
        let duplicates = detect_duplicates(&files).await?;
        assert!(duplicates.is_empty());
        Ok(())
    }

    /// Test: Single file returns empty result
    #[tokio::test]
    async fn single_file() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let path = temp_file.path().to_path_buf();
        create_file(&path, b"unique").await?;

        let files = vec![ScannerFileEntry { path, file_size: 6 }];
        let duplicates = detect_duplicates(&files).await?;
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

        let files = vec![
            ScannerFileEntry {
                path: file1,
                file_size: 5,
            },
            ScannerFileEntry {
                path: file2,
                file_size: 20,
            },
        ];

        let duplicates = detect_duplicates(&files).await?;
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

        let files = vec![
            ScannerFileEntry {
                path: file1,
                file_size: 9,
            },
            ScannerFileEntry {
                path: file2,
                file_size: 9,
            },
        ];

        let duplicates = detect_duplicates(&files).await?;
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

        let files = vec![
            ScannerFileEntry {
                path: file1.clone(),
                file_size: content.len() as u64,
            },
            ScannerFileEntry {
                path: file2.clone(),
                file_size: content.len() as u64,
            },
        ];

        let duplicates = detect_duplicates(&files).await?;

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

        let scanner_files: Vec<_> = files
            .iter()
            .map(|path| ScannerFileEntry {
                path: path.clone(),
                file_size: content.len() as u64,
            })
            .collect();

        let duplicates = detect_duplicates(&scanner_files).await?;

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

        let files = vec![
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

        let duplicates = detect_duplicates(&files).await?;

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

        let files = vec![
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

        let duplicates = detect_duplicates(&files).await?;

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

        let files = vec![
            ScannerFileEntry {
                path: file1,
                file_size: binary_content.len() as u64,
            },
            ScannerFileEntry {
                path: file2,
                file_size: binary_content.len() as u64,
            },
        ];

        let duplicates = detect_duplicates(&files).await?;

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

        let files = vec![
            ScannerFileEntry {
                path: file1,
                file_size: large_content.len() as u64,
            },
            ScannerFileEntry {
                path: file2,
                file_size: large_content.len() as u64,
            },
        ];

        let duplicates = detect_duplicates(&files).await?;

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
        let files = vec![
            ScannerFileEntry {
                path: PathBuf::from("/tmp/does_not_exist_12345.txt"),
                file_size: 100,
            },
            ScannerFileEntry {
                path: PathBuf::from("/tmp/does_not_exist_67890.txt"),
                file_size: 100, // Same size to ensure processing
            },
        ];

        let result = detect_duplicates(&files).await;
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

        let duplicates = detect_duplicates(&files).await?;

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
        let files = vec![
            ScannerFileEntry {
                path: file2.clone(),
                file_size: content.len() as u64,
            },
            ScannerFileEntry {
                path: file1.clone(),
                file_size: content.len() as u64,
            },
        ];

        let duplicates = detect_duplicates(&files).await?;

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
