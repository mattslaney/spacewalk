use std::{
    env,
    io::{self, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Copy the exact path as text, without display escaping or a trailing newline.
pub fn copy_path(path: &Path) -> Result<(), String> {
    let text = path
        .to_str()
        .ok_or("The selected path is not valid Unicode")?;
    copy_text(text, &mut commands(), Duration::from_secs(2))
}

fn commands() -> Vec<Command> {
    if cfg!(target_os = "macos") {
        let mut command = Command::new("pbcopy");
        command.env("LC_CTYPE", "en_US.UTF-8");
        return vec![command];
    }
    if cfg!(target_os = "windows") {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::InputEncoding = [System.Text.UTF8Encoding]::new(); Set-Clipboard -Value ([Console]::In.ReadToEnd())",
        ]);
        return vec![command];
    }
    let mut commands = Vec::new();
    if env::var_os("WAYLAND_DISPLAY").is_some() {
        let mut command = Command::new("wl-copy");
        command.args(["--type", "text/plain;charset=utf-8"]);
        commands.push(command);
    }
    if env::var_os("DISPLAY").is_some() {
        let mut command = Command::new("xclip");
        command.args(["-selection", "clipboard", "-in"]);
        commands.push(command);
        let mut command = Command::new("xsel");
        command.args(["--clipboard", "--input"]);
        commands.push(command);
    }
    commands
}

fn copy_text(text: &str, commands: &mut [Command], timeout: Duration) -> Result<(), String> {
    if commands.is_empty() {
        return Err("No desktop clipboard session is available".into());
    }
    let mut errors = Vec::new();
    for command in commands {
        match write_clipboard(command, text, timeout) {
            Ok(()) => return Ok(()),
            Err(error) => errors.push(format!(
                "{}: {error}",
                command.get_program().to_string_lossy()
            )),
        }
    }
    Err(errors.join("; "))
}

fn write_clipboard(command: &mut Command, text: &str, timeout: Duration) -> io::Result<()> {
    // Clipboard owners such as wl-copy and xclip fork into the background.
    // Do not capture their output pipes: those can stay open after success.
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| {
        let mut stdin = child.stdin.take().expect("piped clipboard input");
        stdin.write_all(text.as_bytes())?;
        drop(stdin);
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other(format!("exited with {status}")))
                };
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "clipboard helper timed out",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;

    fn capture(path: &Path) -> Command {
        let mut command = Command::new("sh");
        command
            .args(["-c", "cat > \"$1\"", "clipboard-test"])
            .arg(path);
        command
    }

    #[test]
    fn copies_exact_text_without_interpreting_shell_characters() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("clipboard");
        let text = "/tmp/spaces and café/$(literal) 'quoted'\nfile.txt";
        copy_text(text, &mut [capture(&output)], Duration::from_secs(1)).unwrap();
        assert_eq!(fs::read_to_string(output).unwrap(), text);
    }

    #[test]
    fn missing_and_failing_helpers_fall_back_to_the_next_provider() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("clipboard");
        let mut failing = Command::new("sh");
        failing.args(["-c", "exit 1"]);
        let mut commands = [
            Command::new(dir.path().join("missing-helper")),
            failing,
            capture(&output),
        ];
        copy_text("/selected/path", &mut commands, Duration::from_secs(1)).unwrap();
        assert_eq!(fs::read_to_string(output).unwrap(), "/selected/path");
    }

    #[test]
    fn unavailable_helpers_and_stalled_helpers_report_errors() {
        assert!(copy_text("path", &mut [], Duration::ZERO).is_err());
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 10"]);
        let error = write_clipboard(&mut command, "path", Duration::from_millis(20)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn non_unicode_paths_are_rejected_instead_of_copying_a_different_path() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let path = Path::new(OsStr::from_bytes(b"/tmp/\xff"));
        assert!(copy_path(path).unwrap_err().contains("not valid Unicode"));
    }
}
