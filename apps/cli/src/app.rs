//! Owns the current screen and turns terminal and worker events into screen changes.

use std::{
    io,
    path::PathBuf,
    sync::mpsc::{Receiver, TryRecvError},
    time::Duration,
};

use jiff::tz::TimeZone;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
};

use crate::{
    pipeline::{PipelineError, PipelineEvent, PipelineProgress},
    screens::{KeyHandling, results::ResultsScreen, scanning},
};

/// While the scan runs, how long the interface waits for a key before redrawing with the worker's
/// latest progress.
const FRAME_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub enum AppExit {
    Quit,
    ScanFailed(PipelineError),
}

#[derive(Debug)]
enum Screen {
    Scanning(PipelineProgress),
    Results(ResultsScreen),
}

#[derive(Debug)]
pub struct App {
    root_label: String,
    time_zone: TimeZone,
    home: PathBuf,
    screen: Screen,
    exit: Option<AppExit>,
}

impl App {
    pub fn new(root_label: String, time_zone: TimeZone, home: PathBuf) -> Self {
        Self {
            root_label,
            time_zone,
            home,
            screen: Screen::Scanning(PipelineProgress::Walking { files_found: 0 }),
            exit: None,
        }
    }

    pub fn run(
        mut self,
        terminal: &mut DefaultTerminal,
        events: &Receiver<PipelineEvent>,
    ) -> io::Result<AppExit> {
        loop {
            self.drain_pipeline_events(events);
            if let Some(exit) = self.exit.take() {
                return Ok(exit);
            }
            terminal.draw(|frame| self.render(frame))?;
            // Only the scan changes the screen on its own. Once it is over, nothing changes until a
            // key, so waiting for one keeps an idle results screen from redrawing 20 times a second.
            let scan_in_progress = matches!(self.screen, Screen::Scanning(_));
            if !scan_in_progress || event::poll(FRAME_INTERVAL)? {
                self.on_terminal_event(event::read()?);
            }
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        match &mut self.screen {
            Screen::Scanning(progress) => scanning::render(progress, &self.root_label, frame),
            Screen::Results(results) => results.render(frame),
        }
    }

    fn drain_pipeline_events(&mut self, events: &Receiver<PipelineEvent>) {
        loop {
            match events.try_recv() {
                Ok(event) => self.on_pipeline_event(event),
                Err(TryRecvError::Empty) => return,
                // The worker always says `Finished` before it goes. Going silent while the scan is
                // still on screen means it died on the way.
                Err(TryRecvError::Disconnected) => {
                    if matches!(self.screen, Screen::Scanning(_)) && self.exit.is_none() {
                        self.exit = Some(AppExit::ScanFailed(PipelineError::WorkerStopped));
                    }
                    return;
                }
            }
        }
    }

    fn on_pipeline_event(&mut self, event: PipelineEvent) {
        match event {
            PipelineEvent::Progress(progress) => {
                if let Screen::Scanning(current) = &mut self.screen {
                    *current = progress;
                }
            }
            PipelineEvent::Finished(Ok(outcome)) => {
                self.screen = Screen::Results(ResultsScreen::new(
                    outcome,
                    self.root_label.clone(),
                    &self.time_zone,
                    &self.home,
                ));
            }
            PipelineEvent::Finished(Err(error)) => self.exit = Some(AppExit::ScanFailed(error)),
        }
    }

    fn on_terminal_event(&mut self, event: Event) {
        // Anything but a key press, a resize included, only needs the redraw every frame gets.
        let Event::Key(key) = event else {
            return;
        };
        if key.kind != KeyEventKind::Press {
            return;
        }

        let handling = match &mut self.screen {
            Screen::Results(results) => results.on_key(key),
            Screen::Scanning(_) => KeyHandling::Ignored,
        };
        if handling == KeyHandling::Ignored && is_quit_key(key) {
            self.exit = Some(AppExit::Quit);
        }
    }
}

fn is_quit_key(key: KeyEvent) -> bool {
    let is_ctrl_c = key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
    is_ctrl_c || matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
}

#[cfg(test)]
mod tests {
    use std::{
        path::{MAIN_SEPARATOR, MAIN_SEPARATOR_STR},
        sync::mpsc,
        time::SystemTime,
    };

    use model::{DuplicateGroup, ScannerFileEntry, SkipReason, SkippedEntry};
    use ratatui::{Terminal, backend::TestBackend};
    use replica::ComparisonProgress;

    use super::*;
    use crate::pipeline::ScanOutcome;

    const HOME: &str = "/home/someone";
    const ROOT_LABEL: &str = "~/Photos";
    /// 2024-01-12T03:00:00Z
    const MODIFIED_SECONDS: u64 = 1_705_028_400;

    fn app() -> App {
        App::new(ROOT_LABEL.to_owned(), TimeZone::UTC, PathBuf::from(HOME))
    }

    /// `relative_path` uses `/`; each segment is joined so the path is native on every platform.
    fn photo_path(relative_path: &str) -> PathBuf {
        relative_path
            .split('/')
            .fold(PathBuf::from(HOME).join("Photos"), |path, segment| {
                path.join(segment)
            })
    }

    /// How the screen writes [`photo_path`]: home as `~`, the platform's separator.
    fn shown(relative_path: &str) -> String {
        format!(
            "~{MAIN_SEPARATOR}Photos{MAIN_SEPARATOR}{}",
            relative_path.replace('/', MAIN_SEPARATOR_STR)
        )
    }

    fn copy(relative_path: &str, file_size: u64) -> ScannerFileEntry {
        ScannerFileEntry {
            path: photo_path(relative_path),
            file_size,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(MODIFIED_SECONDS),
        }
    }

    fn finished(scanned_files: usize, groups: Vec<Vec<ScannerFileEntry>>) -> PipelineEvent {
        finished_with_skips(scanned_files, groups, Vec::new())
    }

    fn finished_with_skips(
        scanned_files: usize,
        groups: Vec<Vec<ScannerFileEntry>>,
        skipped: Vec<SkippedEntry>,
    ) -> PipelineEvent {
        PipelineEvent::Finished(Ok(ScanOutcome {
            scanned_files,
            groups: groups
                .into_iter()
                .map(|copies| DuplicateGroup::new(copies).expect("at least two copies"))
                .collect(),
            skipped,
        }))
    }

    fn skipped(relative_path: &str, reason: SkipReason) -> SkippedEntry {
        SkippedEntry {
            path: photo_path(relative_path),
            reason,
        }
    }

    /// Listed out of path order, so the screen proves the sorting.
    fn three_skips() -> Vec<SkippedEntry> {
        vec![
            skipped(
                "locked",
                SkipReason::Unreadable(io::ErrorKind::PermissionDenied),
            ),
            skipped("empty.txt", SkipReason::EmptyFile),
            skipped("link", SkipReason::Symlink),
        ]
    }

    fn two_groups_and_three_skips() -> PipelineEvent {
        let PipelineEvent::Finished(Ok(mut outcome)) = two_groups() else {
            unreachable!("two_groups always succeeds");
        };
        outcome.skipped = three_skips();
        PipelineEvent::Finished(Ok(outcome))
    }

    /// A small group listed after a larger one, so the order on screen proves the sorting.
    fn two_groups() -> PipelineEvent {
        finished(
            9,
            vec![
                vec![copy("small.txt", 100), copy("old/small.txt", 100)],
                vec![
                    copy("2023/big.mov", 4096),
                    copy("dump/big.mov", 4096),
                    copy("old/big (1).mov", 4096),
                ],
            ],
        )
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_terminal_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn rendered_rows(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let mut terminal =
            Terminal::new(TestBackend::new(width, height)).expect("a test backend never fails");
        terminal
            .draw(|frame| app.render(frame))
            .expect("a test backend never fails");
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|row| {
                (0..width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect()
            })
            .collect()
    }

    fn row_containing<'rows>(rows: &'rows [String], text: &str) -> &'rows str {
        rows.iter()
            .find(|row| row.contains(text))
            .unwrap_or_else(|| panic!("no row contains {text:?} in:\n{}", rows.join("\n")))
    }

    #[test]
    fn scanning_shows_the_running_count_of_files_found() {
        let mut app = app();
        app.on_pipeline_event(PipelineEvent::Progress(PipelineProgress::Walking {
            files_found: 12_345,
        }));

        let rows = rendered_rows(&mut app, 80, 12);

        row_containing(&rows, "onlyfile ~/Photos");
        row_containing(&rows, "Scanning · 12 345 files found");
        row_containing(&rows, "q cancel");
    }

    #[test]
    fn sampling_shows_files_sampled_out_of_the_total() {
        let mut app = app();
        app.on_pipeline_event(PipelineEvent::Progress(PipelineProgress::Comparing(
            ComparisonProgress::Sampling {
                sampled_files: 3,
                total_files: 8,
            },
        )));

        row_containing(
            &rendered_rows(&mut app, 80, 12),
            "Comparing · 3 / 8 files sampled",
        );
    }

    #[test]
    fn hashing_shows_files_and_bytes_read_with_a_gauge() {
        let mut app = app();
        app.on_pipeline_event(PipelineEvent::Progress(PipelineProgress::Comparing(
            ComparisonProgress::Hashing {
                hashed_files: 1,
                total_files: 2,
                hashed_bytes: 17,
                total_bytes: 34,
            },
        )));

        let rows = rendered_rows(&mut app, 80, 12);

        row_containing(&rows, "Comparing · 1 / 2 files · 17 B / 34 B read");
        row_containing(&rows, "50%");
    }

    #[test]
    fn results_list_the_group_reclaiming_most_space_first() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        let rows = rendered_rows(&mut app, 100, 12);

        row_containing(&rows, "onlyfile ~/Photos · 2 groups · 8.1 KiB reclaimable");
        let first_group = row_containing(&rows, "4.0 KiB ×3 big.mov");
        assert!(first_group.contains("> "), "{first_group}");
        row_containing(&rows, "100 B ×2 small.txt");
    }

    #[test]
    fn a_long_root_is_cut_so_the_totals_stay_visible() {
        let mut app = App::new(
            "/private/tmp/a/very/long/folder/name/that/does/not/fit/Photos".to_owned(),
            TimeZone::UTC,
            PathBuf::from(HOME),
        );
        app.on_pipeline_event(two_groups());

        let header = rendered_rows(&mut app, 70, 12).remove(0);

        assert!(header.contains('…'), "{header}");
        assert!(
            header.ends_with("/fit/Photos · 2 groups · 8.1 KiB reclaimable ┐"),
            "{header}"
        );
    }

    #[test]
    fn the_copies_pane_shows_every_copy_of_the_selected_group_with_its_date() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        let rows = rendered_rows(&mut app, 100, 12);

        row_containing(&rows, "big.mov · 4.0 KiB · 3 copies");
        for path in ["2023/big.mov", "dump/big.mov", "old/big (1).mov"] {
            let copy_row = row_containing(&rows, &shown(path));
            assert!(copy_row.contains("2024-01-12"), "{copy_row}");
        }
    }

    #[test]
    fn down_selects_the_next_group_and_shows_its_copies() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        press(&mut app, KeyCode::Down);
        let rows = rendered_rows(&mut app, 100, 12);

        row_containing(&rows, "small.txt · 100 B · 2 copies");
        row_containing(&rows, &shown("old/small.txt"));
    }

    #[test]
    fn down_stops_at_the_last_group() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Up);
        let rows = rendered_rows(&mut app, 100, 12);

        row_containing(&rows, "big.mov · 4.0 KiB · 3 copies");
    }

    #[test]
    fn tab_moves_the_selection_keys_to_the_copies_pane() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        let rows = rendered_rows(&mut app, 100, 12);

        row_containing(&rows, "big.mov · 4.0 KiB · 3 copies");
        let second_copy = row_containing(&rows, &shown("dump/big.mov"));
        assert!(
            second_copy.contains(&format!("> {}", shown("dump/big.mov"))),
            "{second_copy}"
        );
    }

    #[test]
    fn a_path_too_long_for_its_pane_keeps_its_end() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        let rows = rendered_rows(&mut app, 60, 12);

        let copy_row = row_containing(&rows, "big (1).mov");
        assert!(copy_row.contains('…'), "{copy_row}");
        assert!(copy_row.contains("2024-01-12"), "{copy_row}");
    }

    #[test]
    fn a_group_name_too_long_for_its_pane_keeps_its_start() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        let rows = rendered_rows(&mut app, 60, 12);

        row_containing(&rows, ">  4.0 KiB ×3 big…│");
    }

    #[test]
    fn no_duplicates_says_so_with_the_number_of_files_scanned() {
        let mut app = app();
        app.on_pipeline_event(finished(3, Vec::new()));

        let rows = rendered_rows(&mut app, 80, 12);

        row_containing(&rows, "No duplicates found in ~/Photos.");
        row_containing(&rows, "3 files scanned.");
        row_containing(&rows, "q quit");
    }

    #[test]
    fn the_key_bar_offers_the_skipped_list_with_its_count() {
        let mut app = app();
        app.on_pipeline_event(two_groups_and_three_skips());

        let rows = rendered_rows(&mut app, 100, 12);

        row_containing(&rows, " ↑↓ move  tab switch pane  s skipped (3)  q quit ");
    }

    #[test]
    fn without_skipped_entries_the_key_bar_does_not_mention_them() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        let rows = rendered_rows(&mut app, 100, 12);

        assert!(
            rows.iter().all(|row| !row.contains("skipped")),
            "{}",
            rows.join("\n")
        );
    }

    #[test]
    fn s_lists_skipped_entries_by_path_with_their_reason() {
        let mut app = app();
        app.on_pipeline_event(two_groups_and_three_skips());

        press(&mut app, KeyCode::Char('s'));
        let rows = rendered_rows(&mut app, 100, 12);

        row_containing(&rows, "Skipped · 3 entries");
        let listed: Vec<&String> = rows.iter().filter(|row| row.contains(&shown(""))).collect();
        assert_eq!(listed.len(), 3, "{}", rows.join("\n"));
        assert!(
            listed[0].contains(&format!(
                "> empty file                     {}",
                shown("empty.txt")
            )),
            "{}",
            listed[0]
        );
        assert!(
            listed[1].contains(&format!(
                "  symlink                        {}",
                shown("link")
            )),
            "{}",
            listed[1]
        );
        assert!(
            listed[2].contains(&format!(
                "  unreadable: permission denied  {}",
                shown("locked")
            )),
            "{}",
            listed[2]
        );
        row_containing(&rows, " ↑↓ move  esc back  q quit ");
    }

    #[test]
    fn esc_leaves_the_skipped_list_instead_of_quitting() {
        let mut app = app();
        app.on_pipeline_event(two_groups_and_three_skips());

        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Esc);
        let rows = rendered_rows(&mut app, 100, 12);

        assert!(app.exit.is_none());
        row_containing(&rows, "Groups");
    }

    #[test]
    fn q_quits_from_the_skipped_list() {
        let mut app = app();
        app.on_pipeline_event(two_groups_and_three_skips());

        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('q'));

        assert!(matches!(app.exit, Some(AppExit::Quit)));
    }

    #[test]
    fn s_does_nothing_when_nothing_was_skipped() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        press(&mut app, KeyCode::Char('s'));
        let rows = rendered_rows(&mut app, 100, 12);

        row_containing(&rows, "Groups");
        assert!(app.exit.is_none());
    }

    #[test]
    fn no_duplicates_still_counts_and_lists_skipped_entries() {
        let mut app = app();
        app.on_pipeline_event(finished_with_skips(3, Vec::new(), three_skips()));

        let rows = rendered_rows(&mut app, 80, 12);
        row_containing(&rows, "3 files scanned, 3 skipped.");
        row_containing(&rows, " s skipped (3)  q quit ");

        press(&mut app, KeyCode::Char('s'));
        row_containing(&rendered_rows(&mut app, 80, 12), "Skipped · 3 entries");
    }

    #[test]
    fn q_esc_and_ctrl_c_quit_from_any_screen() {
        let quit_keys = [
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ];
        for key in quit_keys {
            let mut scanning = app();
            scanning.on_terminal_event(Event::Key(key));
            assert!(matches!(scanning.exit, Some(AppExit::Quit)), "{key:?}");

            let mut results = app();
            results.on_pipeline_event(two_groups());
            results.on_terminal_event(Event::Key(key));
            assert!(matches!(results.exit, Some(AppExit::Quit)), "{key:?}");
        }
    }

    #[test]
    fn a_plain_c_does_not_quit() {
        let mut app = app();
        press(&mut app, KeyCode::Char('c'));

        assert!(app.exit.is_none());
    }

    #[test]
    fn a_failed_scan_exits_with_its_error() {
        let mut app = app();
        app.on_pipeline_event(PipelineEvent::Finished(Err(PipelineError::WorkerStopped)));

        assert!(matches!(
            app.exit,
            Some(AppExit::ScanFailed(PipelineError::WorkerStopped))
        ));
    }

    #[test]
    fn a_worker_gone_silent_during_the_scan_is_a_failure() {
        let (sender, receiver) = mpsc::sync_channel::<PipelineEvent>(1);
        drop(sender);
        let mut app = app();

        app.drain_pipeline_events(&receiver);

        assert!(matches!(
            app.exit,
            Some(AppExit::ScanFailed(PipelineError::WorkerStopped))
        ));
    }

    #[test]
    fn a_worker_gone_after_its_result_is_normal() {
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(two_groups()).expect("the receiver is alive");
        drop(sender);
        let mut app = app();

        app.drain_pipeline_events(&receiver);

        assert!(app.exit.is_none());
        assert!(matches!(app.screen, Screen::Results(_)));
    }
}
