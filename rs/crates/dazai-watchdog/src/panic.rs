//! The panic decision policy, ported from the Phase 1 Python `PanicController`.
//!
//! All side effects are injected as closures so the policy can be exercised in
//! tests with recording fakes — in particular the lethal `kill` is a callable,
//! so the full decide/wipe/kill path runs without the test process dying.
//!
//! `armed` is an `Arc<AtomicBool>` rather than a plain `bool` so it can be
//! flipped at runtime by an `ARM` control message (Phase 3).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Decides what each trigger does, given the arming / dry-run / grace policy.
///
/// Collaborators (all injected):
/// - `kill_registered`: SIGKILL every registered PID. Runs first on an armed
///   trigger, *before* the buffer wipe, so supervised agents stop immediately.
/// - `wipe`: zeroize every secret buffer.
/// - `kill`: the lethal action for *this* process (kill child + `raise(SIGKILL)`
///   self). Only ever invoked when armed; in production it does not return.
/// - `dry_done`: invoked after a dry-run wipe so the host loop can wind down.
/// - `clock`: monotonic `Instant` source (real: [`Instant::now`]).
/// - `log`: human-facing log sink.
pub struct PanicController {
    armed: Arc<AtomicBool>,
    grace: Duration,
    fired: bool,
    deadline: Option<Instant>,
    kill_registered: Box<dyn FnMut()>,
    wipe: Box<dyn FnMut()>,
    kill: Box<dyn FnMut()>,
    dry_done: Box<dyn FnMut()>,
    clock: Box<dyn Fn() -> Instant>,
    log: Box<dyn Fn(&str)>,
}

impl PanicController {
    /// Construct a controller from its policy and injected collaborators.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        armed: Arc<AtomicBool>,
        grace: Duration,
        kill_registered: Box<dyn FnMut()>,
        wipe: Box<dyn FnMut()>,
        kill: Box<dyn FnMut()>,
        dry_done: Box<dyn FnMut()>,
        clock: Box<dyn Fn() -> Instant>,
        log: Box<dyn Fn(&str)>,
    ) -> Self {
        PanicController {
            armed,
            grace,
            fired: false,
            deadline: None,
            kill_registered,
            wipe,
            kill,
            dry_done,
            clock,
            log,
        }
    }

    /// Whether the controller is currently armed for a real self-destruct.
    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    /// Whether a trigger has already fired (the one-shot guard is set).
    pub fn has_fired(&self) -> bool {
        self.fired
    }

    /// The pending grace deadline, if an armed graceful panic is scheduled.
    pub fn pending_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn emit(&self, msg: &str) {
        (self.log)(msg);
    }

    fn dry_wipe(&mut self, reason: &str, hard: bool) {
        let kind = if hard { "HARD " } else { "" };
        self.emit(&format!(
            "DRY-RUN {kind}trigger ({reason}): zeroizing buffers; WOULD SIGKILL registered PIDs and self"
        ));
        (self.wipe)();
        self.fired = true;
        (self.dry_done)();
    }

    /// A graceful trigger (heartbeat loss, missed ping, `SIGUSR1`).
    ///
    /// Dry-run wipes and stands down; armed schedules a cancellable grace
    /// window (or executes immediately if `grace == 0`).
    pub fn request_graceful(&mut self, reason: &str) {
        if self.fired {
            return;
        }
        if !self.is_armed() {
            self.dry_wipe(reason, false);
            return;
        }
        if self.grace > Duration::ZERO {
            let deadline = (self.clock)() + self.grace;
            self.deadline = Some(deadline);
            self.emit(&format!(
                "ARMED trigger ({reason}): SIGKILL in {:?} unless a client reconnects or sends CANCEL",
                self.grace
            ));
        } else {
            self.execute(reason);
        }
    }

    /// A hard trigger (`SIGUSR2`): bypasses the grace window entirely.
    ///
    /// Dry-run still only wipes; armed executes the real self-destruct now.
    pub fn request_hard(&mut self, reason: &str) {
        if self.fired {
            return;
        }
        if !self.is_armed() {
            self.dry_wipe(reason, true);
            return;
        }
        self.execute(reason);
    }

    /// Abort a pending armed panic. Returns whether one was pending.
    pub fn cancel(&mut self, reason: &str) -> bool {
        if self.deadline.is_some() {
            self.deadline = None;
            self.emit(&format!("panic CANCELLED ({reason})"));
            true
        } else {
            false
        }
    }

    /// Drive the grace timer; call once per host-loop iteration / timeout.
    pub fn tick(&mut self, now: Instant) {
        if let Some(deadline) = self.deadline {
            if now >= deadline {
                self.execute("grace window expired");
            }
        }
    }

    /// The real self-destruct: SIGKILL registered PIDs, wipe, then self-kill.
    /// Only reached when armed.
    fn execute(&mut self, reason: &str) {
        if self.fired {
            return;
        }
        self.fired = true;
        self.deadline = None;
        self.emit(&format!(
            "PANIC ({reason}): SIGKILL registered PIDs, zeroize buffers, then SIGKILL self"
        ));
        (self.kill_registered)();
        (self.wipe)();
        (self.kill)();
    }
}
