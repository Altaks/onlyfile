//! Drives the built binary for what happens before the interface takes the terminal.

use std::process::Command;

use tempfile::{NamedTempFile, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_onlyfile");
const FAILURE_EXIT_CODE: i32 = 1;
const USAGE_EXIT_CODE: i32 = 2;

#[test]
fn a_missing_folder_exits_with_code_1_and_names_it() -> Result<(), Box<dyn std::error::Error>> {
    let parent = TempDir::new()?;
    let missing = parent.path().join("missing");

    let output = Command::new(BINARY).arg(&missing).output()?;

    assert_eq!(output.status.code(), Some(FAILURE_EXIT_CODE));
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.starts_with(&format!("onlyfile: cannot read {}: ", missing.display())),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn a_file_instead_of_a_folder_exits_with_code_1() -> Result<(), Box<dyn std::error::Error>> {
    let file = NamedTempFile::new()?;

    let output = Command::new(BINARY).arg(file.path()).output()?;

    assert_eq!(output.status.code(), Some(FAILURE_EXIT_CODE));
    assert_eq!(
        String::from_utf8(output.stderr)?,
        format!("onlyfile: {} is not a folder\n", file.path().display())
    );
    Ok(())
}

#[test]
fn an_unknown_flag_is_a_usage_error() -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new(BINARY).arg("--no-such-flag").output()?;

    assert_eq!(output.status.code(), Some(USAGE_EXIT_CODE));
    Ok(())
}

#[test]
fn help_documents_the_path_argument() -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new(BINARY).arg("--help").output()?;

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Usage: onlyfile [PATH]"), "{stdout}");
    assert!(
        stdout.contains("[PATH]  Folder to scan. Defaults to the current folder\n"),
        "{stdout}"
    );
    Ok(())
}
