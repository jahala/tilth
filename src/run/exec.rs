use std::path::Path;

use crate::error::TilthError;

pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
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

    let mut child = std::process::Command::new("sh")
        .args(["-c", command])
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| TilthError::IoError {
            path: cwd.to_path_buf(),
            source: e,
        })?;

    // Read stdout/stderr in background threads to avoid pipe deadlock.
    // Bounded reads prevent OOM from runaway commands.
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stdout_handle = std::thread::spawn(move || read_bounded(stdout));
    let stderr_handle = std::thread::spawn(move || read_bounded(stderr));

    // Wait for exit on a dedicated thread so we don't block the MCP I/O loop.
    let child_pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait());
    });

    let (status, _timed_out) = match rx.recv_timeout(std::time::Duration::from_secs(timeout_secs)) {
        Ok(Ok(status)) => (status, false),
        Ok(Err(e)) => {
            return Err(TilthError::IoError {
                path: cwd.to_path_buf(),
                source: e,
            });
        }
        Err(_) => {
            // Kill the child process. The wait thread will unblock and clean up.
            let _ = std::process::Command::new("kill")
                .args(["-9", &child_pid.to_string()])
                .status();
            // Collect whatever output we have so far.
            let stdout_str = stdout_handle
                .join()
                .ok()
                .and_then(std::result::Result::ok)
                .unwrap_or_default();
            let stderr_str = stderr_handle
                .join()
                .ok()
                .and_then(std::result::Result::ok)
                .unwrap_or_default();
            return Ok(ExecResult {
                exit_code: -1,
                stdout: stdout_str,
                stderr: stderr_str,
                timed_out: true,
            });
        }
    };

    let stdout_str = stdout_handle
        .join()
        .map_err(|_| TilthError::InvalidQuery {
            query: command.to_string(),
            reason: "stdout reader thread panicked".to_string(),
        })?
        .map_err(|e| TilthError::IoError {
            path: cwd.to_path_buf(),
            source: e,
        })?;
    let stderr_str = stderr_handle
        .join()
        .map_err(|_| TilthError::InvalidQuery {
            query: command.to_string(),
            reason: "stderr reader thread panicked".to_string(),
        })?
        .map_err(|e| TilthError::IoError {
            path: cwd.to_path_buf(),
            source: e,
        })?;

    Ok(ExecResult {
        exit_code: status.code().unwrap_or(-1),
        stdout: stdout_str,
        stderr: stderr_str,
        timed_out: false,
    })
}

/// Read up to `MAX_OUTPUT_BYTES` from a reader into a String.
fn read_bounded(reader: impl std::io::Read) -> std::io::Result<String> {
    use std::io::Read;
    let mut buf = String::new();
    reader.take(MAX_OUTPUT_BYTES).read_to_string(&mut buf)?;
    Ok(buf)
}
