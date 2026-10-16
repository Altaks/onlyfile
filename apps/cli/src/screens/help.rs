//! Every key of the results screen, and the rules behind marking and removing.

use purge::Disposal;
use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Block, Clear, Paragraph, Wrap},
};

use crate::screens::text_dialog_area;

pub const KEY_HINTS: &str = " ? or esc close  q quit ";
const KEY_COLUMN_WIDTH: usize = 9;
const KEYS: [(&str, &str); 12] = [
    ("↑ ↓", "move in the focused list"),
    ("tab", "switch between the groups and the copies"),
    ("a", "mark all but the oldest copy of each group"),
    ("u", "unmark every copy"),
    ("space", "mark or unmark the highlighted copy"),
    ("k", "keep the highlighted copy, mark the rest"),
    ("d", "remove the marked copies, after asking"),
    ("s", "list the entries the scan skipped"),
    ("enter", "close the removal summary"),
    ("?", "show or hide this help"),
    ("q, esc", "quit"),
    ("ctrl-c", "quit, or stop a running removal"),
];

pub fn render(disposal: Disposal, frame: &mut Frame, area: Rect) {
    let disposal_rule = match disposal {
        Disposal::Trash => "Removed copies go to the trash, where they can be restored.",
        Disposal::Permanent => {
            "This run was started with --permanent: removed copies are deleted for good."
        }
    };
    let mut lines: Vec<String> = KEYS
        .iter()
        .map(|(key, action)| format!("{key:<KEY_COLUMN_WIDTH$}{action}"))
        .collect();
    lines.extend([
        String::new(),
        "Every group keeps at least one copy.".to_owned(),
        "a skips the groups you already marked by hand.".to_owned(),
        // Short enough that the whole help fits an 80 by 24 terminal.
        "Each marked copy is checked again before removal; one that is no longer a copy stays."
            .to_owned(),
        disposal_rule.to_owned(),
    ]);

    let help = Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>())
        .wrap(Wrap { trim: false })
        .block(Block::bordered().title(" Help "));
    let dialog_area = text_dialog_area(area, &help);
    frame.render_widget(Clear, dialog_area);
    frame.render_widget(help, dialog_area);
}
