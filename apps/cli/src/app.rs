//! Owns the current screen and turns terminal and worker events into screen changes.

use std::{
    io,
    sync::mpsc::{Receiver, TryRecvError},
    time::Duration,
};

use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
};

use crate::{
    pipeline::{PipelineError, PipelineEvent, PipelineProgress},
    removal::{self, RemovalEvent},
    screens::{KeyHandling, results::ResultsScreen, scanning},
    settings::RunSettings,
};

/// While a worker runs, how long the interface waits for a key before redrawing with its latest
/// progress.
const FRAME_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub enum AppExit {
    Quit,
    ScanFailed(PipelineError),
    /// The removal worker went silent before its result; the log lists what it already removed.
    RemovalInterrupted,
}

#[derive(Debug)]
enum Screen {
    Scanning(PipelineProgress),
    Results(Box<ResultsScreen>),
}

#[derive(Debug)]
pub struct App {
    settings: RunSettings,
    screen: Screen,
    removal_events: Option<Receiver<RemovalEvent>>,
    exit: Option<AppExit>,
}

impl App {
    pub fn new(settings: RunSettings) -> Self {
        Self {
            settings,
            screen: Screen::Scanning(PipelineProgress::Walking { files_found: 0 }),
            removal_events: None,
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
            self.drain_removal_events();
            if let Some(exit) = self.exit.take() {
                return Ok(exit);
            }
            terminal.draw(|frame| self.render(frame))?;
            // Only a worker changes the screen on its own. Without one, nothing changes until a
            // key, so waiting for one keeps an idle results screen from redrawing 20 times a second.
            let worker_running =
                matches!(self.screen, Screen::Scanning(_)) || self.removal_events.is_some();
            if !worker_running || event::poll(FRAME_INTERVAL)? {
                self.on_terminal_event(event::read()?);
            }
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        match &mut self.screen {
            Screen::Scanning(progress) => {
                scanning::render(progress, &self.settings.root_label, frame);
            }
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
                self.screen =
                    Screen::Results(Box::new(ResultsScreen::new(outcome, &self.settings)));
            }
            PipelineEvent::Finished(Err(error)) => self.exit = Some(AppExit::ScanFailed(error)),
        }
    }

    fn drain_removal_events(&mut self) {
        let Some(events) = self.removal_events.take() else {
            return;
        };
        let Screen::Results(results) = &mut self.screen else {
            return;
        };
        loop {
            match events.try_recv() {
                Ok(RemovalEvent::Progress { handled_copies }) => {
                    results.on_removal_progress(handled_copies);
                }
                Ok(RemovalEvent::Finished(outcomes)) => {
                    results.on_removal_finished(&outcomes);
                    return;
                }
                Err(TryRecvError::Empty) => break,
                // Like the scan, the removal always says `Finished` before it goes.
                Err(TryRecvError::Disconnected) => {
                    self.exit = Some(AppExit::RemovalInterrupted);
                    return;
                }
            }
        }
        self.removal_events = Some(events);
    }

    fn on_terminal_event(&mut self, event: Event) {
        // Anything but a key press, a resize included, only needs the redraw every frame gets.
        let Event::Key(key) = event else {
            return;
        };
        if key.kind != KeyEventKind::Press {
            return;
        }
        // Ctrl-C always quits, even in the middle of a removal: each copy is checked and removed
        // as one step, so stopping between two copies leaves nothing half done.
        if is_interrupt(key) {
            self.exit = Some(AppExit::Quit);
            return;
        }

        let handling = match &mut self.screen {
            Screen::Results(results) => results.on_key(key),
            Screen::Scanning(_) => KeyHandling::Ignored,
        };
        match handling {
            KeyHandling::Ignored if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) => {
                self.exit = Some(AppExit::Quit);
            }
            KeyHandling::Remove(removals) => {
                self.removal_events = Some(removal::spawn(removals, self.settings.disposal));
            }
            KeyHandling::Ignored | KeyHandling::Consumed => {}
        }
    }
}

fn is_interrupt(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
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

    use std::path::PathBuf;

    use jiff::tz::TimeZone;
    use purge::Disposal;
    use tempfile::TempDir;

    use super::*;
    use crate::pipeline::ScanOutcome;

    const HOME: &str = "/home/someone";
    const ROOT_LABEL: &str = "~/Photos";
    const LOG_LABEL: &str = "~/logs/onlyfile.log";
    /// 2024-01-12T03:00:00Z
    const MODIFIED_SECONDS: u64 = 1_705_028_400;

    fn settings() -> RunSettings {
        RunSettings {
            root_label: ROOT_LABEL.to_owned(),
            time_zone: TimeZone::UTC,
            home: PathBuf::from(HOME),
            disposal: Disposal::Trash,
            log_label: LOG_LABEL.to_owned(),
        }
    }

    fn app() -> App {
        App::new(settings())
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
        let mut app = App::new(RunSettings {
            root_label: "/private/tmp/a/very/long/folder/name/that/does/not/fit/Photos".to_owned(),
            ..settings()
        });
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
            second_copy.contains(&format!("> [ ] {}", shown("dump/big.mov"))),
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

        row_containing(
            &rows,
            " ↑↓ move  tab switch pane  space mark  k keep this  d remove marked  s skipped (3)  q quit ",
        );
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

    fn copy_row<'rows>(rows: &'rows [String], relative_path: &str) -> &'rows str {
        row_containing(rows, &shown(relative_path))
    }

    #[test]
    fn copies_start_unmarked_and_the_key_bar_offers_marking() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        let rows = rendered_rows(&mut app, 110, 12);

        for path in ["2023/big.mov", "dump/big.mov", "old/big (1).mov"] {
            let row = copy_row(&rows, path);
            assert!(row.contains(&format!("[ ] {}", shown(path))), "{row}");
        }
        row_containing(
            &rows,
            " ↑↓ move  tab switch pane  space mark  k keep this  d remove marked  q quit ",
        );
        assert!(!rows[0].contains("marked"), "{}", rows[0]);
    }

    #[test]
    fn space_marks_the_highlighted_copy_and_counts_it_in_the_header() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        let rows = rendered_rows(&mut app, 110, 12);

        assert!(copy_row(&rows, "dump/big.mov").contains("[x]"));
        assert!(copy_row(&rows, "2023/big.mov").contains("[ ]"));
        assert!(rows[0].contains(" · marked 1 · 4.0 KiB "), "{}", rows[0]);
    }

    #[test]
    fn space_on_a_marked_copy_unmarks_it() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char(' '));
        let rows = rendered_rows(&mut app, 110, 12);

        assert!(copy_row(&rows, "2023/big.mov").contains("[ ]"));
        assert!(!rows[0].contains("marked"), "{}", rows[0]);
    }

    #[test]
    fn the_last_unmarked_copy_of_a_group_cannot_be_marked() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        let rows = rendered_rows(&mut app, 110, 12);

        assert!(copy_row(&rows, "old/big (1).mov").contains("[ ]"));
        assert!(rows[0].contains(" · marked 2 · 8.0 KiB "), "{}", rows[0]);
        row_containing(
            &rows,
            "Every group keeps one copy: unmark another copy of big.mov first.",
        );
    }

    #[test]
    fn the_refusal_disappears_at_the_next_key() {
        let mut app = app();
        app.on_pipeline_event(two_groups());
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char(' '));

        press(&mut app, KeyCode::Down);
        let rows = rendered_rows(&mut app, 110, 12);

        assert!(
            rows.iter().all(|row| !row.contains("Every group keeps")),
            "{}",
            rows.join("\n")
        );
    }

    #[test]
    fn k_keeps_the_highlighted_copy_and_marks_every_other() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('k'));
        let rows = rendered_rows(&mut app, 110, 12);

        assert!(copy_row(&rows, "2023/big.mov").contains("[x]"));
        assert!(copy_row(&rows, "dump/big.mov").contains("[ ]"));
        assert!(copy_row(&rows, "old/big (1).mov").contains("[x]"));
        assert!(rows[0].contains(" · marked 2 · 8.0 KiB "), "{}", rows[0]);
    }

    #[test]
    fn marking_from_the_groups_pane_acts_on_the_highlighted_copy() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('k'));
        let rows = rendered_rows(&mut app, 110, 12);

        assert!(copy_row(&rows, "small.txt").contains("[ ]"));
        assert!(copy_row(&rows, "old/small.txt").contains("[x]"));
        assert!(rows[0].contains(" · marked 1 · 100 B "), "{}", rows[0]);
    }

    #[test]
    fn marks_survive_moving_between_groups() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Up);
        let rows = rendered_rows(&mut app, 110, 12);

        assert!(copy_row(&rows, "2023/big.mov").contains("[ ]"));
        assert!(copy_row(&rows, "dump/big.mov").contains("[x]"));
        assert!(rows[0].contains(" · marked 3 · 8.1 KiB "), "{}", rows[0]);
    }

    #[test]
    fn marking_keys_do_nothing_in_the_skipped_list() {
        let mut app = app();
        app.on_pipeline_event(two_groups_and_three_skips());

        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Esc);
        let rows = rendered_rows(&mut app, 110, 12);

        assert!(!rows[0].contains("marked"), "{}", rows[0]);
    }

    const PHOTO: &[u8] = b"the same bytes in every copy";
    const NOTES: &[u8] = b"notes";

    /// Two groups of real files under a temporary home, so a removal acts on the disk:
    /// `a/photo.jpg`, `b/photo.jpg`, `c/photo.jpg`, then `x/notes.txt`, `y/notes.txt`.
    fn real_duplicates() -> (TempDir, App) {
        let home = TempDir::new().expect("a temporary folder");
        let mut groups = Vec::new();
        for (content, paths) in [
            (
                PHOTO,
                ["a/photo.jpg", "b/photo.jpg", "c/photo.jpg"].as_slice(),
            ),
            (NOTES, ["x/notes.txt", "y/notes.txt"].as_slice()),
        ] {
            let copies = paths
                .iter()
                .map(|relative_path| {
                    let path = home
                        .path()
                        .join(relative_path.replace('/', MAIN_SEPARATOR_STR));
                    std::fs::create_dir_all(path.parent().expect("a parent folder"))
                        .expect("the temporary folder is writable");
                    std::fs::write(&path, content).expect("the temporary folder is writable");
                    ScannerFileEntry {
                        modified: std::fs::metadata(&path)
                            .and_then(|metadata| metadata.modified())
                            .expect("just written"),
                        path,
                        file_size: content.len() as u64,
                    }
                })
                .collect();
            groups.push(copies);
        }

        let mut app = App::new(RunSettings {
            home: home.path().to_path_buf(),
            disposal: Disposal::Permanent,
            ..settings()
        });
        app.on_pipeline_event(finished(5, groups));
        (home, app)
    }

    fn real_shown(relative_path: &str) -> String {
        format!(
            "~{MAIN_SEPARATOR}{}",
            relative_path.replace('/', MAIN_SEPARATOR_STR)
        )
    }

    fn real_path(home: &TempDir, relative_path: &str) -> PathBuf {
        home.path()
            .join(relative_path.replace('/', MAIN_SEPARATOR_STR))
    }

    /// Polls until the removal worker has delivered its result.
    fn wait_for_removal(app: &mut App) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while app.removal_events.is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "the removal never finished"
            );
            app.drain_removal_events();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn d_with_nothing_marked_says_how_to_mark() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        press(&mut app, KeyCode::Char('d'));

        row_containing(
            &rendered_rows(&mut app, 110, 12),
            "Nothing is marked yet: space marks a copy, k keeps one and marks the rest.",
        );
    }

    #[test]
    fn d_asks_before_moving_marked_copies_to_the_trash() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('d'));
        let rows = rendered_rows(&mut app, 110, 14);

        row_containing(&rows, "Remove marked copies");
        row_containing(&rows, "Move 2 copies (8.0 KiB) to the trash?");
        row_containing(
            &rows,
            "Each one is first compared byte for byte with the copy its group keeps.",
        );
        row_containing(&rows, " y move to the trash  n cancel ");
    }

    #[test]
    fn a_narrow_confirmation_wraps_instead_of_cutting_its_text() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('d'));
        let rows = rendered_rows(&mut app, 60, 16);

        row_containing(&rows, "Move 2 copies (8.0 KiB) to the trash?");
        row_containing(&rows, "group keeps.");
    }

    #[test]
    fn with_permanent_d_asks_before_deleting_and_says_it_cannot_be_undone() {
        let (_home, mut app) = real_duplicates();

        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('d'));
        let rows = rendered_rows(&mut app, 110, 14);

        row_containing(
            &rows,
            "Delete 2 copies (56 B) permanently? This cannot be undone.",
        );
        row_containing(&rows, " y delete  n cancel ");
    }

    #[test]
    fn n_or_esc_cancel_and_keep_the_marks() {
        for cancel in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut app = app();
            app.on_pipeline_event(two_groups());

            press(&mut app, KeyCode::Char('k'));
            press(&mut app, KeyCode::Char('d'));
            press(&mut app, cancel);
            let rows = rendered_rows(&mut app, 110, 14);

            assert!(app.exit.is_none(), "{cancel:?}");
            assert!(app.removal_events.is_none(), "{cancel:?}");
            assert!(
                rows.iter().all(|row| !row.contains("Remove marked copies")),
                "{cancel:?}"
            );
            assert!(rows[0].contains(" · marked 2 · 8.0 KiB "), "{}", rows[0]);
        }
    }

    #[test]
    fn other_keys_do_nothing_while_the_confirmation_is_open() {
        let mut app = app();
        app.on_pipeline_event(two_groups());

        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('q'));
        press(&mut app, KeyCode::Char(' '));

        assert!(app.exit.is_none());
        row_containing(
            &rendered_rows(&mut app, 110, 14),
            "Move 2 copies (8.0 KiB) to the trash?",
        );
    }

    #[test]
    fn y_removes_the_marked_copies_and_summarises() -> Result<(), Box<dyn std::error::Error>> {
        let (home, mut app) = real_duplicates();

        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        wait_for_removal(&mut app);
        let rows = rendered_rows(&mut app, 110, 16);

        row_containing(&rows, "Removal summary");
        row_containing(&rows, "Deleted 2 copies permanently, freeing 56 B.");
        row_containing(&rows, "Every removal is recorded in:");
        row_containing(&rows, "  ~/logs/onlyfile.log");
        row_containing(&rows, " ↑↓ scroll  enter continue  q quit ");
        assert!(
            rows.iter().all(|row| !row.contains("Left in place")),
            "{}",
            rows.join("\n")
        );
        assert_eq!(std::fs::read(real_path(&home, "a/photo.jpg"))?, PHOTO);
        assert!(!real_path(&home, "b/photo.jpg").exists());
        assert!(!real_path(&home, "c/photo.jpg").exists());
        Ok(())
    }

    #[test]
    fn after_the_summary_the_emptied_group_is_gone() {
        let (_home, mut app) = real_duplicates();

        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        wait_for_removal(&mut app);
        press(&mut app, KeyCode::Enter);
        let rows = rendered_rows(&mut app, 110, 14);

        assert!(
            rows[0].contains("· 1 group · 5 B reclaimable "),
            "{}",
            rows[0]
        );
        row_containing(&rows, "notes.txt · 5 B · 2 copies");
        assert!(
            rows.iter().all(|row| !row.contains("photo.jpg")),
            "{}",
            rows.join("\n")
        );
    }

    #[test]
    fn a_copy_changed_after_marking_is_left_in_place_and_unmarked()
    -> Result<(), Box<dyn std::error::Error>> {
        let (home, mut app) = real_duplicates();
        press(&mut app, KeyCode::Char('k'));
        std::fs::write(
            real_path(&home, "b/photo.jpg"),
            b"the same bytes in every cOpy",
        )?;

        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        wait_for_removal(&mut app);
        let rows = rendered_rows(&mut app, 110, 16);

        row_containing(&rows, "Deleted 1 copy permanently, freeing 28 B.");
        row_containing(&rows, "Left in place (1):");
        row_containing(
            &rows,
            &format!(
                "  no longer identical to the kept copy  {}",
                real_shown("b/photo.jpg")
            ),
        );
        assert!(real_path(&home, "b/photo.jpg").exists());

        press(&mut app, KeyCode::Enter);
        let rows = rendered_rows(&mut app, 110, 14);
        row_containing(&rows, "photo.jpg · 28 B · 2 copies");
        let left_in_place = row_containing(&rows, &real_shown("b/photo.jpg"));
        assert!(left_in_place.contains("[ ]"), "{left_in_place}");
        Ok(())
    }

    #[test]
    fn removing_every_duplicate_says_how_much_was_freed() {
        let (_home, mut app) = real_duplicates();

        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        wait_for_removal(&mut app);
        press(&mut app, KeyCode::Enter);
        let rows = rendered_rows(&mut app, 110, 14);

        row_containing(&rows, "No duplicates left in ~/Photos.");
        row_containing(&rows, "61 B freed.");
    }

    #[test]
    fn a_long_root_is_cut_in_the_empty_state_too() {
        let mut app = App::new(RunSettings {
            root_label: "/private/tmp/a/very/long/folder/name/that/does/not/fit/Photos".to_owned(),
            ..settings()
        });
        app.on_pipeline_event(finished(3, Vec::new()));

        let rows = rendered_rows(&mut app, 50, 12);

        row_containing(&rows, "No duplicates found in …");
        row_containing(&rows, "/fit/Photos.");
    }

    #[test]
    fn keys_wait_while_copies_are_being_removed_but_ctrl_c_quits() {
        let (_home, mut app) = real_duplicates();

        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        press(&mut app, KeyCode::Char('q'));
        assert!(app.exit.is_none());
        row_containing(&rendered_rows(&mut app, 110, 14), " ctrl-c quit ");

        app.on_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(matches!(app.exit, Some(AppExit::Quit)));
    }

    #[test]
    fn a_removal_worker_gone_silent_is_reported() {
        let mut app = app();
        app.on_pipeline_event(two_groups());
        let (sender, receiver) = mpsc::sync_channel::<RemovalEvent>(1);
        drop(sender);
        app.removal_events = Some(receiver);

        app.drain_removal_events();

        assert!(matches!(app.exit, Some(AppExit::RemovalInterrupted)));
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
