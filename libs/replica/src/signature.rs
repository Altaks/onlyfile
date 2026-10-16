use std::{
    fs::File,
    io::{self, ErrorKind, Read, Seek, SeekFrom},
    path::Path,
};

use xxhash_rust::xxh3::{Xxh3, xxh3_64};

const FILE_CHUNK_SIZE: usize = 1024;

/// Bytes read per call while streaming a whole file through the hasher.
const CONTENT_HASH_BUFFER_SIZE: usize = 64 * 1024;

pub(crate) fn quick_hash_file(path: impl AsRef<Path>, file_size: u64) -> io::Result<u64> {
    // If the file is smaller than the size of a scannable chunk, then hash the whole file instead.
    if file_size <= FILE_CHUNK_SIZE as u64 {
        return hash_file(path);
    }

    let mut file_reader = File::open(&path)?;
    let mut hash = Xxh3::new();

    //                                                        WHOLE FILE BYTES
    //                               |--------------------------------------------------------------|
    //                               |                               || <- Center                   |    file_size / 2
    //                               |                      ||XXXXXXXXXXXXXXXX|| <- Chunk to hash   |
    //                               |                      ^^                                      |    - (file_chunk_size / 2)
    //                               |             We want to start here                            |    = Seeked byte
    let sampled_chunk_starts = [
        SeekFrom::Start(0),
        SeekFrom::Start(file_size / 2 - FILE_CHUNK_SIZE as u64 / 2),
        SeekFrom::End(-(FILE_CHUNK_SIZE as i64)),
    ];

    let mut chunk = [0u8; FILE_CHUNK_SIZE];
    for chunk_start in sampled_chunk_starts {
        file_reader.seek(chunk_start)?;
        file_reader.read_exact(&mut chunk)?;
        hash.update(&chunk);
    }

    // Return the hash result of the three blocks : start, middle and ending bytes chunks
    Ok(hash.digest())
}

pub(crate) fn content_hash_file(path: impl AsRef<Path>) -> io::Result<u128> {
    let mut file_reader = File::open(path)?;
    let mut hash = Xxh3::new();
    let mut buffer = vec![0u8; CONTENT_HASH_BUFFER_SIZE];

    loop {
        match file_reader.read(&mut buffer) {
            Ok(0) => return Ok(hash.digest128()),
            Ok(read_bytes_count) => hash.update(&buffer[..read_bytes_count]),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn hash_file(path: impl AsRef<Path>) -> io::Result<u64> {
    let file_content = std::fs::read(path)?;
    Ok(xxh3_64(&file_content))
}

#[cfg(test)]
mod tests {
    use tempfile::{NamedTempFile, TempDir};
    use xxhash_rust::xxh3::xxh3_128;

    use super::*;

    /// A distinct byte at every offset, so a read at the wrong place or of the wrong
    /// length changes the hash.
    fn position_dependent_content(length: usize) -> Vec<u8> {
        (0..length).map(|offset| (offset % 251) as u8).collect()
    }

    #[test]
    fn quick_hash_covers_exactly_the_first_middle_and_last_chunks()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let content = position_dependent_content(5 * FILE_CHUNK_SIZE + 300);
        std::fs::write(temp_file.path(), &content)?;

        let middle_start = content.len() / 2 - FILE_CHUNK_SIZE / 2;
        let sampled_bytes = [
            &content[..FILE_CHUNK_SIZE],
            &content[middle_start..middle_start + FILE_CHUNK_SIZE],
            &content[content.len() - FILE_CHUNK_SIZE..],
        ]
        .concat();

        let quick_hash = quick_hash_file(temp_file.path(), content.len() as u64)?;
        assert_eq!(quick_hash, xxh3_64(&sampled_bytes));
        Ok(())
    }

    #[test]
    fn content_hash_streams_the_whole_file_across_buffer_boundaries()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let content = position_dependent_content(2 * CONTENT_HASH_BUFFER_SIZE + 123);
        std::fs::write(temp_file.path(), &content)?;

        assert_eq!(content_hash_file(temp_file.path())?, xxh3_128(&content));
        Ok(())
    }

    #[test]
    fn content_hash_of_an_empty_file_is_the_hash_of_no_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;

        assert_eq!(content_hash_file(temp_file.path())?, xxh3_128(&[]));
        Ok(())
    }

    /// Test hashing a small file (smaller than FILE_CHUNK_SIZE)
    #[test]
    fn test_quick_hash_small_file() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let file_path = temp_file.path();

        // Write content smaller than FILE_CHUNK_SIZE
        let content = b"Hello, World!";
        std::fs::write(file_path, content)?;

        let file_size = content.len() as u64;
        let quick_hash = quick_hash_file(file_path, file_size)?;
        let full_hash = hash_file(file_path)?;

        // For small files, quick hash should equal full hash
        assert_eq!(quick_hash, full_hash);

        Ok(())
    }

    /// Test hashing a file exactly equal to FILE_CHUNK_SIZE
    #[test]
    fn test_quick_hash_exact_chunk_size_file() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let file_path = temp_file.path();

        // Write content exactly FILE_CHUNK_SIZE bytes
        let content = vec![0x42u8; FILE_CHUNK_SIZE];
        std::fs::write(file_path, &content)?;

        let file_size = content.len() as u64;
        let quick_hash = quick_hash_file(file_path, file_size)?;
        let full_hash = hash_file(file_path)?;

        // For files exactly FILE_CHUNK_SIZE, quick hash should equal full hash
        assert_eq!(quick_hash, full_hash);

        Ok(())
    }

    /// Test hashing a large file (larger than FILE_CHUNK_SIZE)
    #[test]
    fn test_quick_hash_large_file() -> Result<(), Box<dyn std::error::Error>> {
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

        std::fs::write(file_path, &content)?;

        let file_size = content.len() as u64;
        let quick_hash = quick_hash_file(file_path, file_size)?;

        // The hash should be different from the full file hash
        let full_hash = hash_file(file_path)?;
        assert_ne!(quick_hash, full_hash);

        // But should be deterministic
        let quick_hash2 = quick_hash_file(file_path, file_size)?;
        assert_eq!(quick_hash, quick_hash2);

        Ok(())
    }

    /// Test that two identical files produce the same hash
    #[test]
    fn test_quick_hash_identical_files() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let file1_path = temp_dir.path().join("file1.txt");
        let file2_path = temp_dir.path().join("file2.txt");

        let content = b"This is test content for hashing.";

        // Create two identical files
        for path in [&file1_path, &file2_path] {
            std::fs::write(path, content)?;
        }

        let file_size = content.len() as u64;
        let hash1 = quick_hash_file(&file1_path, file_size)?;
        let hash2 = quick_hash_file(&file2_path, file_size)?;

        assert_eq!(hash1, hash2);

        Ok(())
    }

    /// Test that two different files produce different hashes
    #[test]
    fn test_quick_hash_different_files() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let file1_path = temp_dir.path().join("file1.txt");
        let file2_path = temp_dir.path().join("file2.txt");

        let content1 = b"This is test content for hashing.";
        let content2 = b"This is different test content for hashing.";

        // Create two different files
        std::fs::write(&file1_path, content1)?;

        std::fs::write(&file2_path, content2)?;

        let hash1 = quick_hash_file(&file1_path, content1.len() as u64)?;
        let hash2 = quick_hash_file(&file2_path, content2.len() as u64)?;

        assert_ne!(hash1, hash2);

        Ok(())
    }

    /// Test hashing an empty file
    #[test]
    fn test_quick_hash_empty_file() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let file_path = temp_file.path();

        // Create an empty file
        std::fs::write(file_path, b"")?;

        let file_size = 0u64;
        let quick_hash = quick_hash_file(file_path, file_size)?;
        let full_hash = hash_file(file_path)?;

        // For empty files, both should be equal
        assert_eq!(quick_hash, full_hash);

        Ok(())
    }

    /// Test that files with same start/middle/end but different middle content produce same quick hash
    #[test]
    fn test_quick_hash_collision_scenario() -> Result<(), Box<dyn std::error::Error>> {
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
        std::fs::write(&file1_path, &content1)?;

        std::fs::write(&file2_path, &content2)?;

        let hash1 = quick_hash_file(&file1_path, content1.len() as u64)?;
        let hash2 = quick_hash_file(&file2_path, content2.len() as u64)?;

        // These should have the same quick hash since their start/middle/end chunks are identical
        assert_eq!(hash1, hash2);

        // But different full hashes
        let full_hash1 = hash_file(&file1_path)?;
        let full_hash2 = hash_file(&file2_path)?;
        assert_ne!(full_hash1, full_hash2);

        Ok(())
    }

    /// Test error handling for non-existent file
    #[test]
    fn test_quick_hash_non_existent_file() {
        let non_existent_path = "/tmp/this_file_does_not_exist_12345.txt";
        let file_size = 100u64;

        let result = quick_hash_file(non_existent_path, file_size);

        assert!(
            matches!(&result, Err(error) if error.kind() == ErrorKind::NotFound),
            "{result:?}"
        );
    }

    /// Test error handling when file size doesn't match actual file size
    #[test]
    fn test_quick_hash_incorrect_file_size() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let file_path = temp_file.path();

        let content = b"Hello, World!";
        std::fs::write(file_path, content)?;

        // Pass an incorrect file size (larger than actual)
        let incorrect_size = 1000u64;
        let result = quick_hash_file(file_path, incorrect_size);

        // This should still work but may produce unexpected results
        // The function doesn't validate the file_size parameter
        assert!(result.is_ok());

        Ok(())
    }

    /// Test hashing a file with binary content
    #[test]
    fn test_quick_hash_binary_content() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let file_path = temp_file.path();

        // Create binary content with null bytes and other special values
        let content = vec![
            0x00, 0x01, 0x02, 0x03, 0xFF, 0xFE, 0xFD, 0xFC, 0x80, 0x81, 0x82, 0x83, 0x40, 0x41,
            0x42, 0x43,
        ];

        std::fs::write(file_path, &content)?;

        let file_size = content.len() as u64;
        let quick_hash = quick_hash_file(file_path, file_size)?;
        let full_hash = hash_file(file_path)?;

        // For small binary files, should be equal
        assert_eq!(quick_hash, full_hash);

        Ok(())
    }

    /// Test edge case: file size is exactly 2 * FILE_CHUNK_SIZE
    #[test]
    fn test_quick_hash_double_chunk_size() -> Result<(), Box<dyn std::error::Error>> {
        let temp_file = NamedTempFile::new()?;
        let file_path = temp_file.path();

        // Create file exactly 4 * FILE_CHUNK_SIZE to ensure all chunk reads succeed
        let content = vec![0x55u8; 4 * FILE_CHUNK_SIZE];
        std::fs::write(file_path, &content)?;

        let file_size = content.len() as u64;
        let quick_hash = quick_hash_file(file_path, file_size)?;

        // Should be deterministic
        let quick_hash2 = quick_hash_file(file_path, file_size)?;
        assert_eq!(quick_hash, quick_hash2);

        // Should be different from full hash
        let full_hash = hash_file(file_path)?;
        assert_ne!(quick_hash, full_hash);

        Ok(())
    }
}
