//! The two-pane screen: duplicate groups on the left, the copies of the selected group on the right,
//! and the list of skipped entries one key away.

use std::path::Path;

use jiff::tz::TimeZone;
use model::DuplicateGroup;
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Block, ListState, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    format::{
        DATE_WIDTH, format_bytes, format_count, format_date, format_path, format_quantity,
        truncate_end, truncate_start,
    },
    pipeline::ScanOutcome,
    screens::{
        BORDER_CELLS, HIGHLIGHT_SYMBOL, KeyHandling, Step, pane_list, screen_title,
        skipped::SkippedList, step_selection,
    },
};

const GROUPS_PANE_PERCENT: u16 = 35;
/// `1023 MiB` is the widest a size gets below 1024 PiB.
const GROUP_SIZE_WIDTH: usize = 8;
const COLUMN_GAP: &str = "  ";
const GROUPS_PANE_TITLE: &str = " Groups ";
const SKIPPED_KEY_HINTS: &str = " ↑↓ move  esc back  q quit ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Duplicates,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Groups,
    Copies,
}

#[derive(Debug)]
pub struct ResultsScreen {
    root_label: String,
    scanned_files: usize,
    reclaimable_bytes: u64,
    groups: Vec<GroupView>,
    skipped: SkippedList,
    view: View,
    focus: Pane,
    group_selection: ListState,
    copy_selection: ListState,
}

/// What the screen shows of one duplicate group, formatted once when the results arrive.
#[derive(Debug)]
struct GroupView {
    /// `412 MiB ×3 IMG_2041.mov`
    row: String,
    /// `IMG_2041.mov · 412 MiB · 3 copies`
    title: String,
    copies: Vec<CopyView>,
}

#[derive(Debug)]
struct CopyView {
    path: String,
    modified_date: String,
}

impl GroupView {
    fn new(group: &DuplicateGroup, time_zone: &TimeZone, home: &Path) -> Self {
        let first_path = &group.copies()[0].path;
        let name = first_path.file_name().map_or_else(
            || first_path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let file_size = format_bytes(group.file_size());
        let copy_count = group.copies().len();

        Self {
            row: format!("{file_size:>GROUP_SIZE_WIDTH$} ×{copy_count} {name}"),
            title: format!(" {name} · {file_size} · {copy_count} copies "),
            copies: group
                .copies()
                .iter()
                .map(|copy| CopyView {
                    path: format_path(&copy.path, home),
                    modified_date: format_date(copy.modified, time_zone),
                })
                .collect(),
        }
    }
}

impl ResultsScreen {
    pub fn new(
        outcome: ScanOutcome,
        root_label: String,
        time_zone: &TimeZone,
        home: &Path,
    ) -> Self {
        let mut groups = outcome.groups;
        // Largest gain first. The first copy's path breaks ties so the order is the same every run.
        groups.sort_by(|left, right| {
            right
                .reclaimable_bytes()
                .cmp(&left.reclaimable_bytes())
                .then_with(|| left.copies()[0].path.cmp(&right.copies()[0].path))
        });
        let first_row = (!groups.is_empty()).then_some(0);

        Self {
            root_label,
            scanned_files: outcome.scanned_files,
            reclaimable_bytes: groups.iter().map(DuplicateGroup::reclaimable_bytes).sum(),
            groups: groups
                .iter()
                .map(|group| GroupView::new(group, time_zone, home))
                .collect(),
            skipped: SkippedList::new(outcome.skipped, home),
            view: View::Duplicates,
            focus: Pane::Groups,
            group_selection: ListState::default().with_selected(first_row),
            copy_selection: ListState::default().with_selected(first_row),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> KeyHandling {
        match (self.view, key.code) {
            (View::Skipped, KeyCode::Esc | KeyCode::Char('s')) => self.view = View::Duplicates,
            (View::Skipped, KeyCode::Up) => self.skipped.move_selection(Step::Previous),
            (View::Skipped, KeyCode::Down) => self.skipped.move_selection(Step::Next),
            (View::Duplicates, KeyCode::Char('s')) if !self.skipped.is_empty() => {
                self.view = View::Skipped;
            }
            (View::Duplicates, KeyCode::Up) => self.move_selection(Step::Previous),
            (View::Duplicates, KeyCode::Down) => self.move_selection(Step::Next),
            (View::Duplicates, KeyCode::Tab | KeyCode::BackTab) => {
                self.focus = match self.focus {
                    Pane::Groups => Pane::Copies,
                    Pane::Copies => Pane::Groups,
                };
            }
            _ => return KeyHandling::Ignored,
        }
        KeyHandling::Consumed
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let details = if self.groups.is_empty() {
            String::new()
        } else {
            format!(
                " · {} · {} reclaimable",
                format_quantity(self.groups.len(), "group", "groups"),
                format_bytes(self.reclaimable_bytes)
            )
        };
        let key_hints = match self.view {
            View::Skipped => SKIPPED_KEY_HINTS.to_owned(),
            View::Duplicates => self.duplicates_key_hints(),
        };
        let outer = Block::bordered()
            .title(screen_title(&self.root_label, &details, frame.area().width))
            .title_bottom(key_hints);
        let inner = outer.inner(frame.area());
        frame.render_widget(outer, frame.area());

        match self.view {
            View::Skipped => self.skipped.render(frame, inner),
            View::Duplicates if self.groups.is_empty() => self.render_empty(frame, inner),
            View::Duplicates => self.render_duplicates(frame, inner),
        }
    }

    fn duplicates_key_hints(&self) -> String {
        let navigation = if self.groups.is_empty() {
            ""
        } else {
            "↑↓ move  tab switch pane  "
        };
        let skipped = if self.skipped.is_empty() {
            String::new()
        } else {
            format!("s skipped ({})  ", format_count(self.skipped.len()))
        };
        format!(" {navigation}{skipped}q quit ")
    }

    fn render_duplicates(&mut self, frame: &mut Frame, area: Rect) {
        let [groups_area, copies_area] = Layout::horizontal([
            Constraint::Percentage(GROUPS_PANE_PERCENT),
            Constraint::Fill(1),
        ])
        .areas(area);

        let group_row_width = usize::from(groups_area.width.saturating_sub(BORDER_CELLS))
            .saturating_sub(HIGHLIGHT_SYMBOL.width());
        let group_rows = self
            .groups
            .iter()
            .map(|group| truncate_end(&group.row, group_row_width));
        frame.render_stateful_widget(
            pane_list(
                group_rows,
                GROUPS_PANE_TITLE.to_owned(),
                self.focus == Pane::Groups,
            ),
            groups_area,
            &mut self.group_selection,
        );

        let path_width = usize::from(copies_area.width.saturating_sub(BORDER_CELLS))
            .saturating_sub(HIGHLIGHT_SYMBOL.width() + COLUMN_GAP.width() + DATE_WIDTH);
        let Some(group) = self.selected_group() else {
            return;
        };
        let title = group.title.clone();
        let copy_rows: Vec<String> = group
            .copies
            .iter()
            .map(|copy| {
                let path = truncate_start(&copy.path, path_width);
                let padding = " ".repeat(path_width.saturating_sub(path.width()));
                format!("{path}{padding}{COLUMN_GAP}{}", copy.modified_date)
            })
            .collect();
        frame.render_stateful_widget(
            pane_list(copy_rows, title, self.focus == Pane::Copies),
            copies_area,
            &mut self.copy_selection,
        );
    }

    fn render_empty(&self, frame: &mut Frame, area: Rect) {
        let [_, message_area, _] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(2),
            Constraint::Fill(1),
        ])
        .areas(area);
        let scanned = format_quantity(self.scanned_files, "file", "files");
        let totals = if self.skipped.is_empty() {
            format!("{scanned} scanned.")
        } else {
            format!(
                "{scanned} scanned, {} skipped.",
                format_count(self.skipped.len())
            )
        };
        let message = Paragraph::new(vec![
            Line::from(format!("No duplicates found in {}.", self.root_label)),
            Line::from(totals),
        ])
        .centered();
        frame.render_widget(message, message_area);
    }

    fn selected_group(&self) -> Option<&GroupView> {
        self.group_selection
            .selected()
            .and_then(|index| self.groups.get(index))
    }

    fn move_selection(&mut self, step: Step) {
        match self.focus {
            Pane::Groups => {
                if step_selection(&mut self.group_selection, self.groups.len(), step) {
                    self.copy_selection.select(Some(0));
                }
            }
            Pane::Copies => {
                let copy_count = self.selected_group().map_or(0, |group| group.copies.len());
                step_selection(&mut self.copy_selection, copy_count, step);
            }
        }
    }
}
