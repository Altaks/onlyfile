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

    // Prepare a chunk to collect read bytes, and create an async file reader
    let mut file_chunk = vec![0u8; FILE_CHUNK_SIZE];
    let mut file_reader = tokio::fs::File::open(&path).await?;

    // Prepare a hasher instance
    let mut hash = Xxh3::new();

    // Read the starting chunk of the file into the `file_chunk` buffer, assert it's the right size in debug mode, and update the hasher with the new bytes.
    let first_chunk_read_bytes_count = file_reader.read_buf(&mut file_chunk).await?;
    debug_assert_eq!(first_chunk_read_bytes_count, FILE_CHUNK_SIZE);
    hash.update(&file_chunk);

    //                                                        WHOLE FILE BYTES
    //                               |--------------------------------------------------------------|
    //                               |                               || <- Center                   |    file_size / 2
    //                               |                      ||XXXXXXXXXXXXXXXX|| <- Chunk to hash   |
    //                               |                      ^^                                      |    - (file_chunk_size / 2)
    //                               |             We want to start here                            |    = Seeked byte

    // Move the `file_reader` to the right position, look at the explaination right before this comment, read the file chunk, and add it to the hasher
    let middle_chunk_position =
        file_size.saturating_div(2) - (FILE_CHUNK_SIZE as u64).saturating_div(2);
    file_reader
        .seek(std::io::SeekFrom::Start(middle_chunk_position))
        .await?; // Seek middle
    let middle_chunk_read_bytes_count = file_reader.read_buf(&mut file_chunk).await?;
    debug_assert_eq!(middle_chunk_read_bytes_count, FILE_CHUNK_SIZE);
    hash.update(&file_chunk);

    // Move the reader to the end of the file, read the last file chunk and add it to the hasher
    file_reader
        .seek(std::io::SeekFrom::End(-(FILE_CHUNK_SIZE as i64)))
        .await?;
    let end_chunk_read_bytes_count = file_reader.read_buf(&mut file_chunk).await?;
    debug_assert_eq!(end_chunk_read_bytes_count, FILE_CHUNK_SIZE);
    hash.update(&file_chunk);

    // Return the hash result of the three blocks : start, middle and ending bytes chunks
    Ok(hash.digest())
}

pub(crate) async fn hash_file(path: impl AsRef<Path>) -> Result<u64, ReplicaError> {
    let file_content = tokio::fs::read(path).await?;
    Ok(xxh3_64(&file_content))
}
