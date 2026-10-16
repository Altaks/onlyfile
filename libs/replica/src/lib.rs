use std::{
    collections::{HashMap, HashSet},
    io,
    path::PathBuf,
};

use identity::FileIdentity;
use itertools::Itertools;
use model::{DuplicateGroup, ScannerFileEntry, SkipReason, SkippedEntry};

use crate::signature::{content_hash_file, quick_hash_file};

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

/// What the comparison found: the duplicate groups, and every file it had to leave out with the
/// reason.
#[derive(Debug, Default)]
pub struct ComparisonReport {
    pub groups: Vec<DuplicateGroup>,
    pub skipped: Vec<SkippedEntry>,
}

/// Finds every set of files whose whole content is identical. Groups come in no particular order;
/// the copies inside a group keep the input order.
///
/// Reading every byte of every file is the expensive part, so it only happens for files that
/// already share a size and a sampled hash with another file. Hardlinks are counted once, under
/// their first name. A file that cannot be read at any step is skipped and the others are still
/// compared. `on_progress` is called once when each phase starts and once after each file.
pub fn detect_duplicates(
    files: &[ScannerFileEntry],
    mut on_progress: impl FnMut(ComparisonProgress),
) -> ComparisonReport {
    let mut skipped = Vec::new();
    let mut skip = |path: &PathBuf, reason: SkipReason| {
        skipped.push(SkippedEntry {
            path: path.clone(),
            reason,
        });
    };
    let unreadable = |error: io::Error| SkipReason::Unreadable(error.kind());

    // Hardlinks share their size, so only files with a size twin can be one. The first name of a
    // file is kept, in input order.
    let mut seen_identities = HashSet::<FileIdentity>::new();
    let mut distinct_files = Vec::new();
    for file in with_a_size_twin(files.iter()) {
        match FileIdentity::of_path(&file.path) {
            Ok(identity) if seen_identities.insert(identity) => distinct_files.push(file),
            Ok(_) => skip(&file.path, SkipReason::Hardlink),
            Err(error) => skip(&file.path, unreadable(error)),
        }
    }
    // A file whose only size twin was another name of itself has nothing left to compare with.
    let size_candidates = with_a_size_twin(distinct_files.into_iter()).collect_vec();

    on_progress(ComparisonProgress::Sampling {
        sampled_files: 0,
        total_files: size_candidates.len(),
    });
    let mut candidates_by_sample = HashMap::<(u64, u64), Vec<&ScannerFileEntry>>::new();
    for (sampled_files, file) in (1..).zip(&size_candidates) {
        match quick_hash_file(&file.path, file.file_size) {
            Ok(sampled_hash) => candidates_by_sample
                .entry((file.file_size, sampled_hash))
                .or_default()
                .push(file),
            Err(error) => skip(&file.path, unreadable(error)),
        }
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

    let mut groups = Vec::new();
    for candidates in content_candidates {
        let mut copies_by_content = HashMap::<u128, Vec<ScannerFileEntry>>::new();
        for candidate in candidates {
            match content_hash_file(&candidate.path) {
                Ok(content_hash) => copies_by_content
                    .entry(content_hash)
                    .or_default()
                    .push(candidate.clone()),
                Err(error) => skip(&candidate.path, unreadable(error)),
            }
            hashed_files += 1;
            hashed_bytes += candidate.file_size;
            on_progress(ComparisonProgress::Hashing {
                hashed_files,
                total_files,
                hashed_bytes,
                total_bytes,
            });
        }

        groups.extend(
            copies_by_content
                .into_values()
                .filter_map(DuplicateGroup::new),
        );
    }

    ComparisonReport { groups, skipped }
}

/// The files sharing their size with at least one other file of `files`.
fn with_a_size_twin<'files>(
    files: impl Iterator<Item = &'files ScannerFileEntry> + Clone,
) -> impl Iterator<Item = &'files ScannerFileEntry> {
    let size_counts: HashMap<u64, usize> = files.clone().map(|file| file.file_size).counts();
    files.filter(move |file| size_counts[&file.file_size] > 1)
}

#[cfg(test)]
mod tests {
    use std::{path::Path, time::SystemTime};

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
        assert!(detect_duplicates(&[], |_| {}).groups.is_empty());
        Ok(())
    }

    #[test]
    fn a_single_file_has_no_duplicates() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let files = write_files(temp_dir.path(), &[("unique", b"unique")])?;

        assert!(detect_duplicates(&files, |_| {}).groups.is_empty());
        Ok(())
    }

    #[test]
    fn files_of_different_sizes_are_not_duplicates() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let files = write_files(
            temp_dir.path(),
            &[("small", b"small"), ("large", b"much larger content")],
        )?;

        assert!(detect_duplicates(&files, |_| {}).groups.is_empty());
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

        assert!(detect_duplicates(&files, |_| {}).groups.is_empty());
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
            grouped_paths(&detect_duplicates(&files, |_| {}).groups),
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
            grouped_paths(&detect_duplicates(&files, |_| {}).groups),
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
            grouped_paths(&detect_duplicates(&files, |_| {}).groups),
            [
                paths_under(temp_dir.path(), &["first_a", "first_b"]),
                paths_under(temp_dir.path(), &["second_a", "second_b"]),
            ]
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
            grouped_paths(&detect_duplicates(&files, |_| {}).groups),
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
            grouped_paths(&detect_duplicates(&files, |_| {}).groups),
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

        let duplicates = detect_duplicates(&files, |_| {}).groups;

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
        detect_duplicates(&files, |progress| reported_progress.push(progress));

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
    fn a_missing_candidate_is_skipped_as_unreadable() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let missing = |name: &str| ScannerFileEntry {
            path: temp_dir.path().join(name),
            file_size: 100,
            modified: SystemTime::UNIX_EPOCH,
        };
        let files = [missing("first"), missing("second")];

        let report = detect_duplicates(&files, |_| {});

        assert!(report.groups.is_empty());
        assert_eq!(
            report.skipped,
            [
                SkippedEntry {
                    path: temp_dir.path().join("first"),
                    reason: SkipReason::Unreadable(io::ErrorKind::NotFound),
                },
                SkippedEntry {
                    path: temp_dir.path().join("second"),
                    reason: SkipReason::Unreadable(io::ErrorKind::NotFound),
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn a_file_vanishing_before_it_is_sampled_is_skipped_and_the_rest_compared()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let content: &[u8] = b"three identical copies";
        let files = write_files(
            temp_dir.path(),
            &[
                ("first", content),
                ("second", content),
                ("vanishing", content),
            ],
        )?;
        let vanishing = temp_dir.path().join("vanishing");

        let report = detect_duplicates(&files, |progress| {
            if let ComparisonProgress::Sampling {
                sampled_files: 0, ..
            } = progress
            {
                std::fs::remove_file(&vanishing).expect("the file exists until now");
            }
        });

        assert_eq!(
            grouped_paths(&report.groups),
            [paths_under(temp_dir.path(), &["first", "second"])]
        );
        assert_eq!(
            report.skipped,
            [SkippedEntry {
                path: vanishing,
                reason: SkipReason::Unreadable(io::ErrorKind::NotFound),
            }]
        );
        Ok(())
    }

    #[test]
    fn a_file_vanishing_before_its_content_is_read_is_skipped_and_the_rest_compared()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let content: &[u8] = b"three identical copies";
        let files = write_files(
            temp_dir.path(),
            &[
                ("first", content),
                ("second", content),
                ("vanishing", content),
            ],
        )?;
        let vanishing = temp_dir.path().join("vanishing");

        let report = detect_duplicates(&files, |progress| {
            if let ComparisonProgress::Hashing {
                hashed_files: 0, ..
            } = progress
            {
                std::fs::remove_file(&vanishing).expect("the file exists until now");
            }
        });

        assert_eq!(
            grouped_paths(&report.groups),
            [paths_under(temp_dir.path(), &["first", "second"])]
        );
        assert_eq!(
            report.skipped,
            [SkippedEntry {
                path: vanishing,
                reason: SkipReason::Unreadable(io::ErrorKind::NotFound),
            }]
        );
        Ok(())
    }

    #[test]
    fn a_hardlink_counts_once_and_is_listed_as_skipped() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let content: &[u8] = b"one file, two names, and a real copy";
        let original = temp_dir.path().join("original");
        let hardlink = temp_dir.path().join("hardlink");
        std::fs::write(&original, content)?;
        std::fs::hard_link(&original, &hardlink)?;
        let mut files = write_files(temp_dir.path(), &[("copy", content)])?;
        for path in [&original, &hardlink] {
            files.insert(
                files.len() - 1,
                ScannerFileEntry {
                    path: path.clone(),
                    file_size: content.len() as u64,
                    modified: std::fs::metadata(path)?.modified()?,
                },
            );
        }

        let report = detect_duplicates(&files, |_| {});

        assert_eq!(
            grouped_paths(&report.groups),
            [paths_under(temp_dir.path(), &["copy", "original"])]
        );
        assert_eq!(
            report.skipped,
            [SkippedEntry {
                path: hardlink,
                reason: SkipReason::Hardlink,
            }]
        );
        Ok(())
    }

    #[test]
    fn two_names_for_one_file_are_not_duplicates() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let original = temp_dir.path().join("original");
        let hardlink = temp_dir.path().join("hardlink");
        std::fs::write(&original, b"only one file on disk")?;
        std::fs::hard_link(&original, &hardlink)?;
        let files = [&original, &hardlink].map(|path| ScannerFileEntry {
            path: path.clone(),
            file_size: 21,
            modified: SystemTime::UNIX_EPOCH,
        });

        let mut reported_progress = Vec::new();
        let report = detect_duplicates(&files, |progress| reported_progress.push(progress));

        assert!(report.groups.is_empty());
        assert_eq!(
            report.skipped,
            [SkippedEntry {
                path: hardlink,
                reason: SkipReason::Hardlink,
            }]
        );
        // Alone once the names collapse, the file is not even sampled.
        assert_eq!(
            reported_progress[0],
            ComparisonProgress::Sampling {
                sampled_files: 0,
                total_files: 0,
            }
        );
        Ok(())
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

        let duplicates = detect_duplicates(&files, |_| {}).groups;

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
            grouped_paths(&detect_duplicates(&files, |_| {}).groups),
            [paths_under(temp_dir.path(), &["copy", "original"])]
        );
        Ok(())
    }
}
