//! Per-request wall-clock timeout wrapper for synchronous tool calls.
//!
//! Rust has no sync-world equivalent of Java's `Future.get(timeout, unit)`, so
//! we spawn a fresh thread per call, wait on a `crossbeam_channel` bounded(1)
//! with `select! { default(timeout) => ... }`, and detach the worker on timeout.
//!
//! Because Rust cannot forcibly cancel a running thread, a timed-out worker
//! keeps running until its `FnOnce` completes naturally. A bounded counter
//! ([`ThreadTracker`]) tracks how many detached workers are still in flight and
//! refuses new work at [`MAX_ABANDONED_THREADS`] to prevent unbounded thread
//! accumulation on pathologically slow operations.
//!
//! [`ThreadCoord`] is a `RUNNING` → (`TIMED_OUT` | `FINISHED`) CAS state
//! machine that guarantees the tracker is incremented and decremented exactly
//! once per spawn even when the worker's channel send races the main
//! thread's deadline.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{bounded, select, RecvError};

/// Warn-once threshold: print a single stderr line the first time the
/// abandoned-thread count crosses this value. The deadline arm checks
/// `if n == ABANDONED_THREAD_WARN { ... }` — deliberately `==`, not `>=`, so
/// the operator gets one signal at the threshold rather than a flood as each
/// subsequent timeout piles on. The hard cap at [`MAX_ABANDONED_THREADS`]
/// stops accepting work entirely, so silence past this point is bounded.
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

    /// `Acquire` here pairs with the `AcqRel` RMWs in `record_timeout` and
    /// `record_finish_after_timeout`. `Relaxed` would suffice for the counter
    /// value alone (atomic RMWs are atomic at any ordering); we pay the cheap
    /// upgrade so the pairing is canonical and the next reader doesn't have
    /// to re-derive that the value is consistent with the rest of the spawn
    /// state machine.
    pub(crate) fn is_at_cap(&self) -> bool {
        self.count.load(Ordering::Acquire) >= MAX_ABANDONED_THREADS
    }

    /// `AcqRel` is the canonical pessimistic ordering for an RMW that's read
    /// from another thread: documents intent better than `Release` alone, and
    /// guarantees the RMW sees any prior `Release` write to this atomic before
    /// computing its new value. The cost is negligible on x86 and one extra
    /// barrier on weakly-ordered architectures.
    fn record_timeout(&self) -> usize {
        self.count.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn record_finish_after_timeout(&self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
    }

    #[cfg(test)]
    pub(crate) fn current(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    /// Pre-load the counter to the hard cap so callers can assert the
    /// `is_at_cap()` branch without launching real timeouts.
    #[cfg(test)]
    pub(crate) fn saturate(&self) {
        self.count.store(MAX_ABANDONED_THREADS, Ordering::Release);
    }
}

/// Per-request coordination between the main thread and the worker thread.
/// Exactly one of `claim_timeout` / `claim_finish` wins, so the tracker count
/// is updated at most once per spawn even when a worker's send and the main
/// thread's `select!` deadline race.
struct ThreadCoord {
    state: AtomicU8,
    /// Set to `true` after the main thread has incremented the tracker. The
    /// worker thread, if it lost the CAS to `TIMED_OUT`, must wait for this
    /// flag before decrementing — otherwise it could race ahead of the
    /// increment and underflow the counter.
    timeout_acked: AtomicBool,
}

impl ThreadCoord {
    const RUNNING: u8 = 0;
    const TIMED_OUT: u8 = 1;
    const FINISHED: u8 = 2;

    fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::RUNNING),
            timeout_acked: AtomicBool::new(false),
        }
    }

    /// Main-thread side. Returns true if we transitioned `RUNNING` → `TIMED_OUT`;
    /// the caller must then increment the tracker and call `ack_timeout`.
    /// False means the worker already reached `FINISHED` — no counter change.
    fn claim_timeout(&self) -> bool {
        self.state
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
    /// to `TIMED_OUT` and will increment — the caller must wait for
    /// `timeout_acked` and then decrement to undo it.
    fn claim_finish(&self) -> bool {
        self.state
            .compare_exchange(
                Self::RUNNING,
                Self::FINISHED,
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    fn ack_timeout(&self) {
        self.timeout_acked.store(true, Ordering::Release);
    }

    /// Spin until the main thread signals the tracker increment is visible.
    /// The main thread runs only a single counter update between `claim_timeout`
    /// and `ack_timeout`, so this loop terminates promptly in practice.
    ///
    /// `spin_loop()` is a CPU pipeline hint, not a scheduler yield — on a
    /// single-CPU container where the worker was scheduled before the main
    /// thread, a pure spin would burn the worker's full quantum (~10ms) before
    /// the main thread gets a turn to set the flag. The trailing `yield_now()`
    /// surrenders the rest of the quantum so the ack becomes visible in tens
    /// of microseconds instead.
    fn wait_for_timeout_ack(&self) {
        while !self.timeout_acked.load(Ordering::Acquire) {
            std::hint::spin_loop();
            std::thread::yield_now();
        }
    }
}

/// Reasons a `spawn_with_timeout` call did not return a value. Marked
/// `#[non_exhaustive]` so a future failure mode (e.g. OS-level thread spawn
/// failure) can be added without churning every call site.
#[derive(Debug, PartialEq)]
#[non_exhaustive]
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
            // Main thread already claimed the timeout. It will increment the
            // tracker before signalling `timeout_acked`; wait for that signal
            // before decrementing so we cannot underflow the counter.
            coord_worker.wait_for_timeout_ack();
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
            // Claim the timeout before touching the tracker so a concurrent
            // `is_at_cap()` cannot observe an inflated count that we then roll
            // back. If the worker already won the CAS, we leave the tracker
            // alone entirely.
            if coord.claim_timeout() {
                let n = tracker.record_timeout();
                coord.ack_timeout();
                if n == ABANDONED_THREAD_WARN {
                    eprintln!(
                        "tilth: warning: {n} abandoned threads still running. \
                         Consider reducing scope or increasing TILTH_TIMEOUT."
                    );
                }
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

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
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
    fn worker_panic_surfaces_as_panic_failure() {
        let tracker = Arc::new(ThreadTracker::new());
        let result: Result<(), SpawnFailure> =
            spawn_with_timeout(&tracker, Duration::from_secs(5), || {
                panic!("boom");
            });
        assert_eq!(result, Err(SpawnFailure::Panic));
        assert_eq!(tracker.current(), 0, "panic must not leak a tracker slot");
    }

    #[test]
    fn saturated_tracker_reports_at_cap() {
        let tracker = Arc::new(ThreadTracker::new());
        assert!(!tracker.is_at_cap());
        tracker.saturate();
        assert!(tracker.is_at_cap());
    }

    #[test]
    fn request_timeout_reads_env() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("TILTH_TIMEOUT", "7");
        assert_eq!(request_timeout(), Duration::from_secs(7));
        std::env::remove_var("TILTH_TIMEOUT");
        assert_eq!(request_timeout(), Duration::from_secs(90));
    }
}
