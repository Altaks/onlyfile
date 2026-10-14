//! The list of entries the scan left out, each with its reason.

use std::path::Path;

use model::SkippedEntry;
use ratatui::{Frame, layout::Rect, widgets::ListState};
use unicode_width::UnicodeWidthStr;

use crate::{
    format::{format_path, format_quantity, format_skip_reason, truncate_start},
    screens::{BORDER_CELLS, HIGHLIGHT_SYMBOL, Step, pane_list, step_selection},
};

const COLUMN_GAP: &str = "  ";

#[derive(Debug)]
pub struct SkippedList {
    rows: Vec<SkippedRow>,
    reason_width: usize,
    selection: ListState,
}

#[derive(Debug)]
struct SkippedRow {
    reason: String,
    path: String,
}

impl SkippedList {
    pub fn new(mut skipped: Vec<SkippedEntry>, home: &Path) -> Self {
        skipped.sort_by(|left, right| left.path.cmp(&right.path));
        let rows: Vec<SkippedRow> = skipped
            .iter()
            .map(|entry| SkippedRow {
                reason: format_skip_reason(entry.reason),
                path: format_path(&entry.path, home),
            })
            .collect();

        Self {
            reason_width: rows.iter().map(|row| row.reason.width()).max().unwrap_or(0),
            selection: ListState::default().with_selected((!rows.is_empty()).then_some(0)),
            rows,
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn move_selection(&mut self, step: Step) {
        step_selection(&mut self.selection, self.rows.len(), step);
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let path_width = usize::from(area.width.saturating_sub(BORDER_CELLS))
            .saturating_sub(HIGHLIGHT_SYMBOL.width() + self.reason_width + COLUMN_GAP.width());
        let rows = self.rows.iter().map(|row| {
            let padding = " ".repeat(self.reason_width.saturating_sub(row.reason.width()));
            format!(
                "{}{padding}{COLUMN_GAP}{}",
                row.reason,
                truncate_start(&row.path, path_width)
            )
        });
        let title = format!(
            " Skipped · {} ",
            format_quantity(self.rows.len(), "entry", "entries")
        );
        frame.render_stateful_widget(pane_list(rows, title, true), area, &mut self.selection);
    }
}
