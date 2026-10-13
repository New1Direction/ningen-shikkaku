#![deny(unsafe_code)]
//! Non-secret lifecycle records for [Korg](https://github.com/New1Direction/korg).
//!
//! `kiroku` (記録, "record") is the ningen-shikkaku side of the narrow
//! ningen-shikkaku → Korg interoperability contract, `ningen-korg-lifecycle@1`.
//! `dazai daemon` and `motokano` emit one JSON line per lifecycle step —
//! session start/end, arm, read, wipe, kill — in Korg's **append shape** (the
//! `NormalizedEvent` body accepted by `POST /api/agent/tool-call`). Korg owns
//! everything else: `seq_id`, `prev_hash`/`entry_hash`, HLC timestamps,
//! `event_id`, signatures and anchors are all assigned on ingest, never here.
//!
//! ## Non-leakage by construction
//! A record is built only from [`Lifecycle`], whose fields are closed enums,
//! booleans and small counters. There is no `String`, byte slice or generic
//! payload anywhere in the event type, so a secret value, raw tool argument,
//! command line, tool name, path, environment variable or any digest of them
//! cannot be handed to the emitter in the first place. The only per-process
//! value is a random session id read from the OS RNG at [`Sink::open`]; it is
//! independent of every secret and input.
//!
//! ## Daemon / seccomp constraints
//! [`Sink::open`] performs every syscall that the dazai seccomp filter denies
//! (`openat`, the RNG read) up front, so it must be called *before*
//! `kekkai::apply`. After that, [`Sink::emit`] issues exactly one `write(2)` per
//! record on the already-open descriptor (plus a vDSO clock read), both of which
//! the filter allows. The descriptor is `O_APPEND | O_NONBLOCK | O_CLOEXEC`:
//! appends are atomic per record, a stalled pipe reader drops records instead
//! of blocking the kill path, and the `--exec` child never inherits it.
//! Pass `/dev/fd/N` to write to a descriptor the parent pre-opened.

pub mod contract;

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Contract identifier stamped into every record's `args.contract`.
pub const CONTRACT: &str = "ningen-korg-lifecycle@1";

/// Producer identity stamped into every record's `args.producer`.
pub const PRODUCER: &str = concat!("ningen-shikkaku@", env!("CARGO_PKG_VERSION"));

/// Upper bound on one record, newline included. Kept at or below the POSIX
/// minimum `PIPE_BUF` (512) so a single `write(2)` to a pipe is atomic on every
/// platform: a record is either delivered whole or dropped, never torn.
pub const MAX_RECORD_BYTES: usize = 512;
const _: () = assert!(MAX_RECORD_BYTES <= 512, "POSIX PIPE_BUF minimum");

/// The process emitting a record. Maps to Korg's `source_agent` (spec §1.1
/// `mcp:<server-name>` form — both binaries are ningen-shikkaku MCP servers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    /// `dazai daemon`.
    Dazai,
    /// `motokano`.
    Motokano,
}

impl Component {
    /// Every variant (for exhaustive tests and docs).
    pub const ALL: &'static [Component] = &[Component::Dazai, Component::Motokano];

    /// Short identifier written to `args.component`.
    pub fn as_str(self) -> &'static str {
        match self {
            Component::Dazai => "dazai",
            Component::Motokano => "motokano",
        }
    }

    /// Korg `source_agent` for this component.
    pub fn source_agent(self) -> &'static str {
        match self {
            Component::Dazai => "mcp:dazai",
            Component::Motokano => "mcp:motokano",
        }
    }
}

/// How the process became armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmVia {
    /// `--arm` on the command line.
    Startup,
    /// An `ARM` control message at runtime (dazai only).
    Control,
}

impl ArmVia {
    /// Every variant.
    pub const ALL: &'static [ArmVia] = &[ArmVia::Startup, ArmVia::Control];

    /// Identifier written to `args.via`.
    pub fn as_str(self) -> &'static str {
        match self {
            ArmVia::Startup => "startup",
            ArmVia::Control => "control",
        }
    }
}

/// Why a wipe / kill / session end happened. A fixed vocabulary: free-form
/// log text (which could quote a path or an error message) is never recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// dazai: the heartbeat connection dropped.
    HeartbeatLost,
    /// dazai: no `PING` within `--ping-timeout`.
    PingTimeout,
    /// dazai: `SIGUSR1` graceful panic.
    PanicSignal,
    /// dazai: `SIGUSR2` hard panic.
    HardPanicSignal,
    /// dazai: `SIGTERM`/`SIGINT` clean shutdown (wipe, no kill).
    Shutdown,
    /// dazai: the heartbeat client sent `QUIT`.
    ClientQuit,
    /// motokano: the `--calls` budget was spent.
    CallsExhausted,
    /// motokano: the MCP client disconnected (stdin EOF).
    ClientDisconnect,
    /// motokano: the linked dazai daemon went away.
    DaemonLost,
}

impl Reason {
    /// Every variant.
    pub const ALL: &'static [Reason] = &[
        Reason::HeartbeatLost,
        Reason::PingTimeout,
        Reason::PanicSignal,
        Reason::HardPanicSignal,
        Reason::Shutdown,
        Reason::ClientQuit,
        Reason::CallsExhausted,
        Reason::ClientDisconnect,
        Reason::DaemonLost,
    ];

    /// Identifier written to `args.reason`.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::HeartbeatLost => "heartbeat_lost",
            Reason::PingTimeout => "ping_timeout",
            Reason::PanicSignal => "panic_signal",
            Reason::HardPanicSignal => "hard_panic_signal",
            Reason::Shutdown => "shutdown",
            Reason::ClientQuit => "client_quit",
            Reason::CallsExhausted => "calls_exhausted",
            Reason::ClientDisconnect => "client_disconnect",
            Reason::DaemonLost => "daemon_lost",
        }
    }
}

/// Who a kill step targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillTarget {
    /// dazai: every PID registered over the control socket.
    Registered,
    /// The emitting process itself (and, for dazai, its `--exec` child).
    SelfProcess,
}

impl KillTarget {
    /// Every variant.
    pub const ALL: &'static [KillTarget] = &[KillTarget::Registered, KillTarget::SelfProcess];

    /// Identifier written to `args.target`.
    pub fn as_str(self) -> &'static str {
        match self {
            KillTarget::Registered => "registered",
            KillTarget::SelfProcess => "self",
        }
    }
}

/// What kind of motokano tool was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadKind {
    /// A pre-loaded value held in a locked `SecretBuffer`.
    Static,
    /// An operator-configured command's stdout.
    Exec,
}

impl ReadKind {
    /// Every variant.
    pub const ALL: &'static [ReadKind] = &[ReadKind::Static, ReadKind::Exec];

    /// Identifier written to `args.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            ReadKind::Static => "static",
            ReadKind::Exec => "exec",
        }
    }
}

/// One lifecycle step. Deliberately carries no strings or bytes — see the
/// crate docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// The process started serving / watching.
    SessionStart {
        /// Whether a trigger performs a real `SIGKILL` (vs. dry-run / clean exit).
        armed: bool,
    },
    /// The process became armed.
    Arm {
        /// How.
        via: ArmVia,
    },
    /// A motokano tool call was answered (or refused after the exit fired).
    Read {
        /// 0-based position of the tool in the `--tool` list. The operator's
        /// tool name is not recorded.
        tool: u32,
        /// Tool kind.
        kind: ReadKind,
        /// Whether a value was returned to the caller.
        served: bool,
    },
    /// Secret buffers were zeroized.
    Wipe {
        /// Trigger.
        reason: Reason,
        /// How many buffers were wiped (a count, never a size).
        buffers: u32,
    },
    /// A kill step was executed (or, in dry-run, decided but not executed).
    Kill {
        /// Trigger.
        reason: Reason,
        /// Who.
        target: KillTarget,
        /// How many processes were signalled (or would have been).
        count: u32,
        /// `false` in dazai dry-run mode.
        executed: bool,
    },
    /// The process is exiting without a kill (clean shutdown / dry-run end).
    SessionEnd {
        /// Trigger.
        reason: Reason,
    },
}

impl Lifecycle {
    /// Korg `tool_name` for this step.
    pub fn tool_name(&self) -> &'static str {
        match self {
            Lifecycle::SessionStart { .. } => "ningen.session_start",
            Lifecycle::Arm { .. } => "ningen.arm",
            Lifecycle::Read { .. } => "ningen.read",
            Lifecycle::Wipe { .. } => "ningen.wipe",
            Lifecycle::Kill { .. } => "ningen.kill",
            Lifecycle::SessionEnd { .. } => "ningen.session_end",
        }
    }
}

/// A JSON scalar in a record. Strings are only ever enum identifiers,
/// [`CONTRACT`]/[`PRODUCER`], or the hex session id — none need escaping.
enum Scalar<'a> {
    Str(&'a str),
    Int(u64),
    Bool(bool),
}

fn push_object(out: &mut String, fields: &mut [(&str, Scalar<'_>)]) {
    // Sorted keys: records are byte-stable and already in Korg's canonical
    // key order (Korg still canonicalizes on ingest).
    fields.sort_by(|a, b| a.0.cmp(b.0));
    out.push('{');
    for (i, (key, value)) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "\"{key}\":");
        let _ = match value {
            Scalar::Str(s) => write!(out, "\"{s}\""),
            Scalar::Int(n) => write!(out, "{n}"),
            Scalar::Bool(b) => write!(out, "{b}"),
        };
    }
    out.push('}');
}

/// Render one record (newline-terminated) in Korg's append shape.
fn render(component: Component, session: &str, observed_unix_ms: u64, event: Lifecycle) -> String {
    let mut args: Vec<(&str, Scalar<'_>)> = vec![
        ("component", Scalar::Str(component.as_str())),
        ("contract", Scalar::Str(CONTRACT)),
        ("observed_unix_ms", Scalar::Int(observed_unix_ms)),
        ("producer", Scalar::Str(PRODUCER)),
        ("session", Scalar::Str(session)),
    ];
    let mut success = true;
    let mut result: Vec<(&str, Scalar<'_>)> = match event {
        Lifecycle::SessionStart { armed } => {
            args.push(("armed", Scalar::Bool(armed)));
            vec![("success", Scalar::Bool(true))]
        }
        Lifecycle::Arm { via } => {
            args.push(("via", Scalar::Str(via.as_str())));
            vec![("success", Scalar::Bool(true))]
        }
        Lifecycle::Read { tool, kind, served } => {
            args.push(("tool", Scalar::Int(u64::from(tool))));
            args.push(("kind", Scalar::Str(kind.as_str())));
            success = served;
            vec![("served", Scalar::Bool(served))]
        }
        Lifecycle::Wipe { reason, buffers } => {
            args.push(("reason", Scalar::Str(reason.as_str())));
            args.push(("buffers", Scalar::Int(u64::from(buffers))));
            vec![("success", Scalar::Bool(true))]
        }
        Lifecycle::Kill {
            reason,
            target,
            count,
            executed,
        } => {
            args.push(("reason", Scalar::Str(reason.as_str())));
            args.push(("target", Scalar::Str(target.as_str())));
            args.push(("count", Scalar::Int(u64::from(count))));
            vec![("executed", Scalar::Bool(executed))]
        }
        Lifecycle::SessionEnd { reason } => {
            args.push(("reason", Scalar::Str(reason.as_str())));
            vec![("success", Scalar::Bool(true))]
        }
    };

    let mut out = String::with_capacity(MAX_RECORD_BYTES);
    out.push_str("{\"args\":");
    push_object(&mut out, &mut args);
    out.push_str(
        ",\"duration_ms\":0,\"event_type\":\"AgentToolCall\",\"payload_refs\":[],\"result\":",
    );
    push_object(&mut out, &mut result);
    let _ = writeln!(
        out,
        ",\"source_agent\":\"{}\",\"success\":{success},\"tool_name\":\"{}\"}}",
        component.source_agent(),
        event.tool_name()
    );
    out
}

/// Milliseconds since the Unix epoch, clamped into Korg's integer domain
/// (±(2^53−1), spec §7). A clock before 1970 reads as 0.
fn now_unix_ms() -> u64 {
    const MAX_SAFE: u64 = (1 << 53) - 1;
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| {
            u64::try_from(d.as_millis())
                .unwrap_or(MAX_SAFE)
                .min(MAX_SAFE)
        })
        .unwrap_or(0)
}

/// 128 bits from the OS RNG as lowercase hex.
fn random_session_id() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut hex = String::with_capacity(32);
    for b in bytes {
        let _ = write!(hex, "{b:02x}");
    }
    Ok(hex)
}

/// An append-only, pre-opened lifecycle record sink.
///
/// `Send + Sync`: control-connection threads and the main loop may emit
/// concurrently; each record is one `write(2)` on an `O_APPEND` descriptor, so
/// records never interleave.
pub struct Sink {
    file: File,
    component: Component,
    session: String,
    dropped: AtomicU64,
}

impl Sink {
    /// Open (creating `0600` if absent) the sink at `path` for appending.
    ///
    /// Call before seccomp is applied. `path` may be a regular file, a FIFO
    /// with a reader already attached, or `/dev/fd/N` for a descriptor the
    /// parent pre-opened. A FIFO with no reader fails here (`ENXIO`) rather
    /// than blocking.
    pub fn open(path: &Path, component: Component) -> io::Result<Sink> {
        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        Ok(Sink {
            file,
            component,
            session: random_session_id()?,
            dropped: AtomicU64::new(0),
        })
    }

    /// Append one record. Best-effort: never blocks on a full pipe, never
    /// panics, never fails the caller — the kill path must not depend on
    /// Korg. A record that cannot be written is counted in [`Sink::dropped`].
    pub fn emit(&self, event: Lifecycle) {
        let line = render(self.component, &self.session, now_unix_ms(), event);
        debug_assert!(line.len() <= MAX_RECORD_BYTES);
        if (&self.file).write_all(line.as_bytes()).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Records that could not be written (full pipe, closed reader, disk full).
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// The random per-process session id carried in `args.session`.
    pub fn session(&self) -> &str {
        &self.session
    }
}

/// Emit through an optional sink (the flag is opt-in everywhere).
pub fn emit(sink: Option<&Sink>, event: Lifecycle) {
    if let Some(sink) = sink {
        sink.emit(event);
    }
}
