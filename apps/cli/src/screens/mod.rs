use unicode_width::UnicodeWidthStr;

use crate::format::truncate_start;

pub mod results;
pub mod scanning;

const APP_NAME: &str = "onlyfile";
/// The two corners of the outer border, which a title never covers.
const CORNER_CELLS: usize = 2;

/// ` onlyfile <root><details> `, with the root cut at its start when the screen is too narrow, so
/// the details after it always show.
pub fn screen_title(root_label: &str, details: &str, width: u16) -> String {
    let fixed_width = CORNER_CELLS + format!(" {APP_NAME} {details} ").width();
    let root = truncate_start(root_label, usize::from(width).saturating_sub(fixed_width));
    format!(" {APP_NAME} {root}{details} ")
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
