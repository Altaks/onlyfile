//! The two-pane screen: duplicate groups on the left, the copies of the selected group on the right,
//! the list of skipped entries one key away, and the removal of marked copies.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use jiff::tz::TimeZone;
use model::DuplicateGroup;
use purge::{CopyRemoval, Disposal, GroupRemoval, RemovalOutcome};
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
        BORDER_CELLS, HIGHLIGHT_SYMBOL, KeyHandling, Step, help, pane_list,
        removal::{self, RemovalDialog, RemovalSummary},
        screen_title,
        skipped::SkippedList,
        step_selection,
    },
    settings::RunSettings,
};

const GROUPS_PANE_PERCENT: u16 = 35;
/// `1023 MiB` is the widest a size gets below 1024 PiB.
const GROUP_SIZE_WIDTH: usize = 8;
const COLUMN_GAP: &str = "  ";
const GROUPS_PANE_TITLE: &str = " Groups ";
const MARKED_BOX: &str = "[x] ";
const UNMARKED_BOX: &str = "[ ] ";
const SKIPPED_KEY_HINTS: &str = " ↑↓ move  esc back  ? help  q quit ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Duplicates,
    Skipped,
}

/// What can open over the results: one at a time.
#[derive(Debug)]
enum Overlay {
    Help,
    Removal(RemovalDialog),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Groups,
    Copies,
}

#[derive(Debug)]
pub struct ResultsScreen {
    root_label: String,
    home: PathBuf,
    disposal: Disposal,
    log_label: String,
    scanned_files: usize,
    groups: Vec<GroupView>,
    skipped: SkippedList,
    view: View,
    focus: Pane,
    group_selection: ListState,
    copy_selection: ListState,
    /// Why the last key did nothing, shown until the next key.
    notice: Option<String>,
    overlay: Option<Overlay>,
    /// Taken off the scanned folder by every removal since the scan.
    removed_bytes: u64,
}

/// What the screen shows of one duplicate group.
#[derive(Debug)]
struct GroupView {
    /// `IMG_2041.mov`, the first copy's file name at the scan.
    name: String,
    file_size: u64,
    copies: Vec<CopyView>,
}

#[derive(Debug)]
struct CopyView {
    source: PathBuf,
    /// The path as the screen writes it, home shown as `~`.
    path: String,
    modified_date: String,
    marked: bool,
}

impl GroupView {
    fn new(group: &DuplicateGroup, time_zone: &TimeZone, home: &Path) -> Self {
        let first_path = &group.copies()[0].path;
        Self {
            name: first_path.file_name().map_or_else(
                || first_path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
            file_size: group.file_size(),
            copies: group
                .copies()
                .iter()
                .map(|copy| CopyView {
                    source: copy.path.clone(),
                    path: format_path(&copy.path, home),
                    modified_date: format_date(copy.modified, time_zone),
                    marked: false,
                })
                .collect(),
        }
    }

    /// `412 MiB ×3 IMG_2041.mov`
    fn row(&self) -> String {
        format!(
            "{:>GROUP_SIZE_WIDTH$} ×{} {}",
            format_bytes(self.file_size),
            self.copies.len(),
            self.name
        )
    }

    /// `IMG_2041.mov · 412 MiB · 3 copies`
    fn title(&self) -> String {
        format!(
            " {} · {} · {} copies ",
            self.name,
            format_bytes(self.file_size),
            self.copies.len()
        )
    }

    fn reclaimable_bytes(&self) -> u64 {
        model::reclaimable_bytes(self.file_size, self.copies.len())
    }

    /// What removing this group's marked copies takes, or `None` when none is marked. The first
    /// unmarked copy is the one every removed copy is compared with.
    fn removal(&self) -> Option<GroupRemoval> {
        let kept = self.copies.iter().find(|copy| !copy.marked)?;
        let removed = self
            .copies
            .iter()
            .filter(|copy| copy.marked)
            .map(|copy| copy.source.clone())
            .collect();
        GroupRemoval::new(kept.source.clone(), removed, self.file_size)
    }
}

impl ResultsScreen {
    pub fn new(outcome: ScanOutcome, settings: &RunSettings) -> Self {
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
            root_label: settings.root_label.clone(),
            home: settings.home.clone(),
            disposal: settings.disposal,
            log_label: settings.log_label.clone(),
            scanned_files: outcome.scanned_files,
            groups: groups
                .iter()
                .map(|group| GroupView::new(group, &settings.time_zone, &settings.home))
                .collect(),
            skipped: SkippedList::new(outcome.skipped, &settings.home),
            view: View::Duplicates,
            focus: Pane::Groups,
            group_selection: ListState::default().with_selected(first_row),
            copy_selection: ListState::default().with_selected(first_row),
            notice: None,
            overlay: None,
            removed_bytes: 0,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> KeyHandling {
        self.notice = None;
        if let Some(Overlay::Help) = self.overlay {
            return match key.code {
                KeyCode::Char('?') | KeyCode::Esc => {
                    self.overlay = None;
                    KeyHandling::Consumed
                }
                KeyCode::Char('q') => KeyHandling::Ignored,
                _ => KeyHandling::Consumed,
            };
        }
        if let Some(Overlay::Removal(dialog)) = &mut self.overlay {
            return match (dialog, key.code) {
                (RemovalDialog::Confirming { copies, .. }, KeyCode::Char('y')) => {
                    let total_copies = *copies;
                    self.overlay = Some(Overlay::Removal(RemovalDialog::Removing {
                        handled_copies: 0,
                        total_copies,
                        stopping: false,
                    }));
                    KeyHandling::Remove(self.groups.iter().filter_map(GroupView::removal).collect())
                }
                (RemovalDialog::Confirming { .. }, KeyCode::Char('n') | KeyCode::Esc)
                | (RemovalDialog::Summary(_), KeyCode::Enter | KeyCode::Esc) => {
                    self.overlay = None;
                    KeyHandling::Consumed
                }
                (RemovalDialog::Summary(summary), KeyCode::Up) => {
                    summary.move_selection(Step::Previous);
                    KeyHandling::Consumed
                }
                (RemovalDialog::Summary(summary), KeyCode::Down) => {
                    summary.move_selection(Step::Next);
                    KeyHandling::Consumed
                }
                (RemovalDialog::Summary(_), _) => KeyHandling::Ignored,
                // Waiting on the answer or on the worker: no other key means anything here.
                (RemovalDialog::Confirming { .. } | RemovalDialog::Removing { .. }, _) => {
                    KeyHandling::Consumed
                }
            };
        }

        match (self.view, key.code) {
            (_, KeyCode::Char('?')) => self.overlay = Some(Overlay::Help),
            (View::Skipped, KeyCode::Esc | KeyCode::Char('s')) => self.view = View::Duplicates,
            (View::Skipped, KeyCode::Up) => self.skipped.move_selection(Step::Previous),
            (View::Skipped, KeyCode::Down) => self.skipped.move_selection(Step::Next),
            (View::Duplicates, KeyCode::Char('s')) if !self.skipped.is_empty() => {
                self.view = View::Skipped;
            }
            (View::Duplicates, KeyCode::Up) => self.move_selection(Step::Previous),
            (View::Duplicates, KeyCode::Down) => self.move_selection(Step::Next),
            (View::Duplicates, KeyCode::Char(' ')) => self.toggle_highlighted_copy(),
            (View::Duplicates, KeyCode::Char('k')) => self.keep_only_highlighted_copy(),
            (View::Duplicates, KeyCode::Char('d')) => self.ask_to_remove_marked_copies(),
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

    pub fn on_removal_progress(&mut self, handled: usize) {
        if let Some(Overlay::Removal(RemovalDialog::Removing { handled_copies, .. })) =
            &mut self.overlay
        {
            *handled_copies = handled;
        }
    }

    /// Shows the summary, drops every copy that is no longer part of its group and every group left
    /// with one copy, and unmarks the handled copies that stayed: they were left in place for a
    /// reason the summary gives.
    pub fn on_removal_stopping(&mut self) {
        if let Some(Overlay::Removal(RemovalDialog::Removing { stopping, .. })) = &mut self.overlay
        {
            *stopping = true;
        }
    }

    pub fn on_removal_finished(&mut self, results: &[CopyRemoval]) {
        let not_reached = match &self.overlay {
            Some(Overlay::Removal(RemovalDialog::Removing { total_copies, .. })) => {
                total_copies.saturating_sub(results.len())
            }
            _ => 0,
        };
        self.removed_bytes += purge::removed_bytes(results);
        // Removed, proven different, or found to be the kept file under another name: either way
        // no longer a copy of its group. An unreadable copy proves nothing and stays.
        let out_of_their_group: HashSet<&Path> = results
            .iter()
            .filter(|result| {
                matches!(
                    result.outcome,
                    RemovalOutcome::Removed
                        | RemovalOutcome::DiffersFromKept
                        | RemovalOutcome::SameFileAsKept
                )
            })
            .map(|result| result.path.as_path())
            .collect();

        // A copy the removal never reached stays marked, so `d` picks up where it stopped.
        let handled: HashSet<&Path> = results.iter().map(|result| result.path.as_path()).collect();
        for group in &mut self.groups {
            group
                .copies
                .retain(|copy| !out_of_their_group.contains(copy.source.as_path()));
            for copy in &mut group.copies {
                if handled.contains(copy.source.as_path()) {
                    copy.marked = false;
                }
            }
        }
        self.groups.retain(|group| group.copies.len() > 1);
        let selected_group = match self.groups.len() {
            0 => None,
            group_count => Some(
                self.group_selection
                    .selected()
                    .unwrap_or(0)
                    .min(group_count - 1),
            ),
        };
        self.group_selection.select(selected_group);
        self.copy_selection.select(selected_group.map(|_| 0));

        self.overlay = Some(Overlay::Removal(RemovalDialog::Summary(
            RemovalSummary::new(
                results,
                not_reached,
                self.disposal,
                &self.log_label,
                &self.home,
            ),
        )));
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let details = if self.groups.is_empty() {
            String::new()
        } else {
            format!(
                " · {} · {} reclaimable{}",
                format_quantity(self.groups.len(), "group", "groups"),
                format_bytes(self.groups.iter().map(GroupView::reclaimable_bytes).sum()),
                self.marked_summary()
            )
        };
        let key_hints = match (&self.overlay, self.view) {
            (Some(Overlay::Help), _) => help::KEY_HINTS.to_owned(),
            (Some(Overlay::Removal(dialog)), _) => {
                removal::key_hints(dialog, self.disposal).to_owned()
            }
            (None, View::Skipped) => SKIPPED_KEY_HINTS.to_owned(),
            (None, View::Duplicates) => self.duplicates_key_hints(),
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
        match &mut self.overlay {
            Some(Overlay::Help) => help::render(self.disposal, frame, inner),
            Some(Overlay::Removal(dialog)) => removal::render(dialog, self.disposal, frame, inner),
            None => {}
        }
    }

    fn duplicates_key_hints(&self) -> String {
        // The arrows go without saying; ? lists every key, so the bar fits 80 columns.
        let actions = if self.groups.is_empty() {
            ""
        } else {
            "tab pane  space mark  k keep  d remove  "
        };
        let skipped = if self.skipped.is_empty() {
            String::new()
        } else {
            format!("s skipped ({})  ", format_count(self.skipped.len()))
        };
        format!(" {actions}{skipped}? help  q quit ")
    }

    fn render_duplicates(&mut self, frame: &mut Frame, area: Rect) {
        let [panes_area, notice_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
        if let Some(notice) = &self.notice {
            frame.render_widget(Paragraph::new(notice.as_str()), notice_area);
        }
        let [groups_area, copies_area] = Layout::horizontal([
            Constraint::Percentage(GROUPS_PANE_PERCENT),
            Constraint::Fill(1),
        ])
        .areas(panes_area);

        let group_row_width = usize::from(groups_area.width.saturating_sub(BORDER_CELLS))
            .saturating_sub(HIGHLIGHT_SYMBOL.width());
        let group_rows = self
            .groups
            .iter()
            .map(|group| truncate_end(&group.row(), group_row_width));
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
            .saturating_sub(
                HIGHLIGHT_SYMBOL.width() + MARKED_BOX.width() + COLUMN_GAP.width() + DATE_WIDTH,
            );
        let Some(group) = self.selected_group() else {
            return;
        };
        let title = group.title();
        let copy_rows: Vec<String> = group
            .copies
            .iter()
            .map(|copy| {
                let path = truncate_start(&copy.path, path_width);
                let padding = " ".repeat(path_width.saturating_sub(path.width()));
                let mark_box = if copy.marked {
                    MARKED_BOX
                } else {
                    UNMARKED_BOX
                };
                format!(
                    "{mark_box}{path}{padding}{COLUMN_GAP}{}",
                    copy.modified_date
                )
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
        let headline = if self.removed_bytes > 0 {
            "No duplicates left in"
        } else {
            "No duplicates found in"
        };
        // The root is cut at its start, like everywhere else, so the sentence keeps its end.
        let root_width = usize::from(area.width).saturating_sub(format!("{headline} .").width());
        let headline = format!(
            "{headline} {}.",
            truncate_start(&self.root_label, root_width)
        );
        let lines = if self.removed_bytes > 0 {
            let removed_bytes = format_bytes(self.removed_bytes);
            let outcome = match self.disposal {
                Disposal::Trash => format!("{removed_bytes} moved to the trash."),
                Disposal::Permanent => format!("{removed_bytes} freed."),
            };
            vec![Line::from(headline), Line::from(outcome)]
        } else {
            let scanned = format_quantity(self.scanned_files, "file", "files");
            let totals = if self.skipped.is_empty() {
                format!("{scanned} scanned.")
            } else {
                format!(
                    "{scanned} scanned, {} skipped.",
                    format_count(self.skipped.len())
                )
            };
            vec![Line::from(headline), Line::from(totals)]
        };
        frame.render_widget(Paragraph::new(lines).centered(), message_area);
    }

    /// How many copies are marked, and their total size.
    fn marked_totals(&self) -> (usize, u64) {
        self.groups
            .iter()
            .flat_map(|group| {
                group
                    .copies
                    .iter()
                    .filter(|copy| copy.marked)
                    .map(|_| group.file_size)
            })
            .fold((0, 0), |(copies, bytes), file_size| {
                (copies + 1, bytes + file_size)
            })
    }

    /// ` · marked 3 · 1.2 GiB`, or nothing while no copy is marked.
    fn marked_summary(&self) -> String {
        match self.marked_totals() {
            (0, _) => String::new(),
            (marked_copies, marked_bytes) => format!(
                " · marked {} · {}",
                format_count(marked_copies),
                format_bytes(marked_bytes)
            ),
        }
    }

    fn ask_to_remove_marked_copies(&mut self) {
        match self.marked_totals() {
            (0, _) => {
                self.notice = Some(
                    "Nothing is marked yet: space marks a copy, k keeps one and marks the rest."
                        .to_owned(),
                );
            }
            (copies, bytes) => {
                self.overlay = Some(Overlay::Removal(RemovalDialog::Confirming {
                    copies,
                    bytes,
                }));
            }
        }
    }

    /// The selected group and the index of its highlighted copy, whichever pane has the focus.
    fn highlighted_copy(&mut self) -> Option<(&mut GroupView, usize)> {
        let copy_index = self.copy_selection.selected()?;
        let group = self.groups.get_mut(self.group_selection.selected()?)?;
        (copy_index < group.copies.len()).then_some((group, copy_index))
    }

    fn toggle_highlighted_copy(&mut self) {
        let Some((group, copy_index)) = self.highlighted_copy() else {
            return;
        };
        let unmarked_copies = group.copies.iter().filter(|copy| !copy.marked).count();
        let copy = &mut group.copies[copy_index];

        if !copy.marked && unmarked_copies == 1 {
            self.notice = Some(format!(
                "Every group keeps one copy: unmark another copy of {} first.",
                group.name
            ));
        } else {
            copy.marked = !copy.marked;
        }
    }

    fn keep_only_highlighted_copy(&mut self) {
        let Some((group, kept_index)) = self.highlighted_copy() else {
            return;
        };
        for (index, copy) in group.copies.iter_mut().enumerate() {
            copy.marked = index != kept_index;
        }
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
