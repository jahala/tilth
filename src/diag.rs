//! The one sink for diagnostics.
//!
//! Results go to stdout: the CLI through its single writer, the MCP server
//! as JSON-RPC. Everything said about the run itself goes to stderr through
//! these functions, so stdout stays clean for the program reading it.
//!
//! Each message is written as given, followed by a newline. A message that
//! cannot be written is dropped: stderr is the last channel left, so there is
//! nowhere to report its failure, and a closed stderr must not stop the run.
//! `eprintln!` would panic there instead, and the release profile aborts on
//! panic.

use std::io::Write;

/// Report a failure. The caller decides whether the run ends.
pub fn error(message: &str) {
    emit(message);
}

/// Report something worth knowing that does not stop the run.
pub fn warn(message: &str) {
    emit(message);
}

/// Report progress or an outcome a person asked for, such as the file
/// `tilth install` changed.
pub fn info(message: &str) {
    emit(message);
}

fn emit(message: &str) {
    // Dropped on failure; see the module comment.
    let _ = writeln!(std::io::stderr().lock(), "{message}");
}
