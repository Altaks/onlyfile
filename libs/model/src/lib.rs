use std::{fmt::Debug, hash::Hash, io, path::PathBuf, time::SystemTime};

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct ScannerFileEntry {
    pub path: PathBuf,
    pub file_size: u64,
    /// Last modification instant, as the filesystem reports it.
    pub modified: SystemTime,
}

/// Why an entry was left out of the comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Never followed, so a link can neither loop nor lead outside the scanned folder.
    Symlink,
    /// A socket, a pipe or a device: there is no content to compare.
    SpecialFile,
    /// Nothing to reclaim, and often a placeholder that something depends on.
    EmptyFile,
    Unreadable(io::ErrorKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedEntry {
    pub path: PathBuf,
    pub reason: SkipReason,
}

/// One file below this is not a duplicate of anything.
const MINIMUM_COPIES: usize = 2;

/// Files whose whole content is identical. Always holds at least two copies.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct DuplicateGroup {
    copies: Vec<ScannerFileEntry>,
}

impl DuplicateGroup {
    /// `None` when fewer than two copies are given.
    pub fn new(copies: Vec<ScannerFileEntry>) -> Option<Self> {
        (copies.len() >= MINIMUM_COPIES).then_some(Self { copies })
    }

    pub fn copies(&self) -> &[ScannerFileEntry] {
        &self.copies
    }

    /// The size of one copy. Every copy has the same size since their content is identical.
    pub fn file_size(&self) -> u64 {
        self.copies[0].file_size
    }

    /// The space freed by keeping one copy and removing all the others.
    pub fn reclaimable_bytes(&self) -> u64 {
        self.file_size() * (self.copies.len() as u64 - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> ScannerFileEntry {
        sized_entry(name, 1)
    }

    fn sized_entry(name: &str, file_size: u64) -> ScannerFileEntry {
        ScannerFileEntry {
            path: PathBuf::from(name),
            file_size,
            modified: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn a_single_file_is_not_a_duplicate_group() {
        assert_eq!(DuplicateGroup::new(vec![entry("alone")]), None);
    }

    #[test]
    fn two_copies_form_a_group_in_the_given_order() {
        let group = DuplicateGroup::new(vec![entry("first"), entry("second")])
            .expect("two copies are a group");

        assert_eq!(group.copies(), [entry("first"), entry("second")]);
    }

    #[test]
    fn removing_all_copies_but_one_reclaims_their_size() {
        let group = DuplicateGroup::new(vec![
            sized_entry("first", 412),
            sized_entry("second", 412),
            sized_entry("third", 412),
        ])
        .expect("three copies are a group");

        assert_eq!(group.file_size(), 412);
        assert_eq!(group.reclaimable_bytes(), 2 * 412);
    }
}
