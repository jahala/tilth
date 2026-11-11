//! The one sink for diagnostics.
//!
//! Results go to stdout: the CLI through its single writer, the MCP server
//! as JSON-RPC. Everything said about the run itself, from a failure to the
//! file `tilth install` changed, goes to stderr through [`emit`], so stdout
//! stays clean for the program reading it. `clippy.toml` forbids
//! `std::io::stderr` everywhere else.
//!
//! A message that cannot be written is dropped: stderr is the last channel
//! left, so there is nowhere to report its failure, and a closed stderr must
//! not stop the run. `eprintln!` would panic there instead, and the release
//! profile aborts on panic.

use std::fmt;
use std::io::Write;

/// Write `message` and a newline to stderr, as in
/// `diag::emit(format_args!("grok error: {e}"))`.
pub fn emit(message: fmt::Arguments<'_>) {
    #[expect(
        clippy::disallowed_methods,
        reason = "by design: this is the one stderr sink"
    )]
    let stderr = std::io::stderr();
    // Dropped on failure; see the module comment.
    let _ = writeln!(stderr.lock(), "{message}");
}
