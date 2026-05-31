#![deny(unsafe_code)]
//! dazai-oneshot CLI — parse args, wire the server + counter + optional dazai
//! link, serve over stdio, and route every exit condition through the single
//! wipe+exit path.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};

use dazai_oneshot::counter::{CallCounter, CounterMode};
use dazai_oneshot::dazai::DazaiLink;
use dazai_oneshot::server::{serve_stdio, OneshotServer};
use dazai_oneshot::tools::{parse_tool_spec, ToolRegistry};
use dazai_oneshot::{wipe_and_exit, ExitCtx};

#[derive(Parser)]
#[command(
    name = "dazai-oneshot",
    version,
    about = "Self-immolating MCP server: serve N tool calls, then wipe and exit"
)]
struct Cli {
    /// Exit after N tool calls complete (default 1 unless --session is set alone).
    #[arg(long)]
    calls: Option<usize>,
    /// Exit when the client disconnects.
    #[arg(long)]
    session: bool,
    /// Register with a dazai daemon at this socket; die if the daemon dies.
    #[arg(long = "dazai-socket")]
    dazai_socket: Option<PathBuf>,
    /// Wipe with explicit_bzero + SIGKILL on exit (default: clean exit).
    #[arg(long)]
    arm: bool,
    /// Tool definition, e.g. 'name=get_key,kind=static,value=s3cr3t' (repeatable).
    #[arg(long = "tool", required = true)]
    tool: Vec<String>,
    /// MCP transport (stdio is the standard).
    #[arg(long, value_enum, default_value_t = Transport::Stdio)]
    transport: Transport,
    /// Wait N seconds after the final call before wipe+exit.
    #[arg(long, default_value_t = 0)]
    grace: u64,
}

#[derive(Clone, ValueEnum)]
enum Transport {
    Stdio,
}

/// Resolve the counter mode from the `--calls` / `--session` combination.
fn counter_mode(session: bool, calls: Option<usize>) -> CounterMode {
    match (session, calls) {
        (false, None) => CounterMode::Calls(1), // default
        (false, Some(n)) => CounterMode::Calls(n),
        (true, None) => CounterMode::Session,
        (true, Some(n)) => CounterMode::Either(n),
    }
}

fn run(cli: Cli) -> Result<()> {
    // 1. tools
    let mut defs = Vec::new();
    for spec in &cli.tool {
        defs.push(parse_tool_spec(spec).with_context(|| format!("parsing --tool {spec:?}"))?);
    }
    let tools = Arc::new(Mutex::new(ToolRegistry::new(defs)));

    // 2. counter
    let counter = Arc::new(CallCounter::new(counter_mode(cli.session, cli.calls)));

    // 3. optional dazai link (register now; monitor started after ExitCtx exists)
    let dazai = match &cli.dazai_socket {
        Some(path) => Some(Arc::new(DazaiLink::connect(path)?)),
        None => None,
    };

    // 4. shared exit context
    let exit = Arc::new(ExitCtx {
        tools: Arc::clone(&tools),
        dazai: dazai.clone(),
        arm: cli.arm,
        grace: Duration::from_secs(cli.grace),
        closed: Arc::new(AtomicBool::new(false)),
    });

    if let Some(d) = &dazai {
        d.spawn_monitor(Arc::clone(&exit));
    }

    // 5. serve over stdio on a tokio runtime
    let server = OneshotServer::new(Arc::clone(&exit), Arc::clone(&counter));
    let rt = tokio::runtime::Runtime::new().context("creating tokio runtime")?;
    rt.block_on(serve_stdio(server))
        .context("serving MCP over stdio")?;

    // 6. serve returned => the client disconnected. Exit through the one path.
    let _ = counter.on_disconnect();
    wipe_and_exit(&exit)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS, // unreachable: run ends in wipe_and_exit
        Err(e) => {
            eprintln!("dazai-oneshot: {e:#}");
            ExitCode::FAILURE
        }
    }
}
