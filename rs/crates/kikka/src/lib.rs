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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use goodnight::SecretBuffer;
use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1, SIGUSR2};
use signal_hook::iterator::Signals;

/// Largest line accepted from a client before it is treated as a protocol
/// violation, bounding memory against a peer that never sends a newline.
const MAX_LINE: usize = 8192;

/// Maximum number of PIDs that may be registered for SIGKILL-on-trigger.
const MAX_REGISTERED: usize = 32;

/// Maximum number of concurrent control connections (bounds reader threads
/// against a same-UID local flood).
const MAX_CONTROL: usize = 16;

/// Registered PIDs, shared between control handlers and the kill path.
/// PIDs are not sensitive, so a plain `Vec<u32>` (behind a `Mutex`) — no
/// `SecretBuffer`.
pub type RegisteredPids = Arc<Mutex<Vec<u32>>>;

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

/// Per-connection context handed to control-connection handlers (clone-cheap).
#[derive(Clone)]
struct ConnCtx {
    registered: RegisteredPids,
    armed: Arc<AtomicBool>,
    grace: Duration,
    /// In-flight control connections, to bound concurrent reader threads.
    control_count: Arc<AtomicUsize>,
}

/// The watchdog: owns the socket, the secret buffers, and the panic controller.
pub struct Watchdog {
    config: WatchdogConfig,
    buffers: SharedBuffers,
    controller: PanicController,
    stop: Arc<AtomicBool>,
    /// Runtime-mutable arm flag, shared with the controller and `ARM` handler.
    armed: Arc<AtomicBool>,
    /// PIDs registered for SIGKILL-on-trigger, shared with control handlers.
    registered: RegisteredPids,
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
        let armed = Arc::new(AtomicBool::new(config.armed));
        let registered: RegisteredPids = Arc::new(Mutex::new(Vec::new()));

        // kill_registered: SIGKILL every registered PID. Runs first on an armed
        // trigger, before the buffer wipe. Reads the list the control handlers
        // maintain; recovers a poisoned lock rather than panicking.
        let reg_kill = Arc::clone(&registered);
        let kill_registered = Box::new(move || {
            let pids = reg_kill.lock().unwrap_or_else(|p| p.into_inner());
            for &pid in pids.iter() {
                let killed = goodnight::sigkill_pid(pid);
                eprintln!(
                    "[dazai] SIGKILL registered pid {pid}: {}",
                    if killed { "sent" } else { "already gone" }
                );
            }
        }) as Box<dyn FnMut()>;

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

        let controller = PanicController::new(
            Arc::clone(&armed),
            config.grace,
            kill_registered,
            wipe,
            kill,
            dry_done,
            clock,
            log,
        );

        Watchdog {
            config,
            buffers,
            controller,
            stop,
            armed,
            registered,
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
        let ctx = ConnCtx {
            registered: Arc::clone(&self.registered),
            armed: Arc::clone(&self.armed),
            grace: self.config.grace,
            control_count: Arc::new(AtomicUsize::new(0)),
        };
        thread::spawn(move || {
            for conn in listener.incoming() {
                match conn {
                    Ok(stream) => {
                        let tx = acc_tx.clone();
                        let active = Arc::clone(&active);
                        let gen_source = Arc::clone(&gen_source);
                        let ctx = ctx.clone();
                        thread::spawn(move || {
                            handle_conn(stream, active, gen_source, tx, ping, ctx)
                        });
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

/// Read one bounded, newline-terminated line into `buf` (without the newline).
/// `Ok(true)` => a line was read; `Ok(false)` => clean EOF with nothing
/// buffered; `Err` => timeout, I/O error, or an over-long line.
fn read_line_bounded<R: Read>(reader: &mut R, buf: &mut Vec<u8>) -> std::io::Result<bool> {
    buf.clear();
    loop {
        match read_byte(reader)? {
            None => return Ok(!buf.is_empty()),
            Some(b'\n') => return Ok(true),
            Some(byte) => {
                buf.push(byte);
                if buf.len() >= MAX_LINE {
                    return Err(std::io::Error::new(ErrorKind::InvalidData, "line too long"));
                }
            }
        }
    }
}

/// The uppercased first whitespace-delimited word of a line.
fn first_word_upper(line: &[u8]) -> String {
    String::from_utf8_lossy(line)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
}

/// Parse a `pid=<N>` token from a line, if present.
fn parse_pid(line: &[u8]) -> Option<u32> {
    String::from_utf8_lossy(line)
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("pid=").and_then(|v| v.parse::<u32>().ok()))
}

/// Per-connection dispatcher. The first verb decides the connection's role:
/// `HELLO` => a heartbeat client (single-client lock + liveness/grace);
/// anything else => a control connection (REGISTER / UNREGISTER / ARM / STATUS),
/// which is request/response and never touches the heartbeat lock or liveness.
fn handle_conn(
    stream: UnixStream,
    active: Arc<AtomicBool>,
    gen_source: Arc<AtomicU64>,
    tx: Sender<Event>,
    ping: Option<Duration>,
    ctx: ConnCtx,
) {
    // Bound the handshake so a silent connection cannot pin a thread forever.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let write_half = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let mut line: Vec<u8> = Vec::with_capacity(64);
    match read_line_bounded(&mut reader, &mut line) {
        Ok(true) => {}
        _ => return, // EOF / timeout / over-long before any verb
    }

    if first_word_upper(&line) == "HELLO" {
        handle_heartbeat(reader, write_half, active, gen_source, tx, ping);
    } else {
        handle_control(line, reader, write_half, ctx);
    }
}

/// Heartbeat-client handler (entered after `HELLO`). Enforces the single-client
/// lock, emits generation-tagged liveness events, and enforces the ping
/// deadline per *complete line* (not per read) so a byte-trickle client cannot
/// defer it indefinitely.
fn handle_heartbeat(
    mut reader: BufReader<UnixStream>,
    mut write_half: UnixStream,
    active: Arc<AtomicBool>,
    gen_source: Arc<AtomicU64>,
    tx: Sender<Event>,
    ping: Option<Duration>,
) {
    // Single-client policy: refuse a second heartbeat.
    if active.swap(true, Ordering::SeqCst) {
        let _ = write_half.write_all(b"BUSY\n");
        return;
    }
    let generation = gen_source.fetch_add(1, Ordering::SeqCst) + 1;
    let _ = tx.send(Event::Connected(generation));
    let _ = write_half.write_all(b"WELCOME\n");

    // Small poll granularity so the absolute deadline is re-checked promptly;
    // None => block on reads (only a connection drop can trigger).
    let poll = ping.map(|t| t.min(Duration::from_millis(200)));
    let _ = reader.get_ref().set_read_timeout(poll);
    let mut deadline = ping.map(|t| Instant::now() + t);
    let mut line: Vec<u8> = Vec::with_capacity(64);

    // Clear `active` BEFORE emitting a terminal event so a fast reconnect is not
    // spuriously refused; the generation id (not the clear order) protects the
    // main loop against the resulting event reorder.
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
                match first_word_upper(&line).as_str() {
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
                deadline = ping.map(|t| Instant::now() + t);
            }
            Ok(Some(byte)) => {
                line.push(byte);
                if line.len() >= MAX_LINE {
                    active.store(false, Ordering::SeqCst);
                    let _ = tx.send(Event::HeartbeatLost(generation));
                    return;
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {}
            Err(_) => {
                active.store(false, Ordering::SeqCst);
                let _ = tx.send(Event::HeartbeatLost(generation));
                return;
            }
        }
    }
}

/// Decrements the control-connection counter when a handler exits.
struct ControlGuard(Arc<AtomicUsize>);
impl Drop for ControlGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Control-connection handler: REGISTER / UNREGISTER / ARM / STATUS (+ PING),
/// request/response, looping until the client disconnects. Never affects the
/// heartbeat lock or liveness.
fn handle_control(
    first_line: Vec<u8>,
    mut reader: BufReader<UnixStream>,
    mut write_half: UnixStream,
    ctx: ConnCtx,
) {
    // Bound concurrent control connections (and their reader threads).
    let in_flight = ctx.control_count.fetch_add(1, Ordering::SeqCst);
    let _guard = ControlGuard(Arc::clone(&ctx.control_count));
    if in_flight >= MAX_CONTROL {
        let _ = write_half.write_all(b"BUSY\n");
        return;
    }
    // The 10s deadline bounded the handshake only; clear it so a persistent
    // control connection is not dropped after 10s idle (reads now block until
    // the next request or EOF).
    let _ = reader.get_ref().set_read_timeout(None);

    let mut line = first_line;
    loop {
        if !process_control(&line, &mut write_half, &ctx) {
            return; // QUIT
        }
        line.clear();
        match read_line_bounded(&mut reader, &mut line) {
            Ok(true) => {}
            _ => return,
        }
    }
}

/// Handle one control verb. Returns whether the connection should stay open.
fn process_control(line: &[u8], out: &mut UnixStream, ctx: &ConnCtx) -> bool {
    match first_word_upper(line).as_str() {
        "REGISTER" => {
            // Compute the reply under the lock, then DROP the guard before
            // writing — never hold the registered-PID mutex across a blocking
            // socket write, or a stalled client could defer the kill path.
            let reply: &[u8] = match parse_pid(line) {
                // pid_exists rejects pid 0 / out-of-range, covering "> 0, exists".
                Some(pid) if goodnight::pid_exists(pid) => {
                    let mut reg = ctx.registered.lock().unwrap_or_else(|p| p.into_inner());
                    if reg.contains(&pid) {
                        b"OK\n" // idempotent
                    } else if reg.len() >= MAX_REGISTERED {
                        b"BUSY\n"
                    } else {
                        reg.push(pid);
                        eprintln!("[dazai] registered pid {pid} ({} total)", reg.len());
                        b"OK\n"
                    }
                }
                _ => b"ERROR invalid pid\n",
            };
            let _ = out.write_all(reply);
            true
        }
        "UNREGISTER" => {
            let reply: &[u8] = if let Some(pid) = parse_pid(line) {
                let mut reg = ctx.registered.lock().unwrap_or_else(|p| p.into_inner());
                reg.retain(|&p| p != pid);
                b"OK\n"
            } else {
                b"ERROR invalid pid\n"
            };
            let _ = out.write_all(reply);
            true
        }
        "ARM" => {
            if ctx.armed.swap(true, Ordering::SeqCst) {
                let _ = out.write_all(b"ALREADY_ARMED\n");
            } else {
                eprintln!(
                    "[dazai] ARMED at runtime via control message — self-destruct is now LIVE"
                );
                let _ = out.write_all(b"OK\n");
            }
            true
        }
        "STATUS" => {
            let registered = match ctx.registered.lock() {
                Ok(g) => g.len(),
                Err(p) => p.into_inner().len(),
            };
            let armed = ctx.armed.load(Ordering::SeqCst) as u8;
            let _ = out.write_all(
                format!(
                    "STATUS alive=1 armed={armed} grace={} registered={registered}\n",
                    ctx.grace.as_secs()
                )
                .as_bytes(),
            );
            true
        }
        "PING" => {
            let _ = out.write_all(b"PONG\n");
            true
        }
        "QUIT" => {
            let _ = out.write_all(b"BYE\n");
            false
        }
        "" => true,
        _ => {
            let _ = out.write_all(b"ERR unknown-verb\n");
            true
        }
    }
}
