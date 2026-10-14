use std::collections::HashMap;

use itertools::Itertools;
use model::{DuplicateGroup, ScannerFileEntry};

use crate::{
    errors::ReplicaError,
    signature::{content_hash_file, quick_hash_file},
};

mod errors;
mod signature;

/// How far [`detect_duplicates`] has gone. Each phase reports its totals as soon as it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonProgress {
    /// Reading the first, middle and last KiB of every file sharing its size with another.
    Sampling {
        sampled_files: usize,
        total_files: usize,
    },
    /// Reading the whole content of every file sharing its size and samples with another.
    Hashing {
        hashed_files: usize,
        total_files: usize,
        hashed_bytes: u64,
        total_bytes: u64,
    },
}

/// Finds every set of files whose whole content is identical. Groups come in no particular order;
/// the copies inside a group keep the input order.
///
/// Reading every byte of every file is the expensive part, so it only happens for files that
/// already share a size and a sampled hash with another file. `on_progress` is called once when
/// each phase starts and once after each file.
pub fn detect_duplicates(
    files: &[ScannerFileEntry],
    mut on_progress: impl FnMut(ComparisonProgress),
) -> Result<Vec<DuplicateGroup>, ReplicaError> {
    let size_counts: HashMap<u64, usize> = files.iter().map(|it| it.file_size).counts();
    let size_candidates = files
        .iter()
        .filter(|it| size_counts[&it.file_size] > 1)
        .collect_vec();

    on_progress(ComparisonProgress::Sampling {
        sampled_files: 0,
        total_files: size_candidates.len(),
    });
    let mut candidates_by_sample = HashMap::<(u64, u64), Vec<&ScannerFileEntry>>::new();
    for (sampled_files, file) in (1..).zip(&size_candidates) {
        let sampled_hash = quick_hash_file(&file.path, file.file_size)?;
        candidates_by_sample
            .entry((file.file_size, sampled_hash))
            .or_default()
            .push(file);
        on_progress(ComparisonProgress::Sampling {
            sampled_files,
            total_files: size_candidates.len(),
        });
    }

    let content_candidates = candidates_by_sample
        .into_values()
        .filter(|candidates| candidates.len() > 1)
        .collect_vec();
    let total_files = content_candidates.iter().map(Vec::len).sum();
    let total_bytes = content_candidates
        .iter()
        .flatten()
        .map(|candidate| candidate.file_size)
        .sum();
    let mut hashed_files = 0;
    let mut hashed_bytes = 0;
    on_progress(ComparisonProgress::Hashing {
        hashed_files,
        total_files,
        hashed_bytes,
        total_bytes,
    });

    let mut duplicate_groups = Vec::new();
    for candidates in content_candidates {
        let mut copies_by_content = HashMap::<u128, Vec<ScannerFileEntry>>::new();
        for candidate in candidates {
            copies_by_content
                .entry(content_hash_file(&candidate.path)?)
                .or_default()
                .push(candidate.clone());
            hashed_files += 1;
            hashed_bytes += candidate.file_size;
            on_progress(ComparisonProgress::Hashing {
                hashed_files,
                total_files,
                hashed_bytes,
                total_bytes,
            });
        }

        duplicate_groups.extend(
            copies_by_content
                .into_values()
                .filter_map(DuplicateGroup::new),
        );
    }

    Ok(duplicate_groups)
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        time::SystemTime,
    };

    use tempfile::TempDir;

    use super::*;

    /// Writes each `(name, content)` under `directory` and returns the scanner entries in the
    /// same order.
    fn write_files(
        directory: &Path,
        files: &[(&str, &[u8])],
    ) -> std::io::Result<Vec<ScannerFileEntry>> {
        files
            .iter()
            .map(|(name, content)| {
                let path = directory.join(name);
                std::fs::write(&path, content)?;
                Ok(ScannerFileEntry {
                    modified: std::fs::metadata(&path)?.modified()?,
                    path,
                    file_size: content.len() as u64,
                })
            })
            .collect()
    }

    /// The paths of every group, sorted inside each group and across groups, so the comparison
    /// does not depend on hash map order.
    fn grouped_paths(groups: &[DuplicateGroup]) -> Vec<Vec<PathBuf>> {
        groups
            .iter()
            .map(|group| {
                group
                    .copies()
                    .iter()
                    .map(|copy| copy.path.clone())
                    .sorted()
                    .collect()
            })
            .sorted()
            .collect()
    }

    fn paths_under(directory: &Path, names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|name| directory.join(name)).collect()
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

    #[test]
    fn empty_input_has_no_duplicates() -> Result<(), Box<dyn std::error::Error>> {
        assert!(detect_duplicates(&[], |_| {})?.is_empty());
        Ok(())
    }

    #[test]
    fn a_single_file_has_no_duplicates() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let files = write_files(temp_dir.path(), &[("unique", b"unique")])?;

        assert!(detect_duplicates(&files, |_| {})?.is_empty());
        Ok(())
    }

    #[test]
    fn files_of_different_sizes_are_not_duplicates() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let files = write_files(
            temp_dir.path(),
            &[("small", b"small"), ("large", b"much larger content")],
        )?;

        assert!(detect_duplicates(&files, |_| {})?.is_empty());
        Ok(())
    }

    #[test]
    fn same_size_with_different_content_is_not_a_duplicate()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let files = write_files(
            temp_dir.path(),
            &[("first", b"content A"), ("second", b"content B")],
        )?;

        assert!(detect_duplicates(&files, |_| {})?.is_empty());
        Ok(())
    }

    #[test]
    fn two_identical_files_form_one_group() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let files = write_files(
            temp_dir.path(),
            &[
                ("first", b"identical content"),
                ("second", b"identical content"),
            ],
        )?;

        assert_eq!(
            grouped_paths(&detect_duplicates(&files, |_| {})?),
            [paths_under(temp_dir.path(), &["first", "second"])]
        );
        Ok(())
    }

    #[test]
    fn three_identical_files_form_one_group() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let content: &[u8] = b"triple duplicate";
        let files = write_files(
            temp_dir.path(),
            &[("file1", content), ("file2", content), ("file3", content)],
        )?;

        assert_eq!(
            grouped_paths(&detect_duplicates(&files, |_| {})?),
            [paths_under(temp_dir.path(), &["file1", "file2", "file3"])]
        );
        Ok(())
    }

    #[test]
    fn each_content_forms_its_own_group_and_unique_files_are_left_out()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let files = write_files(
            temp_dir.path(),
            &[
                ("first_a", b"first content"),
                ("first_b", b"first content"),
                ("second_a", b"second content"),
                ("second_b", b"second content"),
                ("unique", b"unique content"),
            ],
        )?;

        assert_eq!(
            grouped_paths(&detect_duplicates(&files, |_| {})?),
            [
                paths_under(temp_dir.path(), &["first_a", "first_b"]),
                paths_under(temp_dir.path(), &["second_a", "second_b"]),
            ]
        );
        Ok(())
    }

    #[test]
    fn empty_files_are_duplicates_of_each_other() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let files = write_files(
            temp_dir.path(),
            &[("empty1", b""), ("empty2", b""), ("nonempty", b"content")],
        )?;

        assert_eq!(
            grouped_paths(&detect_duplicates(&files, |_| {})?),
            [paths_under(temp_dir.path(), &["empty1", "empty2"])]
        );
        Ok(())
    }

    #[test]
    fn binary_content_is_compared_byte_for_byte() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let binary_content: &[u8] = &[0x00, 0x01, 0x02, 0xFF, 0xFE];
        let files = write_files(
            temp_dir.path(),
            &[("binary1", binary_content), ("binary2", binary_content)],
        )?;

        assert_eq!(
            grouped_paths(&detect_duplicates(&files, |_| {})?),
            [paths_under(temp_dir.path(), &["binary1", "binary2"])]
        );
        Ok(())
    }

    #[test]
    fn files_larger_than_a_sampled_chunk_are_detected() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let large_content = vec![0x42u8; 4096];
        let files = write_files(
            temp_dir.path(),
            &[("large1", &large_content), ("large2", &large_content)],
        )?;

        assert_eq!(
            grouped_paths(&detect_duplicates(&files, |_| {})?),
            [paths_under(temp_dir.path(), &["large1", "large2"])]
        );
        Ok(())
    }

    #[test]
    fn copies_keep_the_input_order() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let files = write_files(
            temp_dir.path(),
            &[("second", b"test content"), ("first", b"test content")],
        )?;

        let duplicates = detect_duplicates(&files, |_| {})?;

        assert_eq!(
            duplicates,
            [DuplicateGroup::new(files).expect("two copies")]
        );
        Ok(())
    }

    #[test]
    fn reports_sampling_then_hashing_progress_up_to_the_totals()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let content: &[u8] = b"identical content";
        let content_size = content.len() as u64;
        let files = write_files(
            temp_dir.path(),
            &[
                ("first", content),
                ("other_size", b"no other file has this size"),
                ("second", content),
            ],
        )?;

        let mut reported_progress = Vec::new();
        detect_duplicates(&files, |progress| reported_progress.push(progress))?;

        assert_eq!(
            reported_progress,
            [
                ComparisonProgress::Sampling {
                    sampled_files: 0,
                    total_files: 2,
                },
                ComparisonProgress::Sampling {
                    sampled_files: 1,
                    total_files: 2,
                },
                ComparisonProgress::Sampling {
                    sampled_files: 2,
                    total_files: 2,
                },
                ComparisonProgress::Hashing {
                    hashed_files: 0,
                    total_files: 2,
                    hashed_bytes: 0,
                    total_bytes: 2 * content_size,
                },
                ComparisonProgress::Hashing {
                    hashed_files: 1,
                    total_files: 2,
                    hashed_bytes: content_size,
                    total_bytes: 2 * content_size,
                },
                ComparisonProgress::Hashing {
                    hashed_files: 2,
                    total_files: 2,
                    hashed_bytes: 2 * content_size,
                    total_bytes: 2 * content_size,
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn a_missing_candidate_is_a_file_system_error() {
        let files = [
            ScannerFileEntry {
                path: PathBuf::from("/tmp/does_not_exist_12345.txt"),
                file_size: 100,
                modified: SystemTime::UNIX_EPOCH,
            },
            ScannerFileEntry {
                path: PathBuf::from("/tmp/does_not_exist_67890.txt"),
                file_size: 100, // Same size to ensure processing
                modified: SystemTime::UNIX_EPOCH,
            },
        ];

        let result = detect_duplicates(&files, |_| {});
        assert!(matches!(result, Err(ReplicaError::FileSystemError(_))));
    }

    #[test]
    fn files_matching_only_on_sampled_chunks_are_not_duplicates()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let files = write_files(
            temp_dir.path(),
            &[
                ("original", &content_sharing_sampled_chunks(0x00)),
                ("lookalike", &content_sharing_sampled_chunks(0xFF)),
            ],
        )?;

        let duplicates = detect_duplicates(&files, |_| {})?;

        assert!(duplicates.is_empty(), "reported {duplicates:?}");
        Ok(())
    }

    #[test]
    fn content_splits_files_sharing_sampled_chunks() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let shared_content = content_sharing_sampled_chunks(0x00);
        let files = write_files(
            temp_dir.path(),
            &[
                ("original", &shared_content),
                ("lookalike", &content_sharing_sampled_chunks(0xFF)),
                ("copy", &shared_content),
            ],
        )?;

        assert_eq!(
            grouped_paths(&detect_duplicates(&files, |_| {})?),
            [paths_under(temp_dir.path(), &["copy", "original"])]
        );
        Ok(())
    }
}
