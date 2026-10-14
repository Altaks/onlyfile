//! Runs the scan and the comparison on a worker thread so the interface keeps drawing, and reports
//! progress and the result through a channel.

use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

use model::{DuplicateGroup, SkipReason, SkippedEntry};
use replica::{ComparisonProgress, ReplicaError};
use scanner::ScannerError;
use tracing::{error, info, warn};

/// One frame at the interface's refresh rate. Sending more often only fills the channel.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);
/// Progress only needs its latest value, so a few slots absorb a slow frame without ever blocking
/// the worker.
const EVENT_CHANNEL_CAPACITY: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineProgress {
    Walking { files_found: usize },
    Comparing(ComparisonProgress),
}

#[derive(Debug)]
pub enum PipelineEvent {
    Progress(PipelineProgress),
    Finished(Result<ScanOutcome, PipelineError>),
}

#[derive(Debug)]
pub struct ScanOutcome {
    pub scanned_files: usize,
    pub groups: Vec<DuplicateGroup>,
    pub skipped: Vec<SkippedEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error(transparent)]
    Scan(#[from] ScannerError),
    #[error(transparent)]
    Compare(#[from] ReplicaError),
    #[error("the worker stopped without a result")]
    WorkerStopped,
}

/// Starts the scan of `root` on its own thread. The last event is always `Finished`.
pub fn spawn(root: PathBuf) -> Receiver<PipelineEvent> {
    let (sender, receiver) = mpsc::sync_channel(EVENT_CHANNEL_CAPACITY);
    thread::spawn(move || {
        let outcome = scan_and_compare(&root, &sender);
        // Only fails once the interface has quit, and then nobody is left to tell.
        sender.send(PipelineEvent::Finished(outcome)).ok();
    });
    receiver
}

fn scan_and_compare(
    root: &Path,
    sender: &SyncSender<PipelineEvent>,
) -> Result<ScanOutcome, PipelineError> {
    let started = Instant::now();
    info!(root = %root.display(), "scan started");
    let mut next_report = Instant::now();
    let mut report = |progress: PipelineProgress| {
        let now = Instant::now();
        if now >= next_report {
            next_report = now + PROGRESS_INTERVAL;
            // A full channel means the interface is behind; the next report carries newer values.
            sender.try_send(PipelineEvent::Progress(progress)).ok();
        }
    };

    let outcome = scanner::crawl_and_collect_files(root, |files_found| {
        report(PipelineProgress::Walking { files_found })
    })
    .map_err(PipelineError::from)
    .and_then(|scan| {
        let groups = replica::detect_duplicates(&scan.files, |progress| {
            report(PipelineProgress::Comparing(progress))
        })?;
        Ok(ScanOutcome {
            scanned_files: scan.files.len(),
            groups,
            skipped: scan.skipped,
        })
    });

    match &outcome {
        Ok(outcome) => {
            log_skipped_entries(&outcome.skipped);
            info!(
                scanned_files = outcome.scanned_files,
                duplicate_groups = outcome.groups.len(),
                skipped_entries = outcome.skipped.len(),
                elapsed = ?started.elapsed(),
                "scan finished"
            );
        }
        Err(error) => error!(%error, "scan failed"),
    }
    outcome
}

/// An unreadable entry is worth a look, so each one gets its own line. Symlinks, special and empty
/// files are left out by design and can number in the thousands, so they are only counted.
fn log_skipped_entries(skipped: &[SkippedEntry]) {
    for entry in skipped {
        if let SkipReason::Unreadable(kind) = entry.reason {
            warn!(path = %entry.path.display(), reason = %kind, "unreadable entry skipped");
        }
    }

    let count_of = |reason: SkipReason| {
        skipped
            .iter()
            .filter(|entry| entry.reason == reason)
            .count()
    };
    info!(
        symlinks = count_of(SkipReason::Symlink),
        special_files = count_of(SkipReason::SpecialFile),
        empty_files = count_of(SkipReason::EmptyFile),
        "entries left out by design"
    );
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn the_last_event_carries_every_duplicate_group_and_skipped_entry()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = TempDir::new()?;
        std::fs::write(root.path().join("original"), "same content")?;
        std::fs::write(root.path().join("copy"), "same content")?;
        std::fs::write(root.path().join("unique"), "other content")?;
        std::fs::write(root.path().join("empty"), "")?;

        let last_event = spawn(root.path().to_path_buf())
            .into_iter()
            .last()
            .expect("the worker always sends a final event");

        let PipelineEvent::Finished(Ok(outcome)) = last_event else {
            panic!("expected a successful outcome, got {last_event:?}");
        };
        assert_eq!(outcome.scanned_files, 3);
        let mut grouped_paths: Vec<PathBuf> = outcome
            .groups
            .iter()
            .flat_map(|group| group.copies().iter().map(|copy| copy.path.clone()))
            .collect();
        grouped_paths.sort();
        assert_eq!(
            grouped_paths,
            [root.path().join("copy"), root.path().join("original")]
        );
        assert_eq!(
            outcome.skipped,
            [SkippedEntry {
                path: root.path().join("empty"),
                reason: SkipReason::EmptyFile,
            }]
        );
        Ok(())
    }

    #[test]
    fn a_failing_scan_ends_with_its_error() {
        let missing_root = PathBuf::from("/this/folder/does/not/exist");

        let last_event = spawn(missing_root)
            .into_iter()
            .last()
            .expect("the worker always sends a final event");

        assert!(
            matches!(
                last_event,
                PipelineEvent::Finished(Err(PipelineError::Scan(_)))
            ),
            "got {last_event:?}"
        );
    }
}
