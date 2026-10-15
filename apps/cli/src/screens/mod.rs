use purge::GroupRemoval;
use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Modifier, Style},
    widgets::{Block, List, ListState, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::format::truncate_start;

pub mod help;
pub mod removal;
pub mod results;
pub mod scanning;
pub mod skipped;

const APP_NAME: &str = "onlyfile";
/// The two corners of the outer border, which a title never covers.
const CORNER_CELLS: usize = 2;
pub const HIGHLIGHT_SYMBOL: &str = "> ";
/// One cell for the left border, one for the right.
pub const BORDER_CELLS: u16 = 2;
const DIALOG_WIDTH_PERCENT: u16 = 70;

/// Whether a screen used a key, left it to the application (to quit, for instance), or asks the
/// application to start removing copies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyHandling {
    Consumed,
    Ignored,
    Remove(Vec<GroupRemoval>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Previous,
    Next,
}

/// Moves `selection` one row, staying within `row_count` rows. Returns whether it moved.
pub fn step_selection(selection: &mut ListState, row_count: usize, step: Step) -> bool {
    let Some(current) = selection.selected() else {
        return false;
    };
    let next = match step {
        Step::Previous => current.saturating_sub(1),
        Step::Next => (current + 1).min(row_count.saturating_sub(1)),
    };
    selection.select(Some(next));
    next != current
}

/// A bordered list whose border and selection stand out only while it has the focus.
pub fn pane_list(
    rows: impl IntoIterator<Item = String>,
    title: String,
    focused: bool,
) -> List<'static> {
    let (border_style, highlight_style) = if focused {
        (
            Style::new().add_modifier(Modifier::BOLD),
            Style::new().add_modifier(Modifier::REVERSED),
        )
    } else {
        (Style::new(), Style::new())
    };

    List::new(rows)
        .block(Block::bordered().title(title).border_style(border_style))
        .highlight_symbol(HIGHLIGHT_SYMBOL)
        .highlight_style(highlight_style)
}

/// ` onlyfile <root><details> `, with the root cut at its start when the screen is too narrow, so
/// the details after it always show.
pub fn screen_title(root_label: &str, details: &str, width: u16) -> String {
    let fixed_width = CORNER_CELLS + format!(" {APP_NAME} {details} ").width();
    let root = truncate_start(root_label, usize::from(width).saturating_sub(fixed_width));
    format!(" {APP_NAME} {root}{details} ")
}

/// `done / total` for a gauge: nothing to do counts as done, and overshooting stops at full.
pub fn done_share(done: u64, total: u64) -> f64 {
    if total == 0 {
        1.0
    } else {
        (done as f64 / total as f64).min(1.0)
    }
}

/// A box of the given size in the middle of `area`.
pub fn centered(area: Rect, width: Constraint, height: Constraint) -> Rect {
    let [row] = Layout::vertical([height]).flex(Flex::Center).areas(area);
    let [dialog] = Layout::horizontal([width]).flex(Flex::Center).areas(row);
    dialog
}

/// Where a bordered, wrapped paragraph goes in the middle of `area`: a set share of the width, and
/// exactly as tall as the paragraph's own wrapping makes it at that width, borders included.
pub fn text_dialog_area(area: Rect, text: &Paragraph) -> Rect {
    let [row] = Layout::horizontal([Constraint::Percentage(DIALOG_WIDTH_PERCENT)])
        .flex(Flex::Center)
        .areas(area);
    let height = text.line_count(row.width.saturating_sub(BORDER_CELLS));
    centered(
        area,
        Constraint::Length(row.width),
        Constraint::Length(u16::try_from(height).unwrap_or(u16::MAX)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_that_fits_is_complete() {
        assert_eq!(
            screen_title("~/Photos", " · 2 groups", 40),
            " onlyfile ~/Photos · 2 groups "
        );
    }

    #[test]
    fn a_long_root_gives_way_to_the_details() {
        let title = screen_title("~/a/very/long/path/to/Photos", " · 2 groups", 32);

        assert_eq!(title, " onlyfile …/Photos · 2 groups ");
        assert_eq!(title.width() + CORNER_CELLS, 32);
    }
}
