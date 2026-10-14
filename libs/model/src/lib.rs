use std::{fmt::Debug, hash::Hash, path::PathBuf};

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct ScannerFileEntry {
    pub path: PathBuf,
    pub file_size: u64,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> ScannerFileEntry {
        ScannerFileEntry {
            path: PathBuf::from(name),
            file_size: 1,
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
}
