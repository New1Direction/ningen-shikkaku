#![deny(unsafe_code)]
//! MCP server exposing the dazai daemon as tools any agent can use.
//!
//! This crate is a thin protocol adapter: it adds no mechanism. Tool calls are
//! relayed to the daemon's UNIX socket (status / register / unregister / arm)
//! or turned into signals to the daemon process (panic / hard-panic, via a
//! pidfile lookup).
//!
//! The protocol/signal logic lives in [`DazaiClient`], which is independent of
//! `rmcp` and fully testable against a mock daemon with an injected signal
//! sender. [`DazaiServer`] is the thin `rmcp` wrapper around it.

use std::future::Future; // referenced by the generated `#[tool]` code
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use signal_hook::consts::{SIGUSR1, SIGUSR2};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::Parameters;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};

/// A signal sender `(pid, signum) -> delivered?`. Injectable for tests.
pub type SignalFn = Arc<dyn Fn(u32, i32) -> bool + Send + Sync>;

/// `dazai_status` result.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct StatusOut {
    /// Whether the daemon is reachable and responding.
    pub alive: bool,
    /// Whether the daemon is armed for a real self-destruct.
    pub armed: bool,
    /// The armed graceful-panic grace window, in seconds.
    pub grace_seconds: u64,
    /// How many PIDs are registered for SIGKILL-on-trigger.
    pub registered_pids: u32,
}

impl StatusOut {
    fn dead() -> Self {
        StatusOut {
            alive: false,
            armed: false,
            grace_seconds: 0,
            registered_pids: 0,
        }
    }
}

/// `dazai_register` / `dazai_unregister` result.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ActionOut {
    /// Whether the daemon accepted the request.
    pub ok: bool,
    /// The daemon's raw reply (or an error description).
    pub message: String,
}

/// `dazai_arm` result.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ArmOut {
    /// Whether the daemon is armed after this call.
    pub armed: bool,
    /// The daemon's raw reply (or an error description).
    pub message: String,
}

/// `dazai_panic` / `dazai_hard_panic` result.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TriggerOut {
    /// Whether the panic signal was delivered to the daemon.
    pub triggered: bool,
}

/// A PID argument.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PidParam {
    /// The process id to register/unregister.
    pub pid: u32,
}

/// Talks to the dazai daemon: socket round-trips + signals via a pidfile.
#[derive(Clone)]
pub struct DazaiClient {
    socket: PathBuf,
    pidfile: PathBuf,
    signal_fn: SignalFn,
}

impl DazaiClient {
    /// Build a client for the daemon at `socket` (pidfile is `<socket>.pid`).
    pub fn new(socket: PathBuf) -> Self {
        let pidfile = socket.with_extension("pid");
        DazaiClient {
            socket,
            pidfile,
            signal_fn: Arc::new(dazai_secmem::send_signal),
        }
    }

    /// Override the pidfile path (tests) — builder style.
    pub fn with_pidfile(mut self, pidfile: PathBuf) -> Self {
        self.pidfile = pidfile;
        self
    }

    /// Override the signal sender (tests) — builder style.
    pub fn with_signal_fn(mut self, signal_fn: SignalFn) -> Self {
        self.signal_fn = signal_fn;
        self
    }

    /// Query daemon status. Returns `alive: false` on any connection failure —
    /// a dead daemon is a valid state, never an error.
    pub async fn status(&self) -> StatusOut {
        match daemon_request(&self.socket, "STATUS").await {
            Ok(line) => parse_status(&line),
            Err(_) => StatusOut::dead(),
        }
    }

    /// Register a PID for SIGKILL-on-trigger.
    pub async fn register(&self, pid: u32) -> ActionOut {
        self.action(&format!("REGISTER pid={pid}")).await
    }

    /// Unregister a PID (clean disconnect).
    pub async fn unregister(&self, pid: u32) -> ActionOut {
        self.action(&format!("UNREGISTER pid={pid}")).await
    }

    async fn action(&self, msg: &str) -> ActionOut {
        match daemon_request(&self.socket, msg).await {
            Ok(line) => ActionOut {
                ok: line == "OK",
                message: if line.is_empty() {
                    "no reply".to_string()
                } else {
                    line
                },
            },
            Err(e) => ActionOut {
                ok: false,
                message: format!("daemon unreachable: {e}"),
            },
        }
    }

    /// Arm the daemon for a real self-destruct.
    pub async fn arm(&self) -> ArmOut {
        match daemon_request(&self.socket, "ARM").await {
            Ok(line) => ArmOut {
                armed: line == "OK" || line == "ALREADY_ARMED",
                message: if line.is_empty() {
                    "no reply".to_string()
                } else {
                    line
                },
            },
            Err(e) => ArmOut {
                armed: false,
                message: format!("daemon unreachable: {e}"),
            },
        }
    }

    /// Signal the daemon to panic. `hard` => SIGUSR2 (bypass grace); otherwise
    /// SIGUSR1 (graceful). The daemon PID comes from the pidfile.
    ///
    /// Gated on a live `STATUS` round-trip first: a stale pidfile (left after a
    /// SIGKILL self-destruct or crash) must never make us signal a recycled,
    /// unrelated PID. If the daemon is unreachable there is nothing to panic.
    pub async fn panic(&self, hard: bool) -> TriggerOut {
        if !self.status().await.alive {
            return TriggerOut { triggered: false };
        }
        let signum = if hard { SIGUSR2 } else { SIGUSR1 };
        let triggered = match read_daemon_pid(&self.pidfile) {
            Some(pid) => (self.signal_fn)(pid, signum),
            None => false,
        };
        TriggerOut { triggered }
    }
}

/// Connect to the daemon socket, send `msg\n`, and return the first reply line
/// (trimmed). The whole round-trip (connect + write + read) is bounded by one
/// short timeout so a hung daemon can't block a tool.
async fn daemon_request(socket: &Path, msg: &str) -> std::io::Result<String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    tokio::time::timeout(Duration::from_secs(3), async {
        let stream = tokio::net::UnixStream::connect(socket).await?;
        let mut reader = BufReader::new(stream);
        reader.get_mut().write_all(msg.as_bytes()).await?;
        reader.get_mut().write_all(b"\n").await?;
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        Ok::<String, std::io::Error>(line.trim().to_string())
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "daemon request timeout"))?
}

/// Parse a `STATUS alive=1 armed=0 grace=5 registered=0` line. Unknown/missing
/// fields default to false/0.
fn parse_status(line: &str) -> StatusOut {
    let mut out = StatusOut::dead();
    for tok in line.split_whitespace() {
        if let Some(v) = tok.strip_prefix("alive=") {
            out.alive = v == "1";
        } else if let Some(v) = tok.strip_prefix("armed=") {
            out.armed = v == "1";
        } else if let Some(v) = tok.strip_prefix("grace=") {
            out.grace_seconds = v.parse().unwrap_or(0);
        } else if let Some(v) = tok.strip_prefix("registered=") {
            out.registered_pids = v.parse().unwrap_or(0);
        }
    }
    out
}

/// Read the daemon PID from its pidfile (first line). Returns `None` on a
/// missing or malformed pidfile (handled gracefully by callers).
fn read_daemon_pid(pidfile: &Path) -> Option<u32> {
    std::fs::read_to_string(pidfile)
        .ok()?
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

/// The MCP server: a thin `rmcp` wrapper exposing [`DazaiClient`] as tools.
#[derive(Clone)]
pub struct DazaiServer {
    client: DazaiClient,
    tool_router: ToolRouter<DazaiServer>,
}

impl DazaiServer {
    /// Build a server talking to the daemon at `socket`.
    pub fn new(socket: PathBuf) -> Self {
        DazaiServer {
            client: DazaiClient::new(socket),
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl DazaiServer {
    /// Report daemon liveness, arm state, grace window, and registered count.
    #[tool(
        description = "Report whether the dazai daemon is alive, armed, its grace window, and how many PIDs are registered. A dead daemon reports alive=false."
    )]
    async fn dazai_status(&self) -> Json<StatusOut> {
        Json(self.client.status().await)
    }

    /// Register a PID for SIGKILL-on-trigger.
    #[tool(
        description = "Register a PID for SIGKILL-on-trigger. Agents call this at startup with their own PID."
    )]
    async fn dazai_register(
        &self,
        Parameters(PidParam { pid }): Parameters<PidParam>,
    ) -> Json<ActionOut> {
        Json(self.client.register(pid).await)
    }

    /// Unregister a PID (clean disconnect).
    #[tool(description = "Unregister a previously registered PID (clean disconnect).")]
    async fn dazai_unregister(
        &self,
        Parameters(PidParam { pid }): Parameters<PidParam>,
    ) -> Json<ActionOut> {
        Json(self.client.unregister(pid).await)
    }

    /// Graceful panic (respects the grace window).
    #[tool(
        description = "Graceful panic: signal the daemon to wipe and SIGKILL after its grace window (a reconnect can still cancel)."
    )]
    async fn dazai_panic(&self) -> Json<TriggerOut> {
        Json(self.client.panic(false).await)
    }

    /// Hard panic (bypasses the grace window).
    #[tool(
        description = "Hard panic: signal the daemon to wipe and SIGKILL immediately, bypassing the grace window."
    )]
    async fn dazai_hard_panic(&self) -> Json<TriggerOut> {
        Json(self.client.panic(true).await)
    }

    /// Arm the daemon for a real self-destruct.
    #[tool(
        description = "Arm the daemon for a real self-destruct (dry-run -> armed). Already-armed is reported as armed=true."
    )]
    async fn dazai_arm(&self) -> Json<ArmOut> {
        Json(self.client.arm().await)
    }
}

#[tool_handler]
impl ServerHandler for DazaiServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Controls for the dazai session-bound dead-man's-switch daemon. \
                 Register your PID to be SIGKILLed if the operator's session dies."
                    .to_string(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

/// Serve the MCP server over stdio (the standard MCP transport) until the
/// client disconnects.
pub async fn serve_stdio(socket: PathBuf) -> anyhow::Result<()> {
    let service = DazaiServer::new(socket).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
