#[cfg(not(unix))]
compile_error!(
    "The exec module requires Unix (process groups, killpg). Windows is not yet supported."
);

use std::path::Path;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::error::TilthError;

pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl ExecResult {
    /// Combine stdout and stderr into a single string.
    /// If only one stream has output, return it directly (no duplication).
    /// If both have output, stderr is appended after stdout.
    #[must_use]
    pub fn combined_output(self) -> String {
        match (self.stdout.is_empty(), self.stderr.is_empty()) {
            (_, true) => self.stdout,
            (true, _) => self.stderr,
            _ => {
                if self.stdout.ends_with('\n') {
                    format!("{}{}", self.stdout, self.stderr)
                } else {
                    format!("{}\n{}", self.stdout, self.stderr)
                }
            }
        }
    }
}

const MAX_OUTPUT_BYTES: u64 = 10 * 1024 * 1024; // 10 MB
const DEFAULT_TIMEOUT_SECS: u64 = 120;

#[must_use]
pub fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

pub fn execute(command: &str, cwd: &Path, timeout_secs: u64) -> Result<ExecResult, TilthError> {
    if !cwd.is_dir() {
        return Err(TilthError::NotFound {
            path: cwd.to_path_buf(),
            suggestion: None,
        });
    }

    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", command])
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Make `sh` a process group leader (setpgid(0,0)) so that on timeout we can
    // kill the entire group (sh + grandchildren like cargo, npm, etc.).
    // On Windows, job objects would be the equivalent mechanism.
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|e| TilthError::IoError {
        path: cwd.to_path_buf(),
        source: e,
    })?;

    // Read stdout/stderr in background threads to avoid pipe deadlock.
    // Bounded reads prevent OOM from runaway commands.
    let stdout = child.stdout.take().expect("stdout was set to piped");
    let stderr = child.stderr.take().expect("stderr was set to piped");
    let stdout_handle = std::thread::spawn(move || read_bounded(stdout));
    let stderr_handle = std::thread::spawn(move || read_bounded(stderr));

    // Wait for exit on a dedicated thread so we don't block the MCP I/O loop.
    let child_pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait());
    });

    let status = match rx.recv_timeout(std::time::Duration::from_secs(timeout_secs)) {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => {
            return Err(TilthError::IoError {
                path: cwd.to_path_buf(),
                source: e,
            });
        }
        Err(_) => {
            // Kill the entire process group so grandchildren (cargo, npm, etc.)
            // are also terminated. `sh` becomes the group leader via setpgid(0,0).
            // On Windows a different mechanism (job objects) would be needed.
            let pgid = child_pid as libc::pid_t;
            debug_assert!(pgid > 0, "child PID must be positive");
            if pgid > 0 {
                unsafe {
                    libc::killpg(pgid, libc::SIGKILL);
                }
            } else {
                // PID 0 would kill our own process group — never do that.
                eprintln!("warning: refusing to killpg(0)");
            }

            // Join reader threads with a 5-second timeout. If they don't finish
            // (e.g. grandchild still has the pipe open), detach and let them leak —
            // they'll die once the pipes close after the process group is killed.
            let (tx_done, rx_done) = std::sync::mpsc::channel::<(String, String)>();
            std::thread::spawn(move || {
                let out = stdout_handle.join().unwrap_or_default();
                let err = stderr_handle.join().unwrap_or_default();
                let _ = tx_done.send((out, err));
            });
            let (stdout_str, stderr_str) = rx_done
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or_default();

            return Ok(ExecResult {
                exit_code: -1,
                stdout: stdout_str,
                stderr: stderr_str,
                timed_out: true,
            });
        }
    };

    let stdout_str = stdout_handle.join().map_err(|_| TilthError::InvalidQuery {
        query: command.to_string(),
        reason: "stdout reader thread panicked".to_string(),
    })?;
    let stderr_str = stderr_handle.join().map_err(|_| TilthError::InvalidQuery {
        query: command.to_string(),
        reason: "stderr reader thread panicked".to_string(),
    })?;

    Ok(ExecResult {
        exit_code: status.code().unwrap_or(-1),
        stdout: stdout_str,
        stderr: stderr_str,
        timed_out: false,
    })
}

/// Read up to `MAX_OUTPUT_BYTES` from a reader into a String.
/// Uses lossy UTF-8 conversion so non-UTF-8 bytes (binary output, locale issues)
/// don't cause failures.
fn read_bounded(reader: impl std::io::Read) -> String {
    use std::io::Read;
    let mut buf = Vec::new();
    // Ignore truncation errors — partial output is better than nothing.
    let _ = reader.take(MAX_OUTPUT_BYTES).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_bounded_handles_non_utf8() {
        // Embed invalid UTF-8 bytes (continuation byte without start byte).
        let invalid: &[u8] = b"hello \xff\xfe world";
        let result = read_bounded(std::io::Cursor::new(invalid));
        assert!(result.contains("hello"));
        assert!(result.contains("world"));
        // Replacement characters are present for the invalid bytes.
        assert!(result.contains('\u{FFFD}'));
    }

    #[test]
    fn read_bounded_valid_utf8_unchanged() {
        let input = "cargo build\nFinished in 1.2s\n";
        let result = read_bounded(std::io::Cursor::new(input.as_bytes()));
        assert_eq!(result, input);
    }

    #[test]
    fn combined_output_stdout_only() {
        let r = ExecResult {
            exit_code: 0,
            stdout: "hello".into(),
            stderr: String::new(),
            timed_out: false,
        };
        assert_eq!(r.combined_output(), "hello");
    }

    #[test]
    fn combined_output_stderr_only() {
        let r = ExecResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: "error".into(),
            timed_out: false,
        };
        assert_eq!(r.combined_output(), "error");
    }

    #[test]
    fn combined_output_both_streams() {
        // stdout already ends with newline — no extra separator inserted
        let r = ExecResult {
            exit_code: 0,
            stdout: "out\n".into(),
            stderr: "err\n".into(),
            timed_out: false,
        };
        assert_eq!(r.combined_output(), "out\nerr\n");
    }

    #[test]
    fn combined_output_both_streams_no_trailing_newline() {
        // stdout lacks trailing newline — separator is inserted
        let r = ExecResult {
            exit_code: 0,
            stdout: "out".into(),
            stderr: "err\n".into(),
            timed_out: false,
        };
        assert_eq!(r.combined_output(), "out\nerr\n");
    }

    #[test]
    fn combined_output_both_empty() {
        let r = ExecResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
        };
        assert_eq!(r.combined_output(), "");
    }

    #[test]
    fn execute_echo_hello() {
        let result =
            execute("echo hello", std::path::Path::new("/tmp"), 5).expect("execute should succeed");
        assert!(
            result.stdout.contains("hello"),
            "stdout should contain 'hello'"
        );
        assert_eq!(result.exit_code, 0);
        assert!(!result.timed_out);
        assert!(result.stderr.is_empty(), "stderr should be empty");
    }

    #[test]
    fn execute_timeout() {
        let result = execute("sleep 60", std::path::Path::new("/tmp"), 1)
            .expect("execute should return Ok even on timeout");
        assert!(result.timed_out, "should have timed out");
        assert_eq!(result.exit_code, -1);
    }

    #[test]
    fn execute_bad_cwd() {
        let result = execute("echo x", std::path::Path::new("/nonexistent_dir_xyz"), 5);
        assert!(result.is_err(), "bad cwd should return Err");
    }

    #[test]
    fn execute_nonzero_exit() {
        let result =
            execute("exit 42", std::path::Path::new("/tmp"), 5).expect("execute should succeed");
        assert_eq!(result.exit_code, 42);
        assert!(!result.timed_out);
    }

    #[test]
    fn execute_stderr_output() {
        let result = execute("echo err >&2", std::path::Path::new("/tmp"), 5)
            .expect("execute should succeed");
        assert!(result.stderr.contains("err"), "stderr should contain 'err'");
        assert!(result.stdout.is_empty(), "stdout should be empty");
    }
}
