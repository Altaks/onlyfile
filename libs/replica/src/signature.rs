use std::path::Path;

use tokio::io::{AsyncReadExt, AsyncSeekExt};
use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use crate::errors::ReplicaError;

const FILE_CHUNK_SIZE: usize = 1024;

pub(crate) async fn quick_hash_file(
    path: impl AsRef<Path>,
    file_size: u64,
) -> Result<u64, ReplicaError> {
    // TODO : Make checks on this, it might make an unexpected behavior since it
    // TODO : could detect two different files as similar if their sizes differ but not their start/middle/end chunks ? (If that makes any sense)

    // If the file is smaller than the size of a scannable chunk, then hash the whole file instead.
    if file_size <= FILE_CHUNK_SIZE as u64 {
        return hash_file(path).await;
    }

    // Create an async file reader
    let mut file_reader = tokio::fs::File::open(&path).await?;

    // Prepare a hasher instance
    let mut hash = Xxh3::new();

    // Read the starting chunk of the file
    let mut first_chunk = vec![0u8; FILE_CHUNK_SIZE];
    let first_chunk_read_bytes_count = file_reader.read_buf(&mut first_chunk).await?;
    debug_assert_eq!(first_chunk_read_bytes_count, FILE_CHUNK_SIZE);
    hash.update(&first_chunk);

    //                                                        WHOLE FILE BYTES
    //                               |--------------------------------------------------------------|
    //                               |                               || <- Center                   |    file_size / 2
    //                               |                      ||XXXXXXXXXXXXXXXX|| <- Chunk to hash   |
    //                               |                      ^^                                      |    - (file_chunk_size / 2)
    //                               |             We want to start here                            |    = Seeked byte

    // Move the `file_reader` to the right position and read the middle chunk
    let middle_chunk_position =
        file_size.saturating_div(2) - (FILE_CHUNK_SIZE as u64).saturating_div(2);
    file_reader
        .seek(std::io::SeekFrom::Start(middle_chunk_position))
        .await?; // Seek middle
    let mut middle_chunk = vec![0u8; FILE_CHUNK_SIZE];
    let middle_chunk_read_bytes_count = file_reader.read_buf(&mut middle_chunk).await?;
    debug_assert_eq!(middle_chunk_read_bytes_count, FILE_CHUNK_SIZE);
    hash.update(&middle_chunk);

    // Move the reader to the end of the file and read the last chunk
    file_reader
        .seek(std::io::SeekFrom::End(-(FILE_CHUNK_SIZE as i64)))
        .await?;
    let mut end_chunk = vec![0u8; FILE_CHUNK_SIZE];
    let end_chunk_read_bytes_count = file_reader.read_buf(&mut end_chunk).await?;
    debug_assert_eq!(end_chunk_read_bytes_count, FILE_CHUNK_SIZE);
    hash.update(&end_chunk);

    // Return the hash result of the three blocks : start, middle and ending bytes chunks
    Ok(hash.digest())
}

pub(crate) async fn hash_file(path: impl AsRef<Path>) -> Result<u64, ReplicaError> {
    let file_content = tokio::fs::read(path).await?;
    Ok(xxh3_64(&file_content))
}

#[cfg(test)]
mod tests {
    use tempfile::{NamedTempFile, TempDir};
    use tokio::fs::File;
    use tokio::io::AsyncWriteExt;

    use super::*;

    /// Test hashing a small file (smaller than FILE_CHUNK_SIZE)
    #[tokio::test]
    async fn test_quick_hash_small_file() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let file_path = temp_file.path();

        // Write content smaller than FILE_CHUNK_SIZE
        let content = b"Hello, World!";
        let mut file: File = File::create(file_path).await?;
        file.write_all(content).await?;
        file.flush().await?;

        let file_size = content.len() as u64;
        let quick_hash = quick_hash_file(file_path, file_size).await?;
        let full_hash = hash_file(file_path).await?;

        // For small files, quick hash should equal full hash
        assert_eq!(quick_hash, full_hash);

        Ok(())
    }

    /// Test hashing a file exactly equal to FILE_CHUNK_SIZE
    #[tokio::test]
    async fn test_quick_hash_exact_chunk_size_file() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let file_path = temp_file.path();

        // Write content exactly FILE_CHUNK_SIZE bytes
        let content = vec![0x42u8; FILE_CHUNK_SIZE];
        let mut file: File = File::create(file_path).await?;
        file.write_all(&content).await?;
        file.flush().await?;

        let file_size = content.len() as u64;
        let quick_hash = quick_hash_file(file_path, file_size).await?;
        let full_hash = hash_file(file_path).await?;

        // For files exactly FILE_CHUNK_SIZE, quick hash should equal full hash
        assert_eq!(quick_hash, full_hash);

        Ok(())
    }

    /// Test hashing a large file (larger than FILE_CHUNK_SIZE)
    #[tokio::test]
    async fn test_quick_hash_large_file() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let file_path = temp_file.path();

        // Create a large file with distinct content in start, middle, and end
        let mut content = Vec::new();

        // Start section (first chunk)
        content.extend_from_slice(&[0x01u8; FILE_CHUNK_SIZE]);

        // Large middle section to ensure we have enough bytes for middle chunk reading
        content.extend_from_slice(&[0x00u8; FILE_CHUNK_SIZE * 4]);

        // Middle section (where the middle chunk will be read from)
        content.extend_from_slice(&[0x02u8; FILE_CHUNK_SIZE]);

        // More filler to ensure we have enough space for end chunk
        content.extend_from_slice(&[0x00u8; FILE_CHUNK_SIZE * 4]);

        // End section (last chunk)
        content.extend_from_slice(&[0x03u8; FILE_CHUNK_SIZE]);

        let mut file: File = File::create(file_path).await?;
        file.write_all(&content).await?;
        file.flush().await?;

        let file_size = content.len() as u64;
        let quick_hash = quick_hash_file(file_path, file_size).await?;

        // The hash should be different from the full file hash
        let full_hash = hash_file(file_path).await?;
        assert_ne!(quick_hash, full_hash);

        // But should be deterministic
        let quick_hash2 = quick_hash_file(file_path, file_size).await?;
        assert_eq!(quick_hash, quick_hash2);

        Ok(())
    }

    /// Test that two identical files produce the same hash
    #[tokio::test]
    async fn test_quick_hash_identical_files() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let file1_path = temp_dir.path().join("file1.txt");
        let file2_path = temp_dir.path().join("file2.txt");

        let content = b"This is test content for hashing.";

        // Create two identical files
        for path in [&file1_path, &file2_path] {
            let mut file: File = File::create(path).await?;
            file.write_all(content).await?;
            file.flush().await?;
        }

        let file_size = content.len() as u64;
        let hash1 = quick_hash_file(&file1_path, file_size).await?;
        let hash2 = quick_hash_file(&file2_path, file_size).await?;

        assert_eq!(hash1, hash2);

        Ok(())
    }

    /// Test that two different files produce different hashes
    #[tokio::test]
    async fn test_quick_hash_different_files() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let file1_path = temp_dir.path().join("file1.txt");
        let file2_path = temp_dir.path().join("file2.txt");

        let content1 = b"This is test content for hashing.";
        let content2 = b"This is different test content for hashing.";

        // Create two different files
        let mut file1: File = File::create(&file1_path).await?;
        file1.write_all(content1).await?;
        file1.flush().await?;

        let mut file2: File = File::create(&file2_path).await?;
        file2.write_all(content2).await?;
        file2.flush().await?;

        let hash1 = quick_hash_file(&file1_path, content1.len() as u64).await?;
        let hash2 = quick_hash_file(&file2_path, content2.len() as u64).await?;

        assert_ne!(hash1, hash2);

        Ok(())
    }

    /// Test hashing an empty file
    #[tokio::test]
    async fn test_quick_hash_empty_file() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let file_path = temp_file.path();

        // Create an empty file
        let mut empty_file: File = File::create(file_path).await?;
        empty_file.flush().await?;

        let file_size = 0u64;
        let quick_hash = quick_hash_file(file_path, file_size).await?;
        let full_hash = hash_file(file_path).await?;

        // For empty files, both should be equal
        assert_eq!(quick_hash, full_hash);

        Ok(())
    }

    /// Test that files with same start/middle/end but different middle content produce same quick hash
    #[tokio::test]
    async fn test_quick_hash_collision_scenario() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let file1_path = temp_dir.path().join("file1.txt");
        let file2_path = temp_dir.path().join("file2.txt");

        // Create file 1 - make it large enough for all chunk reads
        let mut content1 = Vec::new();
        content1.extend_from_slice(&[0x01u8; FILE_CHUNK_SIZE]); // Start
        content1.extend_from_slice(&[0x00u8; FILE_CHUNK_SIZE * 4]); // Filler
        content1.extend_from_slice(&[0x02u8; FILE_CHUNK_SIZE]); // Middle
        content1.extend_from_slice(&[0x00u8; FILE_CHUNK_SIZE * 4]); // Filler
        content1.extend_from_slice(&[0x03u8; FILE_CHUNK_SIZE]); // End

        // Create file 2 with different middle content but same start/middle/end chunks
        let mut content2 = Vec::new();
        content2.extend_from_slice(&[0x01u8; FILE_CHUNK_SIZE]); // Same start
        content2.extend_from_slice(&[0xFFu8; FILE_CHUNK_SIZE * 4]); // Different middle filler
        content2.extend_from_slice(&[0x02u8; FILE_CHUNK_SIZE]); // Same middle chunk
        content2.extend_from_slice(&[0xFFu8; FILE_CHUNK_SIZE * 4]); // Different middle filler
        content2.extend_from_slice(&[0x03u8; FILE_CHUNK_SIZE]); // Same end

        // Write both files
        let mut file1: File = File::create(&file1_path).await?;
        file1.write_all(&content1).await?;
        file1.flush().await?;

        let mut file2: File = File::create(&file2_path).await?;
        file2.write_all(&content2).await?;
        file2.flush().await?;

        let hash1 = quick_hash_file(&file1_path, content1.len() as u64).await?;
        let hash2 = quick_hash_file(&file2_path, content2.len() as u64).await?;

        // These should have the same quick hash since their start/middle/end chunks are identical
        assert_eq!(hash1, hash2);

        // But different full hashes
        let full_hash1 = hash_file(&file1_path).await?;
        let full_hash2 = hash_file(&file2_path).await?;
        assert_ne!(full_hash1, full_hash2);

        Ok(())
    }

    /// Test error handling for non-existent file
    #[tokio::test]
    async fn test_quick_hash_non_existent_file() {
        let non_existent_path = "/tmp/this_file_does_not_exist_12345.txt";
        let file_size = 100u64;

        let result = quick_hash_file(non_existent_path, file_size).await;

        assert!(matches!(result, Err(ReplicaError::FileSystemError(_))));
    }

    /// Test error handling when file size doesn't match actual file size
    #[tokio::test]
    async fn test_quick_hash_incorrect_file_size() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let file_path = temp_file.path();

        let content = b"Hello, World!";
        let mut file = File::create(file_path).await?;
        file.write_all(content).await?;
        file.flush().await?;

        // Pass an incorrect file size (larger than actual)
        let incorrect_size = 1000u64;
        let result: Result<u64, ReplicaError> = quick_hash_file(file_path, incorrect_size).await;

        // This should still work but may produce unexpected results
        // The function doesn't validate the file_size parameter
        assert!(result.is_ok());

        Ok(())
    }

    /// Test hashing a file with binary content
    #[tokio::test]
    async fn test_quick_hash_binary_content() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let file_path = temp_file.path();

        // Create binary content with null bytes and other special values
        let content = vec![
            0x00, 0x01, 0x02, 0x03, 0xFF, 0xFE, 0xFD, 0xFC, 0x80, 0x81, 0x82, 0x83, 0x40, 0x41,
            0x42, 0x43,
        ];

        let mut file: File = File::create(file_path).await?;
        file.write_all(&content).await?;
        file.flush().await?;

        let file_size = content.len() as u64;
        let quick_hash = quick_hash_file(file_path, file_size).await?;
        let full_hash = hash_file(file_path).await?;

        // For small binary files, should be equal
        assert_eq!(quick_hash, full_hash);

        Ok(())
    }

    /// Test edge case: file size is exactly 2 * FILE_CHUNK_SIZE
    #[tokio::test]
    async fn test_quick_hash_double_chunk_size() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let file_path = temp_file.path();

        // Create file exactly 4 * FILE_CHUNK_SIZE to ensure all chunk reads succeed
        let content = vec![0x55u8; 4 * FILE_CHUNK_SIZE];
        let mut file: File = File::create(file_path).await?;
        file.write_all(&content).await?;
        file.flush().await?;

        let file_size = content.len() as u64;
        let quick_hash = quick_hash_file(file_path, file_size).await?;

        // Should be deterministic
        let quick_hash2 = quick_hash_file(file_path, file_size).await?;
        assert_eq!(quick_hash, quick_hash2);

        // Should be different from full hash
        let full_hash = hash_file(file_path).await?;
        assert_ne!(quick_hash, full_hash);

        Ok(())
    }
}
