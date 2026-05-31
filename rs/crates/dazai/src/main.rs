#![deny(unsafe_code)]
//! dazai Phase 2 CLI.
//!
//! `dazai daemon` runs the hardened watchdog; `dazai client` runs the heartbeat
//! client (the Rust replacement for Phase 1's `heartbeat.py`).
//!
//! Daemon startup order (matters for the security guarantees):
//! 1. raise `RLIMIT_MEMLOCK`
//! 2. `PR_SET_DUMPABLE=0` (Linux)
//! 3. allocate + lock the secret buffers
//! 4. spawn the LLM child (if `--exec`)
//! 5. bind the UNIX socket
//! 6. **apply seccomp** (Linux + `seccomp` feature) — after bind/alloc, before the loop
//! 7. enter the accept/event loop

use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use dazai_child::ChildProcess;
use dazai_secmem::SecretBuffer;
use dazai_watchdog::{SharedBuffers, Watchdog, WatchdogConfig};

#[derive(Parser)]
#[command(
    name = "dazai",
    version,
    about = "hardened session-bound dead-man's-switch daemon"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the watchdog daemon.
    Daemon(DaemonArgs),
    /// Run the heartbeat client (holds the liveness connection open).
    Client(ClientArgs),
}

#[derive(Args)]
struct DaemonArgs {
    /// Enable a REAL self-destruct (wipe + SIGKILL). Without this, runs in safe DRY-RUN.
    #[arg(long)]
    arm: bool,
    /// Armed graceful-panic grace window, seconds (reconnect/CANCEL aborts).
    #[arg(long, default_value_t = 5.0)]
    grace: f64,
    /// If > 0, panic when no PING arrives within this many seconds.
    #[arg(long = "ping-timeout", default_value_t = 0.0)]
    ping_timeout: f64,
    /// UNIX socket path (default: ${XDG_RUNTIME_DIR:-/tmp}/dazai-$UID.sock).
    #[arg(long)]
    socket: Option<PathBuf>,
    /// LLM runtime to spawn as a child (parent owns the kill switch).
    #[arg(long)]
    exec: Option<PathBuf>,
    /// Synthetic working-buffer size in bytes.
    #[arg(long, default_value_t = 4096)]
    size: usize,
}

#[derive(Args)]
struct ClientArgs {
    /// Send PING every N seconds; 0 = just hold the connection open.
    #[arg(long, default_value_t = 0.0)]
    interval: f64,
    /// UNIX socket path (default: ${XDG_RUNTIME_DIR:-/tmp}/dazai-$UID.sock).
    #[arg(long)]
    socket: Option<PathBuf>,
}

fn default_socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join(format!("dazai-{}.sock", dazai_secmem::current_uid()))
}

/// Parse a seconds-valued CLI flag into a `Duration` without ever panicking.
///
/// `Duration::from_secs_f64` panics on NaN/infinite/out-of-range input;
/// `try_from_secs_f64` returns an error instead (and also rejects negatives),
/// which we surface on the normal error path.
fn duration_secs(flag: &str, secs: f64) -> Result<Duration> {
    Duration::try_from_secs_f64(secs).map_err(|e| {
        anyhow::anyhow!("{flag} must be a finite, non-negative number of seconds ({e})")
    })
}

/// Loud notice on non-Linux about which hardening guarantees are absent.
fn platform_warnings() {
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("[dazai] ============ PLATFORM GUARANTEE NOTICE ============");
        eprintln!("[dazai] Non-Linux OS detected. The following are ABSENT here:");
        eprintln!("[dazai]   - madvise(MADV_DONTDUMP): pages may appear in core dumps");
        eprintln!("[dazai]   - prctl(PR_SET_DUMPABLE,0): no ptrace/core-dump hardening");
        eprintln!("[dazai]   - seccomp syscall confinement");
        eprintln!("[dazai] ACTIVE: mlock (no swap) + non-elidable explicit wipe.");
        eprintln!("[dazai] ===================================================");
    }
}

fn make_buffers(size: usize) -> Result<SharedBuffers> {
    // A small "key" buffer + a larger working buffer, both mlock'd. A real key
    // would be loaded here; for Phase 2 the contents are synthetic.
    let mut key = SecretBuffer::new(32).context("allocating key buffer")?;
    key.write(b"synthetic-key-material")
        .context("writing key")?;
    let mut work = SecretBuffer::new(size).context("allocating work buffer")?;
    work.write(b"SYNTHETIC SECRET -- if readable after a wipe, zeroize failed.\n")
        .context("writing work buffer")?;
    let locked = [&key, &work].iter().filter(|b| b.is_locked()).count();
    eprintln!("[dazai] allocated 2 secret buffer(s), {locked} mlock'd");
    Ok(Rc::new(RefCell::new(vec![key, work])))
}

fn run_daemon(args: DaemonArgs) -> Result<()> {
    if args.size == 0 {
        bail!("--size must be > 0");
    }
    // Validate durations up front (before allocating/locking any secret), so
    // bad input fails on the clean error path rather than panicking later.
    let grace = duration_secs("--grace", args.grace)?;
    if args.ping_timeout < 0.0 {
        bail!("--ping-timeout must be >= 0");
    }
    let ping_timeout = if args.ping_timeout > 0.0 {
        Some(duration_secs("--ping-timeout", args.ping_timeout)?)
    } else {
        None
    };

    // 1. RLIMIT_MEMLOCK
    match dazai_secmem::try_raise_memlock_rlimit() {
        Ok(true) => eprintln!("[dazai] RLIMIT_MEMLOCK raised"),
        Ok(false) => {
            eprintln!(
                "[dazai] WARN: could not raise RLIMIT_MEMLOCK (need CAP_IPC_LOCK?); continuing"
            )
        }
        Err(e) => eprintln!("[dazai] WARN: RLIMIT_MEMLOCK query failed: {e}"),
    }
    // 2. PR_SET_DUMPABLE
    match dazai_secmem::set_process_undumpable() {
        Ok(()) => eprintln!("[dazai] PR_SET_DUMPABLE=0 (core dumps + ptrace disabled)"),
        Err(e) => eprintln!("[dazai] note: core-dump hardening unavailable: {e}"),
    }
    platform_warnings();

    // 3. buffers
    let buffers = make_buffers(args.size)?;

    // 4. child (parent owns the kill switch)
    let child = Rc::new(RefCell::new(match &args.exec {
        Some(path) => ChildProcess::spawn(path, &[])?,
        None => ChildProcess::none(),
    }));

    // 5. injected lethal action: kill child, then SIGKILL self.
    let child_for_kill = Rc::clone(&child);
    let kill = Box::new(move || {
        child_for_kill.borrow_mut().kill();
        eprintln!("[dazai] raising SIGKILL on self");
        let _ = signal_hook::low_level::raise(signal_hook::consts::SIGKILL);
    }) as Box<dyn FnMut()>;

    let socket_path = args.socket.unwrap_or_else(default_socket_path);
    let config = WatchdogConfig {
        socket_path,
        armed: args.arm,
        grace,
        ping_timeout,
    };

    let mut watchdog = Watchdog::new(config, Rc::clone(&buffers), kill);

    // 6. bind, then apply seccomp BEFORE the accept loop.
    let listener = watchdog.bind_listener()?;
    dazai_seccomp::apply().context("applying seccomp filter")?;

    // 7. event loop (returns on clean shutdown / dry-run completion; armed
    //    triggers SIGKILL the process from within).
    watchdog.run_with_listener(listener)?;

    // Clean shutdown: dropping `child` (last Rc) kills it; dropping `buffers`
    // wipes + munmaps.
    drop(watchdog);
    drop(child);
    drop(buffers);
    Ok(())
}

fn run_client(args: ClientArgs) -> Result<()> {
    let socket = args.socket.unwrap_or_else(default_socket_path);
    let mut stream = UnixStream::connect(&socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    writeln!(stream, "HELLO {}", std::process::id()).context("sending HELLO")?;

    // Bound the handshake read so a hung daemon doesn't block us forever.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .context("setting handshake read timeout")?;
    let mut buf = [0u8; 64];
    let reply = match stream.read(&mut buf) {
        Ok(n) => String::from_utf8_lossy(&buf[..n]).trim().to_string(),
        Err(_) => String::new(),
    };
    if reply.contains("BUSY") {
        bail!("daemon refused the connection: another heartbeat client is already connected");
    }
    if !reply.contains("WELCOME") {
        bail!("unexpected handshake reply from daemon: {reply:?}");
    }
    eprintln!("[client] connected: {reply}");

    if args.interval > 0.0 {
        let interval = duration_secs("--interval", args.interval)?;
        // Expect a PONG within interval + slack; otherwise the daemon is gone
        // or unresponsive and we should stop holding the heartbeat open.
        stream
            .set_read_timeout(Some(interval + Duration::from_secs(2)))
            .context("setting ping read timeout")?;
        loop {
            thread::sleep(interval);
            if writeln!(stream, "PING").is_err() {
                eprintln!("[client] daemon closed the connection");
                break;
            }
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => {
                    eprintln!("[client] no PONG (daemon lost/unresponsive)");
                    break;
                }
                Ok(n) => {
                    if !String::from_utf8_lossy(&buf[..n]).contains("PONG") {
                        eprintln!("[client] unexpected reply to PING; treating as lost");
                        break;
                    }
                }
            }
        }
    } else {
        // Hold the connection open (blocking) until the daemon closes it.
        stream
            .set_read_timeout(None)
            .context("clearing read timeout")?;
        loop {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }
    eprintln!("[client] connection closed");
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Daemon(args) => run_daemon(args),
        Cmd::Client(args) => run_client(args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[dazai] error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
