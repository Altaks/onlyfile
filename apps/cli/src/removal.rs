//! Runs the removal on a worker thread, logs every copy, and reports each one as it is handled.

use std::{
    ops::ControlFlow,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
};

use purge::{CopyRemoval, Disposal, GroupRemoval, RemovalOutcome};
use tracing::{info, warn};

use crate::format::format_removal_outcome;

/// Progress only needs its latest value, so a few slots absorb a slow frame without ever blocking
/// the worker.
const EVENT_CHANNEL_CAPACITY: usize = 16;

#[derive(Debug)]
pub enum RemovalEvent {
    Progress { handled_copies: usize },
    Finished(Vec<CopyRemoval>),
}

/// A removal running on its worker thread.
#[derive(Debug)]
pub struct RunningRemoval {
    pub events: Receiver<RemovalEvent>,
    pub stop: Arc<AtomicBool>,
}

impl RunningRemoval {
    /// Asks the worker to stop once the copy it is on is handled and logged. Returns `false` when
    /// that was already asked.
    pub fn request_stop(&self) -> bool {
        !self.stop.swap(true, Ordering::Relaxed)
    }
}

/// Starts removing on its own thread, stopping once a copy is handled while `stop` is set. The last
/// event is always `Finished`, stopped early or not.
pub fn spawn(
    removals: Vec<GroupRemoval>,
    disposal: Disposal,
    stop: Arc<AtomicBool>,
) -> RunningRemoval {
    let (sender, events) = mpsc::sync_channel(EVENT_CHANNEL_CAPACITY);
    let stop_requested = Arc::clone(&stop);
    thread::spawn(move || {
        let mut handled_copies = 0;
        let results = purge::remove_copies(&removals, disposal, |result| {
            log_copy(result, disposal);
            handled_copies += 1;
            // A full channel means the interface is behind; the next report carries newer values.
            sender
                .try_send(RemovalEvent::Progress { handled_copies })
                .ok();
            if stop_requested.load(Ordering::Relaxed) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        log_totals(&results);
        // Only fails once the interface has quit, and then nobody is left to tell.
        sender.send(RemovalEvent::Finished(results)).ok();
    });
    RunningRemoval { events, stop }
}

/// The record a person reads to know what left their disk, and how to get it back.
fn log_copy(result: &CopyRemoval, disposal: Disposal) {
    let path = result.path.display();
    match &result.outcome {
        RemovalOutcome::Removed => info!(
            path = %path,
            size = result.file_size,
            mode = disposal_mode(disposal),
            "copy removed"
        ),
        RemovalOutcome::RemovalFailed(reason) => {
            warn!(path = %path, %reason, "copy removal failed");
        }
        left_in_place => warn!(
            path = %path,
            reason = %format_removal_outcome(left_in_place),
            "copy left in place"
        ),
    }
}

fn log_totals(results: &[CopyRemoval]) {
    let count_of = |matches: fn(&RemovalOutcome) -> bool| {
        results
            .iter()
            .filter(|result| matches(&result.outcome))
            .count()
    };
    info!(
        removed = count_of(|outcome| *outcome == RemovalOutcome::Removed),
        failed = count_of(|outcome| matches!(outcome, RemovalOutcome::RemovalFailed(_))),
        left_in_place = count_of(|outcome| {
            !matches!(
                outcome,
                RemovalOutcome::Removed | RemovalOutcome::RemovalFailed(_)
            )
        }),
        freed_bytes = results
            .iter()
            .filter(|result| result.outcome == RemovalOutcome::Removed)
            .map(|result| result.file_size)
            .sum::<u64>(),
        "removal finished"
    );
}

const fn disposal_mode(disposal: Disposal) -> &'static str {
    match disposal {
        Disposal::Trash => "trash",
        Disposal::Permanent => "permanent",
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn a_worker_asked_to_stop_handles_one_copy_then_finishes() -> std::io::Result<()> {
        let folder = TempDir::new()?;
        let content = b"identical";
        let kept = folder.path().join("kept");
        std::fs::write(&kept, content)?;
        let copies: Vec<_> = ["first", "second", "third"]
            .iter()
            .map(|name| folder.path().join(name))
            .collect();
        for copy in &copies {
            std::fs::write(copy, content)?;
        }
        let removal =
            GroupRemoval::new(kept, copies.clone(), content.len() as u64).expect("a valid removal");

        let running = spawn(
            vec![removal],
            Disposal::Permanent,
            Arc::new(AtomicBool::new(true)),
        );
        let last_event = running
            .events
            .into_iter()
            .last()
            .expect("the worker always sends a final event");

        let RemovalEvent::Finished(results) = last_event else {
            panic!("expected the final result, got {last_event:?}");
        };
        assert_eq!(results.len(), 1, "{results:?}");
        assert_eq!(copies.iter().filter(|copy| copy.exists()).count(), 2);
        Ok(())
    }
}
