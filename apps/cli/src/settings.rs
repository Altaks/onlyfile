//! What every screen needs to know about this run, fixed at startup.

use std::path::PathBuf;

use jiff::tz::TimeZone;
use purge::Disposal;

#[derive(Debug, Clone)]
pub struct RunSettings {
    /// The scanned folder as the screens write it, home shown as `~`.
    pub root_label: String,
    pub time_zone: TimeZone,
    pub home: PathBuf,
    pub disposal: Disposal,
    /// Where every removal is recorded, as the screens write it.
    pub log_label: String,
}
