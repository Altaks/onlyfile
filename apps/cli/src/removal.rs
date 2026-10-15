//! Runs the removal on a worker thread, logs every copy, and reports each one as it is handled.

use std::{
    sync::mpsc::{self, Receiver},
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

/// Starts removing on its own thread. The last event is always `Finished`.
pub fn spawn(removals: Vec<GroupRemoval>, disposal: Disposal) -> Receiver<RemovalEvent> {
    let (sender, receiver) = mpsc::sync_channel(EVENT_CHANNEL_CAPACITY);
    thread::spawn(move || {
        let mut handled_copies = 0;
        let results = purge::remove_copies(&removals, disposal, |result| {
            log_copy(result, disposal);
            handled_copies += 1;
            // A full channel means the interface is behind; the next report carries newer values.
            sender
                .try_send(RemovalEvent::Progress { handled_copies })
                .ok();
        });
        log_totals(&results);
        // Only fails once the interface has quit, and then nobody is left to tell.
        sender.send(RemovalEvent::Finished(results)).ok();
    });
    receiver
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
