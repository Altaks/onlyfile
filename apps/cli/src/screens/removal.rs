//! The removal's three steps over the results: the confirmation, the progress, and the summary.

use std::path::Path;

use purge::{CopyRemoval, Disposal, RemovalOutcome};
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    text::Line,
    widgets::{Block, Clear, Gauge, ListState, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    format::{format_bytes, format_path, format_quantity, format_removal_outcome, truncate_start},
    screens::{BORDER_CELLS, HIGHLIGHT_SYMBOL, Step, pane_list, step_selection},
};

const DIALOG_WIDTH_PERCENT: u16 = 70;
/// Two borders around a line of text and a gauge.
const PROGRESS_HEIGHT: u16 = 4;
const SUMMARY_WIDTH_PERCENT: u16 = 90;
const SUMMARY_HEIGHT_PERCENT: u16 = 80;
const REASON_GAP: &str = "  ";
const INDENT: &str = "  ";

#[derive(Debug)]
pub enum RemovalDialog {
    Confirming {
        copies: usize,
        bytes: u64,
    },
    Removing {
        handled_copies: usize,
        total_copies: usize,
        /// Asked to stop once the copy it is on is handled.
        stopping: bool,
    },
    Summary(RemovalSummary),
}

#[derive(Debug)]
pub struct RemovalSummary {
    rows: Vec<SummaryRow>,
    selection: ListState,
}

/// A line of the summary, with the path kept apart so it can be cut to the width of the screen.
#[derive(Debug)]
struct SummaryRow {
    text: String,
    path: Option<String>,
}

impl SummaryRow {
    fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            path: None,
        }
    }
}

impl RemovalSummary {
    /// `not_reached` counts the marked copies a stopped removal never got to.
    pub fn new(
        results: &[CopyRemoval],
        not_reached: usize,
        disposal: Disposal,
        log_label: &str,
        home: &Path,
    ) -> Self {
        let removed: Vec<&CopyRemoval> = results
            .iter()
            .filter(|result| result.outcome == RemovalOutcome::Removed)
            .collect();
        let removed_bytes = format_bytes(removed.iter().map(|result| result.file_size).sum());
        let copies = format_quantity(removed.len(), "copy", "copies");
        // The trash keeps what it receives: the space only comes back once it is emptied.
        let headline = match (removed.len(), disposal) {
            (0, _) => "No copy was removed.".to_owned(),
            (_, Disposal::Trash) => format!(
                "Moved {copies} ({removed_bytes}) to the trash; emptying the trash frees the space."
            ),
            (_, Disposal::Permanent) => {
                format!("Deleted {copies} permanently, freeing {removed_bytes}.")
            }
        };

        let mut rows = vec![SummaryRow::text(headline)];
        if not_reached > 0 {
            rows.push(SummaryRow::text(format!(
                "Stopped early: {} not handled, still marked.",
                format_quantity(not_reached, "copy", "copies")
            )));
        }
        let failed =
            |result: &&CopyRemoval| matches!(result.outcome, RemovalOutcome::RemovalFailed(_));
        let left_in_place: Vec<&CopyRemoval> = results
            .iter()
            .filter(|result| result.outcome != RemovalOutcome::Removed && !failed(result))
            .collect();
        let could_not_remove: Vec<&CopyRemoval> = results.iter().filter(failed).collect();
        for (heading, entries) in [
            ("Left in place", left_in_place),
            ("Could not remove", could_not_remove),
        ] {
            if entries.is_empty() {
                continue;
            }
            rows.push(SummaryRow::text(""));
            rows.push(SummaryRow::text(format!("{heading} ({}):", entries.len())));
            rows.extend(entries.into_iter().map(|result| SummaryRow {
                text: format!("{INDENT}{}", format_removal_outcome(&result.outcome)),
                path: Some(format_path(&result.path, home)),
            }));
        }
        rows.push(SummaryRow::text(""));
        // On its own row: the log path is long, and it is the part worth reading in full.
        rows.push(SummaryRow::text("Every removal is recorded in:"));
        rows.push(SummaryRow::text(format!("{INDENT}{log_label}")));

        Self {
            rows,
            selection: ListState::default().with_selected(Some(0)),
        }
    }

    pub fn move_selection(&mut self, step: Step) {
        step_selection(&mut self.selection, self.rows.len(), step);
    }
}

pub fn key_hints(dialog: &RemovalDialog, disposal: Disposal) -> &'static str {
    match (dialog, disposal) {
        (RemovalDialog::Confirming { .. }, Disposal::Trash) => " y move to the trash  n cancel ",
        (RemovalDialog::Confirming { .. }, Disposal::Permanent) => " y delete  n cancel ",
        (
            RemovalDialog::Removing {
                stopping: false, ..
            },
            _,
        ) => " ctrl-c stop after this copy ",
        (RemovalDialog::Removing { stopping: true, .. }, _) => " ctrl-c quit now ",
        (RemovalDialog::Summary(_), _) => " ↑↓ scroll  enter continue  q quit ",
    }
}

pub fn render(dialog: &mut RemovalDialog, disposal: Disposal, frame: &mut Frame, area: Rect) {
    match dialog {
        RemovalDialog::Confirming { copies, bytes } => {
            let copies = format_quantity(*copies, "copy", "copies");
            let question = match disposal {
                Disposal::Trash => {
                    format!("Move {copies} ({}) to the trash?", format_bytes(*bytes))
                }
                Disposal::Permanent => format!(
                    "Delete {copies} ({}) permanently? This cannot be undone.",
                    format_bytes(*bytes)
                ),
            };
            let lines = [
                question,
                "Each one is first compared byte for byte with the copy its group keeps."
                    .to_owned(),
            ];
            let width = dialog_width(area);
            let text = Paragraph::new(lines.map(Line::from).to_vec())
                .wrap(Wrap { trim: true })
                .block(Block::bordered().title(" Remove marked copies "));
            // Measured by the paragraph's own word wrapping, borders included, so the box is
            // exactly as tall as the text it will draw.
            let height = text.line_count(width.saturating_sub(BORDER_CELLS));
            let dialog_area = centered(
                area,
                Constraint::Length(width),
                Constraint::Length(u16::try_from(height).unwrap_or(u16::MAX)),
            );
            frame.render_widget(Clear, dialog_area);
            frame.render_widget(text, dialog_area);
        }
        RemovalDialog::Removing {
            handled_copies,
            total_copies,
            stopping,
        } => {
            let action = match (*stopping, disposal) {
                (true, _) => "Stopping after the current copy",
                (false, Disposal::Trash) => "Moving copies to the trash",
                (false, Disposal::Permanent) => "Deleting copies",
            };
            let dialog_area = centered(
                area,
                Constraint::Percentage(DIALOG_WIDTH_PERCENT),
                Constraint::Length(PROGRESS_HEIGHT),
            );
            let block = Block::bordered().title(" Removing ");
            let inner = block.inner(dialog_area);
            let [text_area, gauge_area] =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
            frame.render_widget(Clear, dialog_area);
            frame.render_widget(block, dialog_area);
            frame.render_widget(
                Paragraph::new(format!("{action} · {handled_copies} / {total_copies}")),
                text_area,
            );
            let ratio = if *total_copies == 0 {
                1.0
            } else {
                *handled_copies as f64 / *total_copies as f64
            };
            frame.render_widget(Gauge::default().ratio(ratio.min(1.0)), gauge_area);
        }
        RemovalDialog::Summary(summary) => {
            let dialog_area = centered(
                area,
                Constraint::Percentage(SUMMARY_WIDTH_PERCENT),
                Constraint::Percentage(SUMMARY_HEIGHT_PERCENT),
            );
            let text_width = usize::from(dialog_area.width.saturating_sub(BORDER_CELLS))
                .saturating_sub(HIGHLIGHT_SYMBOL.width());
            let rows = summary.rows.iter().map(|row| match &row.path {
                None => row.text.clone(),
                Some(path) => {
                    let path_width =
                        text_width.saturating_sub(row.text.width() + REASON_GAP.width());
                    format!(
                        "{}{REASON_GAP}{}",
                        row.text,
                        truncate_start(path, path_width)
                    )
                }
            });
            frame.render_widget(Clear, dialog_area);
            frame.render_stateful_widget(
                pane_list(rows, " Removal summary ".to_owned(), true),
                dialog_area,
                &mut summary.selection,
            );
        }
    }
}

/// The width of a dialog drawn over `area`.
fn dialog_width(area: Rect) -> u16 {
    let [dialog] = Layout::horizontal([Constraint::Percentage(DIALOG_WIDTH_PERCENT)])
        .flex(Flex::Center)
        .areas(area);
    dialog.width
}

/// A box of the given size in the middle of `area`.
fn centered(area: Rect, width: Constraint, height: Constraint) -> Rect {
    let [row] = Layout::vertical([height]).flex(Flex::Center).areas(area);
    let [dialog] = Layout::horizontal([width]).flex(Flex::Center).areas(row);
    dialog
}
