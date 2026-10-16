//! Drives the built binary for what happens before the interface takes the terminal.

use std::{
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use tempfile::{NamedTempFile, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_onlyfile");
const FAILURE_EXIT_CODE: i32 = 1;
const USAGE_EXIT_CODE: i32 = 2;
/// A safety net: every case here exits at once, but an interface waiting for keys never would.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

fn wait_for_exit(child: &mut Child) -> std::io::Result<ExitStatus> {
    let deadline = Instant::now() + EXIT_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() > deadline {
            child.kill()?;
            panic!("onlyfile did not exit within {EXIT_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

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
fn help_documents_the_path_argument_and_the_permanent_flag()
-> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new(BINARY).arg("--help").output()?;

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("Usage: onlyfile [OPTIONS] [PATH]"),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "      --permanent  Delete marked copies permanently instead of moving them to the trash\n"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("[PATH]  Folder to scan. Defaults to the current folder\n"),
        "{stdout}"
    );
    Ok(())
}

#[test]
fn piped_output_exits_with_code_1_and_says_to_use_a_terminal()
-> Result<(), Box<dyn std::error::Error>> {
    let folder = TempDir::new()?;
    let mut child = Command::new(BINARY)
        .arg(folder.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let status = wait_for_exit(&mut child)?;
    let output = child.wait_with_output()?;

    assert_eq!(status.code(), Some(FAILURE_EXIT_CODE));
    assert_eq!(
        String::from_utf8(output.stderr)?,
        "onlyfile: its output is not a terminal. onlyfile is interactive: run it directly in a \
         terminal, without piping or redirecting its output.\n"
    );
    assert!(
        output.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    Ok(())
}
