//! Per-request wall-clock timeout wrapper for synchronous tool calls.
//!
//! Rust has no sync-world equivalent of Java's `Future.get(timeout, unit)`, so
//! we spawn a fresh thread per call, wait on a `crossbeam_channel` bounded(1)
//! with `select! { default(timeout) => ... }`, and detach the worker on timeout.
//!
//! Because Rust cannot forcibly cancel a running thread, a timed-out worker
//! keeps running until its `FnOnce` completes naturally. A bounded counter
//! (`ThreadTracker`) tracks how many detached workers are still in flight and
//! refuses new work at [`MAX_ABANDONED_THREADS`] to prevent unbounded thread
//! accumulation on pathologically slow operations.
//!
//! [`ThreadCoord`] is a `RUNNING` → (`TIMED_OUT` | `FINISHED`) CAS state
//! machine that guarantees the tracker is incremented/decremented exactly
//! once per spawn even when the worker's channel send races the main
//! thread's deadline.

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{bounded, select, RecvError};

const ABANDONED_THREAD_WARN: usize = 3;
/// Hard cap: refuse new work when this many prior threads are still running
/// after timeout. Prevents unbounded thread accumulation on stuck operations.
const MAX_ABANDONED_THREADS: usize = 8;

/// Live count of threads that timed out and are still running in the background.
/// Owned by `Services`, so tests instantiate their own instance rather than
/// serializing on a global.
pub(crate) struct ThreadTracker {
    count: AtomicUsize,
}

impl ThreadTracker {
    pub(crate) fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
        }
    }

    pub(crate) fn is_at_cap(&self) -> bool {
        self.count.load(Ordering::Relaxed) >= MAX_ABANDONED_THREADS
    }

    fn record_timeout(&self) -> usize {
        self.count.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn record_finish_after_timeout(&self) {
        self.count.fetch_sub(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn current(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }

    /// Pre-load the counter to the hard cap so callers can assert the
    /// `is_at_cap()` branch without launching real timeouts.
    #[cfg(test)]
    pub(crate) fn saturate(&self) {
        self.count.store(MAX_ABANDONED_THREADS, Ordering::Relaxed);
    }
}

/// Per-request coordination between the main thread and the worker thread.
/// Exactly one of `claim_timeout` / `claim_finish` wins, so the tracker count
/// is updated at most once per spawn even when a worker's send and the main
/// thread's `select!` deadline race.
struct ThreadCoord(AtomicU8);

impl ThreadCoord {
    const RUNNING: u8 = 0;
    const TIMED_OUT: u8 = 1;
    const FINISHED: u8 = 2;

    fn new() -> Self {
        Self(AtomicU8::new(Self::RUNNING))
    }

    /// Main-thread side. Returns true if we transitioned `RUNNING` → `TIMED_OUT`;
    /// the caller should then increment the tracker. False means the worker
    /// already reached `FINISHED` — no counter change needed.
    fn claim_timeout(&self) -> bool {
        self.0
            .compare_exchange(
                Self::RUNNING,
                Self::TIMED_OUT,
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    /// Worker-thread side. Returns true if we transitioned `RUNNING` → `FINISHED`;
    /// no counter change needed. False means the main thread already flipped
    /// to `TIMED_OUT` and incremented — the caller must decrement to undo it.
    fn claim_finish(&self) -> bool {
        self.0
            .compare_exchange(
                Self::RUNNING,
                Self::FINISHED,
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum SpawnFailure {
    Timeout,
    Panic,
}

/// Per-request timeout for tool calls. If a tool doesn't respond within this
/// duration, the MCP server returns a timeout error instead of hanging.
/// Override with `TILTH_TIMEOUT` env var (seconds). Default: 90s.
pub(crate) fn request_timeout() -> Duration {
    let secs = std::env::var("TILTH_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(90);
    Duration::from_secs(secs)
}

/// Run an arbitrary closure on a fresh thread with a wall-clock timeout.
/// Returns `Ok(result)` on success. On timeout, returns `Err(SpawnFailure::Timeout)`
/// and detaches the worker; the tracker is incremented and the worker will
/// decrement it when it eventually exits. On worker panic, returns `Err(Panic)`.
pub(crate) fn spawn_with_timeout<F, R>(
    tracker: &Arc<ThreadTracker>,
    timeout: Duration,
    work: F,
) -> Result<R, SpawnFailure>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let (tx, rx) = bounded::<R>(1);
    let coord = Arc::new(ThreadCoord::new());
    let coord_worker = Arc::clone(&coord);
    let tracker_worker = Arc::clone(tracker);

    let handle = std::thread::spawn(move || {
        // catch_unwind ensures claim_finish / record_finish_after_timeout run
        // even if work() panics after the main thread has already timed out.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
        if let Ok(val) = result {
            let _ = tx.send(val);
        }
        // tx is dropped here on panic, so main thread gets RecvError.
        if !coord_worker.claim_finish() {
            tracker_worker.record_finish_after_timeout();
        }
    });

    select! {
        recv(rx) -> msg => match msg {
            Ok(result) => {
                let _ = handle.join();
                Ok(result)
            }
            Err(RecvError) => Err(SpawnFailure::Panic),
        },
        default(timeout) => {
            // Increment before the CAS so the worker can never observe TIMED_OUT
            // and call fetch_sub before the fetch_add, which would underflow.
            // If the worker already claimed FINISHED, roll back the increment.
            let n = tracker.record_timeout();
            if coord.claim_timeout() {
                if n >= ABANDONED_THREAD_WARN {
                    eprintln!(
                        "tilth: warning: {n} abandoned threads still running. \
                         Consider reducing scope or increasing TILTH_TIMEOUT."
                    );
                }
            } else {
                tracker.record_finish_after_timeout();
            }
            Err(SpawnFailure::Timeout)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes env-var tests so parallel `cargo test` execution doesn't
    /// race on process-global `TILTH_TIMEOUT`. Any test that mutates this
    /// env var must take this lock for its duration.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Drives the real CAS path: a short-timeout `spawn_with_timeout` call
    /// races against a worker that sleeps past the deadline. The main thread
    /// must win the CAS (increment), and the worker must observe the lost CAS
    /// when it eventually exits (decrement). Ends with the counter back at zero.
    #[test]
    fn abandoned_counter_roundtrips_through_cas() {
        let tracker = Arc::new(ThreadTracker::new());
        assert_eq!(tracker.current(), 0);

        let result: Result<(), SpawnFailure> =
            spawn_with_timeout(&tracker, Duration::from_millis(20), || {
                std::thread::sleep(Duration::from_millis(200));
            });

        assert_eq!(result, Err(SpawnFailure::Timeout));
        assert_eq!(tracker.current(), 1, "timeout must increment tracker");

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while tracker.current() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(tracker.current(), 0, "worker exit must decrement tracker");
    }

    #[test]
    fn fast_work_returns_ok_without_counter_change() {
        let tracker = Arc::new(ThreadTracker::new());
        let result = spawn_with_timeout(&tracker, Duration::from_secs(5), || 42_i32);
        assert_eq!(result.expect("fast work should not timeout"), 42);
        assert_eq!(tracker.current(), 0);
    }

    #[test]
    fn request_timeout_reads_env() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("TILTH_TIMEOUT", "7");
        assert_eq!(request_timeout(), Duration::from_secs(7));
        std::env::remove_var("TILTH_TIMEOUT");
        assert_eq!(request_timeout(), Duration::from_secs(90));
    }
}
