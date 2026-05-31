//! Ports the Phase 1 (Python) `PanicControllerTest` suite to Rust, plus the
//! hard-trigger (SIGUSR2), runtime-arm, and registered-PID-kill ordering cases.
//! Every side effect is a recording fake, so the full decide/wipe/kill path runs
//! without the test process dying.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kikka::PanicController;

/// A mutable test clock backed by a shared cell so tests can advance time.
#[derive(Clone)]
struct FakeClock(Rc<Cell<Instant>>);

impl FakeClock {
    fn new() -> Self {
        FakeClock(Rc::new(Cell::new(Instant::now())))
    }
    fn advance(&self, by: Duration) {
        self.0.set(self.0.get() + by);
    }
    fn boxed(&self) -> Box<dyn Fn() -> Instant> {
        let inner = Rc::clone(&self.0);
        Box::new(move || inner.get())
    }
}

struct Counts {
    kreg: Rc<Cell<u32>>,
    wipes: Rc<Cell<u32>>,
    kills: Rc<Cell<u32>>,
    drys: Rc<Cell<u32>>,
}

fn make(armed: bool, grace_secs: u64, clock: &FakeClock) -> (PanicController, Counts) {
    let kreg = Rc::new(Cell::new(0));
    let wipes = Rc::new(Cell::new(0));
    let kills = Rc::new(Cell::new(0));
    let drys = Rc::new(Cell::new(0));

    let kr = Rc::clone(&kreg);
    let kill_registered = Box::new(move || kr.set(kr.get() + 1)) as Box<dyn FnMut()>;
    let w = Rc::clone(&wipes);
    let wipe = Box::new(move || w.set(w.get() + 1)) as Box<dyn FnMut()>;
    let k = Rc::clone(&kills);
    let kill = Box::new(move || k.set(k.get() + 1)) as Box<dyn FnMut()>;
    let d = Rc::clone(&drys);
    let dry_done = Box::new(move || d.set(d.get() + 1)) as Box<dyn FnMut()>;
    let log = Box::new(|_: &str| {}) as Box<dyn Fn(&str)>;

    let ctrl = PanicController::new(
        Arc::new(AtomicBool::new(armed)),
        Duration::from_secs(grace_secs),
        kill_registered,
        wipe,
        kill,
        dry_done,
        clock.boxed(),
        log,
    );
    (
        ctrl,
        Counts {
            kreg,
            wipes,
            kills,
            drys,
        },
    )
}

#[test]
fn dry_run_wipes_but_never_kills() {
    let clk = FakeClock::new();
    let (mut ctrl, c) = make(false, 5, &clk);
    ctrl.request_graceful("connection dropped");
    assert_eq!(c.wipes.get(), 1);
    assert_eq!(c.kills.get(), 0); // no self-kill in dry-run
    assert_eq!(c.kreg.get(), 0); // and no registered PIDs killed in dry-run
    assert_eq!(c.drys.get(), 1);
    assert!(ctrl.has_fired());
}

#[test]
fn armed_no_grace_kills_immediately() {
    let clk = FakeClock::new();
    let (mut ctrl, c) = make(true, 0, &clk);
    ctrl.request_graceful("panic signal");
    assert_eq!(c.kreg.get(), 1);
    assert_eq!(c.wipes.get(), 1);
    assert_eq!(c.kills.get(), 1);
}

#[test]
fn armed_grace_kills_only_after_deadline() {
    let clk = FakeClock::new();
    let (mut ctrl, c) = make(true, 5, &clk);
    ctrl.request_graceful("connection dropped");
    assert!(ctrl.pending_deadline().is_some());
    ctrl.tick(clk.0.get()); // before deadline
    assert_eq!(c.kills.get(), 0);
    clk.advance(Duration::from_millis(5001));
    ctrl.tick(clk.0.get()); // past deadline
    assert_eq!(c.kreg.get(), 1);
    assert_eq!(c.wipes.get(), 1);
    assert_eq!(c.kills.get(), 1);
}

#[test]
fn reconnect_cancels_pending_panic() {
    let clk = FakeClock::new();
    let (mut ctrl, c) = make(true, 5, &clk);
    ctrl.request_graceful("connection dropped");
    assert!(ctrl.cancel("client reconnected"));
    clk.advance(Duration::from_secs(10));
    ctrl.tick(clk.0.get());
    assert_eq!(c.kills.get(), 0);
    assert_eq!(c.kreg.get(), 0);
    assert!(!ctrl.has_fired());
}

#[test]
fn cancel_with_nothing_pending_returns_false() {
    let clk = FakeClock::new();
    let (mut ctrl, _c) = make(true, 5, &clk);
    assert!(!ctrl.cancel("noop"));
}

#[test]
fn fired_guard_prevents_double_kill_and_double_wipe() {
    let clk = FakeClock::new();
    let (mut ctrl, c) = make(true, 0, &clk);
    ctrl.request_graceful("first");
    ctrl.request_graceful("second");
    ctrl.request_hard("third");
    assert_eq!(c.kreg.get(), 1);
    assert_eq!(c.kills.get(), 1);
    assert_eq!(c.wipes.get(), 1);
}

#[test]
fn tick_with_no_deadline_is_noop() {
    let clk = FakeClock::new();
    let (mut ctrl, c) = make(true, 5, &clk);
    clk.advance(Duration::from_secs(3600));
    ctrl.tick(clk.0.get());
    assert_eq!(c.kills.get(), 0);
    assert_eq!(c.kreg.get(), 0);
    assert_eq!(c.wipes.get(), 0);
}

#[test]
fn hard_trigger_bypasses_grace_when_armed() {
    let clk = FakeClock::new();
    let (mut ctrl, c) = make(true, 30, &clk); // long grace
    ctrl.request_hard("SIGUSR2");
    assert_eq!(c.kreg.get(), 1);
    assert_eq!(c.wipes.get(), 1);
    assert_eq!(c.kills.get(), 1); // killed now, did not wait for grace
    assert!(ctrl.pending_deadline().is_none());
}

#[test]
fn hard_trigger_in_dry_run_wipes_but_does_not_kill() {
    let clk = FakeClock::new();
    let (mut ctrl, c) = make(false, 5, &clk);
    ctrl.request_hard("SIGUSR2");
    assert_eq!(c.wipes.get(), 1);
    assert_eq!(c.kills.get(), 0);
    assert_eq!(c.kreg.get(), 0);
    assert_eq!(c.drys.get(), 1);
}

#[test]
fn execute_order_is_registered_then_wipe_then_self() {
    // Spec: SIGKILL registered PIDs BEFORE wiping buffers and self-killing.
    let seq: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
    let s1 = Rc::clone(&seq);
    let kill_registered = Box::new(move || s1.borrow_mut().push("registered")) as Box<dyn FnMut()>;
    let s2 = Rc::clone(&seq);
    let wipe = Box::new(move || s2.borrow_mut().push("wipe")) as Box<dyn FnMut()>;
    let s3 = Rc::clone(&seq);
    let kill = Box::new(move || s3.borrow_mut().push("self")) as Box<dyn FnMut()>;
    let clk = FakeClock::new();
    let mut ctrl = PanicController::new(
        Arc::new(AtomicBool::new(true)),
        Duration::ZERO,
        kill_registered,
        wipe,
        kill,
        Box::new(|| {}),
        clk.boxed(),
        Box::new(|_: &str| {}),
    );
    ctrl.request_graceful("trigger");
    assert_eq!(*seq.borrow(), vec!["registered", "wipe", "self"]);
}
