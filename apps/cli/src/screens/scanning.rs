//! The screen shown while the worker walks the tree and then compares candidates.

use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    widgets::{Block, Gauge, Paragraph},
};
use replica::ComparisonProgress;

use crate::{
    format::{format_bytes, format_count, format_quantity},
    pipeline::PipelineProgress,
    screens::{done_share, screen_title},
};

const KEY_HINTS: &str = " q cancel ";
const GAUGE_WIDTH_PERCENT: u16 = 60;

pub fn render(progress: &PipelineProgress, root_label: &str, frame: &mut Frame) {
    let outer = Block::bordered()
        .title(screen_title(root_label, "", frame.area().width))
        .title_bottom(KEY_HINTS);
    let inner = outer.inner(frame.area());
    frame.render_widget(outer, frame.area());

    let [_, status_area, gauge_row, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(inner);
    frame.render_widget(
        Paragraph::new(status_line(progress)).centered(),
        status_area,
    );

    if let Some(ratio) = completion_ratio(progress) {
        let [_, gauge_area, _] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Percentage(GAUGE_WIDTH_PERCENT),
            Constraint::Fill(1),
        ])
        .areas(gauge_row);
        frame.render_widget(Gauge::default().ratio(ratio), gauge_area);
    }
}

fn status_line(progress: &PipelineProgress) -> String {
    match *progress {
        PipelineProgress::Walking { files_found } => {
            format!(
                "Scanning · {} found",
                format_quantity(files_found, "file", "files")
            )
        }
        PipelineProgress::Comparing(ComparisonProgress::Sampling {
            sampled_files,
            total_files,
        }) => format!(
            "Comparing · {} / {} files sampled",
            format_count(sampled_files),
            format_count(total_files)
        ),
        PipelineProgress::Comparing(ComparisonProgress::Hashing {
            hashed_files,
            total_files,
            hashed_bytes,
            total_bytes,
        }) => format!(
            "Comparing · {} / {} files · {} / {} read",
            format_count(hashed_files),
            format_count(total_files),
            format_bytes(hashed_bytes),
            format_bytes(total_bytes)
        ),
    }
}

/// The share of the current phase already done, when the phase knows its total.
fn completion_ratio(progress: &PipelineProgress) -> Option<f64> {
    match *progress {
        PipelineProgress::Walking { .. } => None,
        PipelineProgress::Comparing(ComparisonProgress::Sampling {
            sampled_files,
            total_files,
        }) => Some(done_share(sampled_files as u64, total_files as u64)),
        PipelineProgress::Comparing(ComparisonProgress::Hashing {
            hashed_bytes,
            total_bytes,
            ..
        }) => Some(done_share(hashed_bytes, total_bytes)),
    }
}
