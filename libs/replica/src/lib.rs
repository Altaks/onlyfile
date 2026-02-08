use std::collections::{HashMap, HashSet};

use itertools::Itertools;
use model::{HashedFileEntry, ScannerFileEntry};

use crate::{errors::ReplicaError, signature::quick_hash_file};

mod errors;
mod signature;

pub async fn detect_duplicates(
    files: &mut Vec<ScannerFileEntry>,
) -> Result<HashMap<HashedFileEntry, Vec<HashedFileEntry>>, ReplicaError> {
    // Presort files list by their sizes
    glidesort::sort_by_key(files, |it| it.file_size);

    // Remove all files that have are unique by size
    let unique_files =
        HashSet::<&ScannerFileEntry>::from_iter(files.iter().unique_by(|it| it.file_size));

    let non_unique_files = files
        .iter()
        .filter(|it| !unique_files.contains(it))
        .collect_vec();

    let mut non_unique_hashed_files = Vec::<HashedFileEntry>::with_capacity(non_unique_files.len());
    for ScannerFileEntry { path, file_size } in non_unique_files {
        non_unique_hashed_files.push(HashedFileEntry {
            path: path.to_owned(),
            file_size: *file_size,
            hash: quick_hash_file(&path, *file_size).await?,
        });
    }
    let unique_hashed_files = HashSet::<&HashedFileEntry>::from_iter(
        non_unique_hashed_files.iter().unique_by(|it| it.hash),
    );

    // Compute hashes for each group
    // Each group can have multiple unique hashes, and multiple duplicates contents, for example :

    // File A -> 0101 -> First apparition of signature "0101"
    // File B -> 1010 -> First apparition of signature "1010"
    // File C -> 0101 <- Duplicate of signature of file A
    // File D -> 1010 <- Duplicate of signature of file B

    // Then File C is a duplicate of A
    // File D is a duplicate of file B

    // They still have the same file size :/

    let non_unique_grouped_files_by_size = non_unique_hashed_files
        .iter()
        .filter(|it| !unique_hashed_files.contains(*it))
        .into_iter()
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
            let mut hash_group_iter = hash_group.1.into_iter();

            if let Some(first_file_entry) = hash_group_iter.next() {
                let mut other_hashed_files = Vec::<HashedFileEntry>::new();
                while let Some(entry) = hash_group_iter.next() {
                    other_hashed_files.push(entry.clone());
                }

                duplicates_registry.insert(first_file_entry.clone(), other_hashed_files);
            }
        })
    });

    Ok(duplicates_registry)
}
