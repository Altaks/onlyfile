//! Removes duplicate copies, each only after proving it still matches, byte for byte, a copy that
//! stays.

use std::{
    fs::File,
    io::{self, ErrorKind, Read},
    path::{Path, PathBuf},
};

use trash::TrashContext;
#[cfg(target_os = "macos")]
use trash::macos::{DeleteMethod, TrashContextExtMacos};

/// Bytes read from each file per step of the comparison.
const COMPARISON_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposal {
    Trash,
    Permanent,
}

/// One group's removal: the copy that stays, which every removed copy is compared with, the copies
/// to remove, and the size every copy had at the scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupRemoval {
    kept: PathBuf,
    removed: Vec<PathBuf>,
    file_size: u64,
}

impl GroupRemoval {
    /// `None` when there is nothing to remove, or when `kept` is among the copies to remove: a
    /// removal always leaves its reference copy in place.
    pub fn new(kept: PathBuf, removed: Vec<PathBuf>, file_size: u64) -> Option<Self> {
        (!removed.is_empty() && !removed.contains(&kept)).then_some(Self {
            kept,
            removed,
            file_size,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemovalOutcome {
    /// Moved to the trash, or deleted with [`Disposal::Permanent`].
    Removed,
    /// Left in place: it no longer matches the kept copy.
    ChangedSinceScan,
    /// Left in place: the copy could not be read for the comparison.
    CopyUnreadable(ErrorKind),
    /// Left in place: the kept copy could not be read, so nothing proves this one is a duplicate.
    KeptCopyUnreadable(ErrorKind),
    /// The comparison passed but the trash or the deletion refused, with the system's reason.
    RemovalFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyRemoval {
    pub path: PathBuf,
    /// The size at the scan, which a successful removal frees.
    pub file_size: u64,
    pub outcome: RemovalOutcome,
}

/// Handles every copy of every removal in order. `on_copy_handled` receives each result as soon as
/// it is known, so a caller can show progress and log as it goes.
pub fn remove_copies(
    removals: &[GroupRemoval],
    disposal: Disposal,
    mut on_copy_handled: impl FnMut(&CopyRemoval),
) -> Vec<CopyRemoval> {
    let trash = trash_context();
    let mut results = Vec::new();
    for removal in removals {
        for copy in &removal.removed {
            let outcome = match same_content(copy, &removal.kept) {
                Ok(true) => dispose(copy, disposal, &trash),
                Ok(false) => RemovalOutcome::ChangedSinceScan,
                Err(Unreadable::Copy(kind)) => RemovalOutcome::CopyUnreadable(kind),
                Err(Unreadable::Kept(kind)) => RemovalOutcome::KeptCopyUnreadable(kind),
            };
            let result = CopyRemoval {
                path: copy.clone(),
                file_size: removal.file_size,
                outcome,
            };
            on_copy_handled(&result);
            results.push(result);
        }
    }
    results
}

/// Which side of a comparison could not be read.
enum Unreadable {
    Copy(ErrorKind),
    Kept(ErrorKind),
}

fn same_content(copy: &Path, kept: &Path) -> Result<bool, Unreadable> {
    let copy_error = |error: io::Error| Unreadable::Copy(error.kind());
    let kept_error = |error: io::Error| Unreadable::Kept(error.kind());
    let mut copy_file = File::open(copy).map_err(copy_error)?;
    let mut kept_file = File::open(kept).map_err(kept_error)?;
    let mut copy_buffer = vec![0; COMPARISON_BUFFER_SIZE];
    let mut kept_buffer = vec![0; COMPARISON_BUFFER_SIZE];

    loop {
        let copy_read = fill(&mut copy_file, &mut copy_buffer).map_err(copy_error)?;
        let kept_read = fill(&mut kept_file, &mut kept_buffer).map_err(kept_error)?;
        if copy_buffer[..copy_read] != kept_buffer[..kept_read] {
            return Ok(false);
        }
        if copy_read == 0 {
            return Ok(true);
        }
    }
}

/// Reads until `buffer` is full or the file ends, so both files are always compared over the same
/// offsets whatever size each read returns.
fn fill(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// Never falls back from one disposal to the other: a copy the trash refuses stays in place.
fn dispose(copy: &Path, disposal: Disposal, trash: &TrashContext) -> RemovalOutcome {
    let result = match disposal {
        Disposal::Trash => trash.delete(copy).map_err(describe_trash_error),
        Disposal::Permanent => std::fs::remove_file(copy).map_err(|error| error.to_string()),
    };
    match result {
        Ok(()) => RemovalOutcome::Removed,
        Err(reason) => RemovalOutcome::RemovalFailed(reason),
    }
}

/// The trash crate prints its errors as debug structures; keep only the part a person can read.
fn describe_trash_error(error: trash::Error) -> String {
    match error {
        trash::Error::Unknown { description } | trash::Error::Os { description, .. } => description,
        trash::Error::CouldNotAccess { target } => format!("cannot access {target}"),
        other => other.to_string(),
    }
}

/// Finder, the crate's default on macOS, asks for permission to control Finder and plays a sound
/// for every file. NSFileManager does neither, and the file still lands in the Trash.
fn trash_context() -> TrashContext {
    #[cfg(target_os = "macos")]
    {
        let mut context = TrashContext::default();
        context.set_delete_method(DeleteMethod::NsFileManager);
        context
    }
    #[cfg(not(target_os = "macos"))]
    TrashContext::default()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    const CONTENT: &[u8] = b"the same bytes in every copy";

    fn write(path: &Path, content: &[u8]) -> PathBuf {
        std::fs::write(path, content).expect("the temporary folder is writable");
        path.to_path_buf()
    }

    fn removal(kept: &Path, removed: &[&Path]) -> GroupRemoval {
        GroupRemoval::new(
            kept.to_path_buf(),
            removed.iter().map(|path| path.to_path_buf()).collect(),
            CONTENT.len() as u64,
        )
        .expect("a valid removal")
    }

    fn outcomes(results: &[CopyRemoval]) -> Vec<(&Path, &RemovalOutcome)> {
        results
            .iter()
            .map(|result| (result.path.as_path(), &result.outcome))
            .collect()
    }

    #[test]
    fn a_removal_always_keeps_its_reference_copy() {
        let kept = PathBuf::from("kept");
        let copy = PathBuf::from("copy");

        assert_eq!(
            GroupRemoval::new(kept.clone(), vec![copy.clone(), kept.clone()], 1),
            None
        );
        assert_eq!(GroupRemoval::new(kept.clone(), Vec::new(), 1), None);
        assert!(GroupRemoval::new(kept, vec![copy], 1).is_some());
    }

    #[test]
    fn an_identical_copy_is_deleted_and_the_kept_one_stays() -> std::io::Result<()> {
        let folder = TempDir::new()?;
        let kept = write(&folder.path().join("kept"), CONTENT);
        let copy = write(&folder.path().join("copy"), CONTENT);

        let results = remove_copies(&[removal(&kept, &[&copy])], Disposal::Permanent, |_| {});

        assert_eq!(
            outcomes(&results),
            [(copy.as_path(), &RemovalOutcome::Removed)]
        );
        assert!(!copy.exists());
        assert_eq!(std::fs::read(&kept)?, CONTENT);
        Ok(())
    }

    #[test]
    fn a_copy_changed_since_the_scan_stays() -> std::io::Result<()> {
        let folder = TempDir::new()?;
        let kept = write(&folder.path().join("kept"), CONTENT);
        let changed = write(
            &folder.path().join("changed"),
            b"the same bytes in every cOpy",
        );
        let shorter = write(
            &folder.path().join("shorter"),
            &CONTENT[..CONTENT.len() - 1],
        );
        let longer = write(&folder.path().join("longer"), &[CONTENT, b"!"].concat());

        let results = remove_copies(
            &[removal(&kept, &[&changed, &shorter, &longer])],
            Disposal::Permanent,
            |_| {},
        );

        assert_eq!(
            outcomes(&results),
            [
                (changed.as_path(), &RemovalOutcome::ChangedSinceScan),
                (shorter.as_path(), &RemovalOutcome::ChangedSinceScan),
                (longer.as_path(), &RemovalOutcome::ChangedSinceScan),
            ]
        );
        assert!(changed.exists() && shorter.exists() && longer.exists());
        Ok(())
    }

    #[test]
    fn a_difference_past_the_first_buffer_is_still_found() -> std::io::Result<()> {
        let folder = TempDir::new()?;
        let content = vec![0x42; 2 * COMPARISON_BUFFER_SIZE + 123];
        let mut differs_at_the_end = content.clone();
        *differs_at_the_end.last_mut().expect("not empty") = 0x43;
        let kept = write(&folder.path().join("kept"), &content);
        let changed = write(&folder.path().join("changed"), &differs_at_the_end);
        let identical = write(&folder.path().join("identical"), &content);

        let results = remove_copies(
            &[removal(&kept, &[&changed, &identical])],
            Disposal::Permanent,
            |_| {},
        );

        assert_eq!(
            outcomes(&results),
            [
                (changed.as_path(), &RemovalOutcome::ChangedSinceScan),
                (identical.as_path(), &RemovalOutcome::Removed),
            ]
        );
        Ok(())
    }

    #[test]
    fn a_missing_copy_is_reported() -> std::io::Result<()> {
        let folder = TempDir::new()?;
        let kept = write(&folder.path().join("kept"), CONTENT);
        let missing = folder.path().join("missing");

        let results = remove_copies(&[removal(&kept, &[&missing])], Disposal::Permanent, |_| {});

        assert_eq!(
            outcomes(&results),
            [(
                missing.as_path(),
                &RemovalOutcome::CopyUnreadable(ErrorKind::NotFound)
            )]
        );
        Ok(())
    }

    #[test]
    fn without_its_kept_copy_no_copy_of_a_group_is_removed() -> std::io::Result<()> {
        let folder = TempDir::new()?;
        let missing_kept = folder.path().join("kept");
        let first = write(&folder.path().join("first"), CONTENT);
        let second = write(&folder.path().join("second"), CONTENT);

        let results = remove_copies(
            &[removal(&missing_kept, &[&first, &second])],
            Disposal::Permanent,
            |_| {},
        );

        let not_found = RemovalOutcome::KeptCopyUnreadable(ErrorKind::NotFound);
        assert_eq!(
            outcomes(&results),
            [
                (first.as_path(), &not_found),
                (second.as_path(), &not_found)
            ]
        );
        assert!(first.exists() && second.exists());
        Ok(())
    }

    #[test]
    fn each_copy_is_reported_as_soon_as_it_is_handled() -> std::io::Result<()> {
        let folder = TempDir::new()?;
        let kept = write(&folder.path().join("kept"), CONTENT);
        let first = write(&folder.path().join("first"), CONTENT);
        let other_kept = write(&folder.path().join("other_kept"), b"another group");
        let second = write(&folder.path().join("second"), b"another group");

        let mut reported = Vec::new();
        let results = remove_copies(
            &[removal(&kept, &[&first]), removal(&other_kept, &[&second])],
            Disposal::Permanent,
            |result| reported.push(result.clone()),
        );

        assert_eq!(reported, results);
        assert!(
            results
                .iter()
                .all(|result| result.file_size == CONTENT.len() as u64)
        );
        assert_eq!(
            outcomes(&results),
            [
                (first.as_path(), &RemovalOutcome::Removed),
                (second.as_path(), &RemovalOutcome::Removed),
            ]
        );
        Ok(())
    }

    /// Needs a user that permissions apply to: root deletes from the locked folder anyway.
    #[cfg(unix)]
    #[test]
    fn a_copy_the_system_refuses_to_delete_stays_with_the_reason() -> std::io::Result<()> {
        use std::{fs::Permissions, os::unix::fs::PermissionsExt};

        const READ_AND_ENTER_ONLY: u32 = 0o500;
        const OWNER_FULL_ACCESS: u32 = 0o700;

        let folder = TempDir::new()?;
        let kept = write(&folder.path().join("kept"), CONTENT);
        let locked = folder.path().join("locked");
        std::fs::create_dir(&locked)?;
        let copy = write(&locked.join("copy"), CONTENT);
        std::fs::set_permissions(&locked, Permissions::from_mode(READ_AND_ENTER_ONLY))?;

        let results = remove_copies(&[removal(&kept, &[&copy])], Disposal::Permanent, |_| {});
        std::fs::set_permissions(&locked, Permissions::from_mode(OWNER_FULL_ACCESS))?;

        let [
            CopyRemoval {
                path,
                outcome: RemovalOutcome::RemovalFailed(reason),
                ..
            },
        ] = results.as_slice()
        else {
            panic!("expected one failed removal, got {results:?}");
        };
        assert_eq!(path, &copy);
        assert!(reason.contains("ermission denied"), "{reason}");
        assert!(copy.exists());
        Ok(())
    }

    /// Unique per run, so the item is found again in the trash and cleaned up.
    fn unique_name() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after 1970")
            .as_nanos();
        format!("onlyfile-trash-test-{}-{nanos}", std::process::id())
    }

    #[test]
    fn trash_moves_the_copy_into_the_trash() -> Result<(), Box<dyn std::error::Error>> {
        let folder = TempDir::new()?;
        let kept = write(&folder.path().join("kept"), CONTENT);
        let name = unique_name();
        let copy = write(&folder.path().join(&name), CONTENT);

        let results = remove_copies(&[removal(&kept, &[&copy])], Disposal::Trash, |_| {});

        assert_eq!(
            outcomes(&results),
            [(copy.as_path(), &RemovalOutcome::Removed)]
        );
        assert!(!copy.exists());
        assert!(kept.exists());
        take_back_from_the_trash(&name)
    }

    /// Proves the file reached the trash, not a plain deletion, then empties it out of the trash.
    #[cfg(target_os = "macos")]
    fn take_back_from_the_trash(name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
        let trashed = PathBuf::from(home).join(".Trash").join(name);
        assert!(
            trashed.exists(),
            "{} is not in the Trash; the terminal may need Full Disk Access to read ~/.Trash",
            trashed.display()
        );
        std::fs::remove_file(&trashed)?;
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    fn take_back_from_the_trash(name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let trashed: Vec<_> = trash::os_limited::list()?
            .into_iter()
            .filter(|item| item.name == name)
            .collect();
        assert_eq!(trashed.len(), 1, "{name} is not in the trash");
        trash::os_limited::purge_all(trashed)?;
        Ok(())
    }
}
