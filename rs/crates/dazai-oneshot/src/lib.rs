#![deny(unsafe_code)]
//! dazai-oneshot — a self-immolating MCP server.
//!
//! Serves a configurable number of tool calls (or until the client disconnects,
//! or until a dazai daemon dies), then wipes any in-memory secret state and
//! exits. Adds no new mechanism: it reuses [`dazai_secmem::SecretBuffer`] for
//! static values and the dazai control protocol for daemon integration.
//!
//! All `unsafe` lives in `dazai-secmem`; this crate is `#![deny(unsafe_code)]`.

pub mod counter;
pub mod dazai;
pub mod server;
pub mod tools;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dazai::DazaiLink;
use tools::ToolRegistry;

/// Brief window allowing the final MCP response to flush to the client before
/// the wipe — rmcp exposes no post-send hook, so the exit path waits this long
/// to ensure the last response was delivered.
const RESPONSE_FLUSH: Duration = Duration::from_millis(150);

/// Everything the single wipe+exit path needs, shared (`Arc`) across the tool
/// handler, the dazai monitor thread, and the disconnect path in `main`.
pub struct ExitCtx {
    /// Secret-bearing tools to wipe on exit.
    pub tools: Arc<Mutex<ToolRegistry>>,
    /// Optional dazai daemon link (for best-effort UNREGISTER).
    pub dazai: Option<Arc<DazaiLink>>,
    /// Whether to `raise(SIGKILL)` (armed) instead of a clean `exit(0)`.
    pub arm: bool,
    /// Operator-requested wait before wiping after the final call.
    pub grace: Duration,
    /// Set the instant an exit condition fires. Once set, the server serves no
    /// further tool calls, so no secret is dispensed during the flush/grace
    /// window before the wipe runs.
    pub closed: Arc<AtomicBool>,
}

/// The single wipe+exit path. Fires at most once — whichever exit condition
/// triggers first wins — and never returns.
///
/// Steps: (1) best-effort dazai UNREGISTER, (2) a brief flush window then the
/// configured grace, (3) wipe all SecretBuffers, (4) exit — `raise(SIGKILL)`
/// when armed, else a clean `exit(0)`.
pub fn wipe_and_exit(ctx: &ExitCtx) -> ! {
    static EXITING: AtomicBool = AtomicBool::new(false);
    if EXITING.swap(true, Ordering::SeqCst) {
        // Another thread is already tearing the process down; park until it
        // ends us, so we never double-wipe or race two exits.
        loop {
            std::thread::park();
        }
    }

    // Stop serving immediately, so no secret is dispensed during the flush /
    // grace window (covers the monitor- and disconnect-initiated exits; the
    // call-counter path also sets this synchronously in `call_tool`).
    ctx.closed.store(true, Ordering::SeqCst);

    // 1. unregister from dazai (best-effort, non-blocking).
    if let Some(d) = &ctx.dazai {
        d.unregister();
    }
    // 2. flush window (deliver the final response), then the grace window.
    std::thread::sleep(RESPONSE_FLUSH);
    if ctx.grace > Duration::ZERO {
        std::thread::sleep(ctx.grace);
    }
    // 3. wipe all SecretBuffers (recover a poisoned lock so we still wipe).
    {
        let mut tools = ctx.tools.lock().unwrap_or_else(|p| p.into_inner());
        tools.wipe();
    }
    // 4. exit.
    if ctx.arm {
        dazai_secmem::raise_sigkill()
    } else {
        std::process::exit(0)
    }
}
