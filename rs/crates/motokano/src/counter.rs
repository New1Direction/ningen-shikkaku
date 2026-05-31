//! The exit-condition counter.
//!
//! [`CallCounter`] decides when the self-immolating server should exit. It is
//! driven from two places — after each tool response is sent, and on client
//! disconnect — and is safe under concurrent calls: it fires (returns `true`)
//! at most once across all of them.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// How the server decides to exit.
#[derive(Debug, Clone, Copy)]
pub enum CounterMode {
    /// Exit after exactly `N` tool calls complete.
    Calls(usize),
    /// Exit when the client disconnects.
    Session,
    /// Exit after `N` calls *or* on disconnect, whichever comes first.
    Either(usize),
}

/// Tracks the exit condition. Fires at most once.
pub struct CallCounter {
    remaining: AtomicUsize,
    mode: CounterMode,
    fired: AtomicBool,
}

impl CallCounter {
    /// Build a counter for the given mode.
    pub fn new(mode: CounterMode) -> Self {
        let remaining = match mode {
            CounterMode::Calls(n) | CounterMode::Either(n) => n,
            CounterMode::Session => 0,
        };
        CallCounter {
            remaining: AtomicUsize::new(remaining),
            mode,
            fired: AtomicBool::new(false),
        }
    }

    /// Whether a call-count limit applies in this mode.
    fn counts_calls(&self) -> bool {
        matches!(self.mode, CounterMode::Calls(_) | CounterMode::Either(_))
    }

    /// Called after each tool response is sent. Returns `true` if the server
    /// should now exit. Fires at most once.
    ///
    /// Uses `compare_exchange` (never `fetch_sub`) so a flood of concurrent
    /// calls can neither underflow the counter nor fire more than once.
    pub fn decrement(&self) -> bool {
        if !self.counts_calls() {
            return false;
        }
        loop {
            let current = self.remaining.load(Ordering::SeqCst);
            if current == 0 {
                return false; // already exhausted
            }
            match self.remaining.compare_exchange(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    // Only the single thread that drove the counter to zero
                    // reaches here with `current - 1 == 0`; the shared `fired`
                    // flag then guarantees a single `true` (also coordinating
                    // with `on_disconnect` in `Either` mode).
                    if current - 1 == 0 {
                        return !self.fired.swap(true, Ordering::SeqCst);
                    }
                    return false;
                }
                Err(_) => continue, // contended; re-read and retry
            }
        }
    }

    /// Called on client disconnect. Returns `true` if the server should now
    /// exit. Fires at most once.
    pub fn on_disconnect(&self) -> bool {
        match self.mode {
            CounterMode::Session | CounterMode::Either(_) => {
                !self.fired.swap(true, Ordering::SeqCst)
            }
            CounterMode::Calls(_) => false,
        }
    }
}
