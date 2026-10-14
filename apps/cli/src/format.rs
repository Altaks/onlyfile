//! Turns sizes, counts, dates and paths into the short strings the screens show.

use std::{
    path::{MAIN_SEPARATOR, Path},
    time::SystemTime,
};

use jiff::{Timestamp, tz::TimeZone};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const BYTES_PER_UNIT: f64 = 1024.0;
const BYTE_UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
/// A value printing as "1024" without decimals belongs to the next unit.
const ROUNDS_TO_NEXT_UNIT: f64 = BYTES_PER_UNIT - 0.5;
/// Below ten units a decimal carries information ("3.1 GiB"); above, it is noise ("412 MiB").
/// Values rounding to 10.0 print without one.
const SHOW_DECIMAL_BELOW: f64 = 9.95;

const DIGITS_PER_GROUP: usize = 3;
const THOUSANDS_SEPARATOR: char = ' ';

const ELLIPSIS: char = '…';
const ELLIPSIS_WIDTH: usize = 1;

/// Shown when the filesystem reports an instant outside the years -9999 to 9999.
const UNKNOWN_DATE: &str = "unknown date";
const HOME_PREFIX: &str = "~";

/// `512 B`, `3.1 GiB`, `412 MiB`.
pub fn format_bytes(bytes: u64) -> String {
    let mut value = bytes as f64;
    if value < BYTES_PER_UNIT {
        return format!("{bytes} B");
    }

    value /= BYTES_PER_UNIT;
    let mut unit_index = 0;
    while value >= ROUNDS_TO_NEXT_UNIT && unit_index < BYTE_UNITS.len() - 1 {
        value /= BYTES_PER_UNIT;
        unit_index += 1;
    }

    let unit = BYTE_UNITS[unit_index];
    if value < SHOW_DECIMAL_BELOW {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.0} {unit}")
    }
}

/// `12 345`.
pub fn format_count(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / DIGITS_PER_GROUP);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(DIGITS_PER_GROUP) {
            grouped.push(THOUSANDS_SEPARATOR);
        }
        grouped.push(digit);
    }
    grouped
}

/// `1 file`, `12 345 files`.
pub fn format_quantity(count: usize, singular: &str, plural: &str) -> String {
    let noun = if count == 1 { singular } else { plural };
    format!("{} {noun}", format_count(count))
}

/// The calendar date of `instant` in `time_zone`, as `YYYY-MM-DD`.
pub fn format_date(instant: SystemTime, time_zone: &TimeZone) -> String {
    Timestamp::try_from(instant)
        .map(|timestamp| timestamp.to_zoned(time_zone.clone()).date().to_string())
        .unwrap_or_else(|_| UNKNOWN_DATE.to_owned())
}

/// `path` with the home folder written as `~`.
pub fn format_path(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(relative) if relative.as_os_str().is_empty() => HOME_PREFIX.to_owned(),
        Ok(relative) => format!("{HOME_PREFIX}{MAIN_SEPARATOR}{}", relative.display()),
        Err(_) => path.display().to_string(),
    }
}

/// `text` cut at the start with an ellipsis so it fits in `max_width` terminal cells. The end of a
/// path is the part that tells copies apart, so it is the part kept.
pub fn truncate_start(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_owned();
    }
    if max_width < ELLIPSIS_WIDTH {
        return String::new();
    }

    let mut kept_width = 0;
    let mut kept_characters = Vec::new();
    for character in text.chars().rev() {
        let character_width = character.width().unwrap_or(0);
        if kept_width + character_width + ELLIPSIS_WIDTH > max_width {
            break;
        }
        kept_width += character_width;
        kept_characters.push(character);
    }

    std::iter::once(ELLIPSIS)
        .chain(kept_characters.into_iter().rev())
        .collect()
}

/// `text` cut at the end with an ellipsis so it fits in `max_width` terminal cells.
pub fn truncate_end(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_owned();
    }
    if max_width < ELLIPSIS_WIDTH {
        return String::new();
    }

    let mut kept_width = 0;
    let mut kept = String::new();
    for character in text.chars() {
        let character_width = character.width().unwrap_or(0);
        if kept_width + character_width + ELLIPSIS_WIDTH > max_width {
            break;
        }
        kept_width += character_width;
        kept.push(character);
    }
    kept.push(ELLIPSIS);
    kept
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use jiff::tz::Offset;

    use super::*;

    #[test]
    fn bytes_below_one_kibibyte_print_as_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
    }

    #[test]
    fn bytes_below_ten_units_keep_one_decimal() {
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(3_328_599_654), "3.1 GiB");
    }

    #[test]
    fn bytes_from_ten_units_print_without_decimal() {
        assert_eq!(format_bytes(432_013_312), "412 MiB");
        assert_eq!(format_bytes(10 * 1024), "10 KiB");
    }

    #[test]
    fn a_value_rounding_to_ten_drops_its_decimal() {
        assert_eq!(format_bytes(10_200), "10 KiB");
    }

    #[test]
    fn a_value_rounding_to_1024_moves_to_the_next_unit() {
        assert_eq!(format_bytes(1024 * 1024 - 1), "1.0 MiB");
    }

    #[test]
    fn the_largest_unit_absorbs_anything_bigger() {
        assert_eq!(format_bytes(u64::MAX), "16384 PiB");
    }

    #[test]
    fn counts_group_thousands_with_a_space() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1 000");
        assert_eq!(format_count(12_345), "12 345");
        assert_eq!(format_count(1_234_567), "1 234 567");
    }

    #[test]
    fn quantities_agree_with_their_count() {
        assert_eq!(format_quantity(1, "file", "files"), "1 file");
        assert_eq!(format_quantity(0, "file", "files"), "0 files");
        assert_eq!(format_quantity(12_345, "file", "files"), "12 345 files");
    }

    #[test]
    fn dates_are_the_calendar_day_in_the_given_zone() {
        // 2024-01-12T03:00:00Z
        let instant = SystemTime::UNIX_EPOCH + Duration::from_secs(1_705_028_400);

        assert_eq!(format_date(instant, &TimeZone::UTC), "2024-01-12");
        assert_eq!(
            format_date(instant, &TimeZone::fixed(Offset::constant(-5))),
            "2024-01-11"
        );
    }

    #[test]
    fn paths_under_home_start_with_a_tilde() {
        let home = PathBuf::from("/home/someone");

        assert_eq!(
            format_path(&home.join("Photos").join("a.png"), &home),
            format!("~{MAIN_SEPARATOR}Photos{MAIN_SEPARATOR}a.png")
        );
        assert_eq!(format_path(&home, &home), "~");
    }

    #[test]
    fn paths_outside_home_are_unchanged() {
        let home = PathBuf::from("/home/someone");
        let elsewhere = PathBuf::from("/var/data/a.png");

        assert_eq!(
            format_path(&elsewhere, &home),
            elsewhere.display().to_string()
        );
    }

    #[test]
    fn text_that_fits_is_unchanged() {
        assert_eq!(truncate_start("photos/a.png", 12), "photos/a.png");
    }

    #[test]
    fn text_too_wide_keeps_its_end_behind_an_ellipsis() {
        assert_eq!(truncate_start("photos/a.png", 6), "…a.png");
    }

    #[test]
    fn wide_characters_count_for_two_cells() {
        // Each of these characters takes two terminal cells.
        assert_eq!(truncate_start("写真/写真.png", 9), "…写真.png");
    }

    #[test]
    fn no_room_at_all_gives_an_empty_string() {
        assert_eq!(truncate_start("photos/a.png", 0), "");
    }

    #[test]
    fn text_too_wide_keeps_its_start_before_an_ellipsis() {
        assert_eq!(truncate_end("BankSize_Android.json", 10), "BankSize_…");
        assert_eq!(
            truncate_end("BankSize_Android.json", 21),
            "BankSize_Android.json"
        );
        assert_eq!(truncate_end("写真.png", 4), "写…");
        assert_eq!(truncate_end("photos", 0), "");
    }
}
