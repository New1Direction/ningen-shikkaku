#![deny(unsafe_code)]
//! Heartbeat listener + panic controller for dazai.
//!
//! The [`Watchdog`] binds a UNIX-domain socket whose connected client is the
//! *heartbeat*; losing that connection (or missing a ping deadline) triggers a
//! panic. Signals are delivered through `signal-hook`'s async-signal-safe
//! self-pipe and turned into events on a channel, alongside socket events from
//! per-connection reader threads. A single-threaded event loop consumes those
//! events and drives the injectable [`PanicController`].
//!
//! Threading model (no `unsafe` anywhere): one signal thread, one accept thread
//! that spawns a short-lived reader thread per connection, and the main thread
//! running the event loop + grace timer. Secret buffers and the controller live
//! only on the main thread; worker threads merely send `Event`s.

mod panic;
pub use panic::PanicController;

use std::cell::RefCell;
use std::io::{BufReader, ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dazai_secmem::SecretBuffer;
use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1, SIGUSR2};
use signal_hook::iterator::Signals;

/// Largest line accepted from a client before it is treated as a protocol
/// violation, bounding memory against a peer that never sends a newline.
const MAX_LINE: usize = 8192;

/// Secret buffers shared between the host and the wipe closure. Lives on the
/// main thread only (`Rc`), so it is never sent across threads.
pub type SharedBuffers = Rc<RefCell<Vec<SecretBuffer>>>;

/// Events fed to the watchdog's main loop.
///
/// `Connected`/`HeartbeatLost`/`PingTimeout` carry a per-connection generation
/// id. Two reader threads can interleave their channel sends (a fast reconnect
/// can enqueue `Connected(N+1)` before the dropped connection's
/// `HeartbeatLost(N)`), so the loop tracks the current generation and ignores a
/// liveness-loss event whose generation has already been superseded — otherwise
/// an armed daemon could schedule an uncancellable panic against a live client.
#[derive(Debug)]
enum Event {
    /// A new (sole) heartbeat client connected, with its generation id.
    Connected(u64),
    /// The heartbeat connection for this generation dropped (EOF / error).
    HeartbeatLost(u64),
    /// No ping arrived within the deadline for this generation.
    PingTimeout(u64),
    /// Client sent `CANCEL`.
    Cancel,
    /// Client sent `QUIT` (intentional clean stand-down).
    Quit,
    /// A signal was delivered.
    Signal(i32),
}

/// Runtime configuration for the watchdog.
pub struct WatchdogConfig {
    /// Path of the UNIX socket to listen on.
    pub socket_path: PathBuf,
    /// Enable the real self-destruct (`false` = safe dry-run).
    pub armed: bool,
    /// Grace window for armed graceful panics.
    pub grace: Duration,
    /// Optional missed-ping deadline (`None` = rely on connection liveness).
    pub ping_timeout: Option<Duration>,
}

/// The watchdog: owns the socket, the secret buffers, and the panic controller.
pub struct Watchdog {
    config: WatchdogConfig,
    buffers: SharedBuffers,
    controller: PanicController,
    stop: Arc<AtomicBool>,
}

impl Watchdog {
    /// Build a watchdog.
    ///
    /// `buffers` are the secret buffers to wipe on any trigger; `kill` is the
    /// injected lethal action (in production: kill the child process, then
    /// `raise(SIGKILL)` on self). The controller's dry-run completion flips an
    /// internal stop flag so the loop exits cleanly in dry-run mode.
    pub fn new(config: WatchdogConfig, buffers: SharedBuffers, kill: Box<dyn FnMut()>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));

        let wipe_bufs = Rc::clone(&buffers);
        let wipe = Box::new(move || {
            let mut bufs = wipe_bufs.borrow_mut();
            for buf in bufs.iter_mut() {
                buf.wipe();
            }
            eprintln!("[dazai] wiped {} secret buffer(s)", bufs.len());
        }) as Box<dyn FnMut()>;

        let stop_flag = Arc::clone(&stop);
        let dry_done =
            Box::new(move || stop_flag.store(true, Ordering::SeqCst)) as Box<dyn FnMut()>;

        let clock = Box::new(Instant::now) as Box<dyn Fn() -> Instant>;
        let log = Box::new(|m: &str| eprintln!("[dazai] {m}")) as Box<dyn Fn(&str)>;

        let controller =
            PanicController::new(config.armed, config.grace, wipe, kill, dry_done, clock, log);

        Watchdog {
            config,
            buffers,
            controller,
            stop,
        }
    }

    fn bind(&self) -> Result<UnixListener> {
        let path = &self.config.socket_path;
        // Remove a stale socket from a previous run.
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
        let listener = UnixListener::bind(path)
            .with_context(|| format!("binding socket {}", path.display()))?;
        // Restrict to the owner. (Created under XDG_RUNTIME_DIR / a 0700 dir;
        // the brief window before chmod is mitigated by the parent dir mode.)
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", path.display()))?;
        Ok(listener)
    }

    fn banner(&self) {
        let mode = if self.config.armed {
            "ARMED — real wipe + SIGKILL on trigger"
        } else {
            "DRY-RUN — wipe + log WOULD-SIGKILL (pass --arm for real)"
        };
        eprintln!("[dazai] ============================================================");
        eprintln!("[dazai] watchdog pid={} mode={}", std::process::id(), mode);
        eprintln!("[dazai] socket={}", self.config.socket_path.display());
        eprintln!(
            "[dazai] grace={:?} ping_timeout={:?}",
            self.config.grace, self.config.ping_timeout
        );
        eprintln!("[dazai] signals: SIGUSR1=graceful SIGUSR2=hard SIGTERM/INT=clean-exit");
        eprintln!("[dazai] ============================================================");
    }

    /// A hook the host can call *after* the socket is bound and buffers are
    /// allocated but *before* the accept loop — e.g. to apply seccomp.
    ///
    /// Splits binding from looping so the caller can interpose. Returns the
    /// bound listener; pass it to [`Watchdog::run_with_listener`].
    pub fn bind_listener(&self) -> Result<UnixListener> {
        self.bind()
    }

    /// Bind the socket and run the event loop until a clean shutdown.
    pub fn run(&mut self) -> Result<()> {
        let listener = self.bind()?;
        self.run_with_listener(listener)
    }

    /// Run the event loop on an already-bound listener.
    pub fn run_with_listener(&mut self, listener: UnixListener) -> Result<()> {
        let (tx, rx) = mpsc::channel::<Event>();

        // Signal thread: signal-hook installs an async-signal-safe handler that
        // writes to its internal self-pipe; we read decoded signals here.
        let mut signals = Signals::new([SIGUSR1, SIGUSR2, SIGTERM, SIGINT])
            .context("installing signal handlers")?;
        let sig_tx = tx.clone();
        thread::spawn(move || {
            for sig in signals.forever() {
                if sig_tx.send(Event::Signal(sig)).is_err() {
                    break;
                }
            }
        });

        // Accept thread: one short-lived reader thread per connection. A shared
        // `active` flag enforces the single-client policy (extra clients get
        // BUSY) regardless of which thread accepts them.
        let active = Arc::new(AtomicBool::new(false));
        let gen_source = Arc::new(AtomicU64::new(0));
        let ping = self.config.ping_timeout;
        let acc_tx = tx.clone();
        thread::spawn(move || {
            for conn in listener.incoming() {
                match conn {
                    Ok(stream) => {
                        let tx = acc_tx.clone();
                        let active = Arc::clone(&active);
                        let gen_source = Arc::clone(&gen_source);
                        thread::spawn(move || handle_conn(stream, active, gen_source, tx, ping));
                    }
                    Err(_) => break,
                }
            }
        });

        self.banner();

        // Authoritative liveness state, owned by this single thread. `present`
        // is whether a client is currently connected; `current_gen` is its
        // generation. A liveness-loss event is honored only if it matches the
        // generation we still believe is live — this is what makes the
        // Connected/HeartbeatLost channel reorder safe.
        let mut current_gen: u64 = 0;
        let mut present = false;

        loop {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            let now = Instant::now();
            let timeout = self
                .controller
                .pending_deadline()
                .map(|d| d.saturating_duration_since(now));
            let event = match timeout {
                Some(t) => rx.recv_timeout(t),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match event {
                Ok(Event::Connected(g)) => {
                    current_gen = g;
                    present = true;
                    self.controller.cancel("client reconnected");
                }
                Ok(Event::HeartbeatLost(g)) => {
                    if present && g == current_gen {
                        present = false;
                        self.controller
                            .request_graceful("heartbeat connection dropped");
                    }
                    // else: stale loss from a superseded connection -> ignore.
                }
                Ok(Event::PingTimeout(g)) => {
                    if present && g == current_gen {
                        present = false;
                        self.controller.request_graceful("ping deadline missed");
                    }
                }
                Ok(Event::Cancel) => {
                    self.controller.cancel("client CANCEL");
                }
                Ok(Event::Quit) => {
                    eprintln!("[dazai] client QUIT: intentional clean stand-down");
                    break;
                }
                Ok(Event::Signal(s)) if s == SIGUSR1 => {
                    self.controller.request_graceful("SIGUSR1 panic signal");
                }
                Ok(Event::Signal(s)) if s == SIGUSR2 => {
                    self.controller.request_hard("SIGUSR2 hard panic");
                }
                Ok(Event::Signal(_)) => {
                    eprintln!("[dazai] SIGTERM/SIGINT: clean shutdown (zeroize, no kill)");
                    break;
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.controller.tick(Instant::now());
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        self.cleanup();
        Ok(())
    }

    fn cleanup(&mut self) {
        // Always wipe on the way out, even for a clean shutdown.
        {
            let mut bufs = self.buffers.borrow_mut();
            for buf in bufs.iter_mut() {
                buf.wipe();
            }
        }
        if self.config.socket_path.exists() {
            let _ = std::fs::remove_file(&self.config.socket_path);
        }
    }
}

/// Read a single byte. `Ok(None)` is EOF; a read timeout surfaces as
/// [`ErrorKind::WouldBlock`]/[`ErrorKind::TimedOut`].
fn read_byte<R: Read>(reader: &mut R) -> std::io::Result<Option<u8>> {
    let mut byte = [0u8; 1];
    match reader.read(&mut byte)? {
        0 => Ok(None),
        _ => Ok(Some(byte[0])),
    }
}

/// Per-connection handler. Enforces the single-client policy via `active`,
/// translates protocol verbs, and emits generation-tagged liveness events.
///
/// The ping deadline is enforced per *complete line*, not per read: the OS read
/// timeout is only a small polling granularity, while an absolute `deadline`
/// (reset on each full line) decides a missed ping. This stops a byte-trickle
/// client from indefinitely deferring the deadline by sending one byte at a
/// time without ever completing a `PING`.
fn handle_conn(
    stream: UnixStream,
    active: Arc<AtomicBool>,
    gen_source: Arc<AtomicU64>,
    tx: Sender<Event>,
    ping: Option<Duration>,
) {
    // Single-client policy: if a heartbeat is already held, refuse this one.
    if active.swap(true, Ordering::SeqCst) {
        let mut s = stream;
        let _ = s.write_all(b"BUSY\n");
        // We never owned the slot; leave `active` set (it belongs to the real
        // client) and just drop this connection.
        return;
    }
    // We own the slot. Take a generation id and announce the connection.
    let generation = gen_source.fetch_add(1, Ordering::SeqCst) + 1;
    let _ = tx.send(Event::Connected(generation));

    // Small poll granularity so the absolute deadline is re-checked promptly;
    // None => block on reads (only a connection drop can trigger).
    let poll = ping.map(|t| t.min(Duration::from_millis(200)));
    let _ = stream.set_read_timeout(poll);
    let mut deadline = ping.map(|t| Instant::now() + t);

    let mut write_half = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => {
            active.store(false, Ordering::SeqCst);
            let _ = tx.send(Event::HeartbeatLost(generation));
            return;
        }
    };
    let mut reader = BufReader::new(stream);
    let mut line: Vec<u8> = Vec::with_capacity(64);

    // Clear `active` BEFORE emitting a terminal event so a fast reconnect is not
    // spuriously refused with BUSY. The generation id (not the clear order)
    // protects the loop against the resulting event reorder.
    loop {
        if let Some(dl) = deadline {
            if Instant::now() >= dl {
                active.store(false, Ordering::SeqCst);
                let _ = tx.send(Event::PingTimeout(generation));
                return;
            }
        }
        match read_byte(&mut reader) {
            Ok(None) => {
                active.store(false, Ordering::SeqCst);
                let _ = tx.send(Event::HeartbeatLost(generation));
                return;
            }
            Ok(Some(b'\n')) => {
                let text = String::from_utf8_lossy(&line);
                let verb = text
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_ascii_uppercase();
                match verb.as_str() {
                    "HELLO" => {
                        let _ = write_half.write_all(b"WELCOME\n");
                    }
                    "PING" => {
                        let _ = write_half.write_all(b"PONG\n");
                    }
                    "CANCEL" => {
                        let _ = tx.send(Event::Cancel);
                        let _ = write_half.write_all(b"CANCELLED\n");
                    }
                    "QUIT" => {
                        let _ = write_half.write_all(b"BYE\n");
                        active.store(false, Ordering::SeqCst);
                        let _ = tx.send(Event::Quit);
                        return;
                    }
                    "" => {}
                    _ => {
                        let _ = write_half.write_all(b"ERR unknown-verb\n");
                    }
                }
                line.clear();
                // A complete line satisfies the ping deadline; reset it.
                deadline = ping.map(|t| Instant::now() + t);
            }
            Ok(Some(byte)) => {
                line.push(byte);
                if line.len() >= MAX_LINE {
                    // Protocol violation (no newline within the cap): drop it.
                    active.store(false, Ordering::SeqCst);
                    let _ = tx.send(Event::HeartbeatLost(generation));
                    return;
                }
            }
            // Poll tick with no byte: loop and let the deadline check decide.
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
            Err(_) => {
                active.store(false, Ordering::SeqCst);
                let _ = tx.send(Event::HeartbeatLost(generation));
                return;
            }
        }
    }
}
