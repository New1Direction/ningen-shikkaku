//! Tool registry: `static` values held in [`SecretBuffer`]s, and `exec` tools
//! that run an operator-configured command.
//!
//! The `--tool` spec is `key=value,...` with keys `name`, `kind`
//! (`static`|`exec`), and `value` (static) or `cmd` (exec). Values and commands
//! must not contain commas (the segment separator).

use anyhow::{anyhow, bail, Context, Result};
use goodnight::SecretBuffer;

/// One configured tool.
pub struct ToolDef {
    /// Tool name (the MCP tool id).
    pub name: String,
    /// Human-readable description shown in `tools/list`.
    pub description: String,
    kind: ToolKind,
}

enum ToolKind {
    /// A fixed value, held at rest in a locked, wipeable buffer.
    Static { buf: SecretBuffer, len: usize },
    /// A command line, run without a shell on each call.
    Exec(String),
}

/// What a tool call produces, computed without holding any lock across I/O.
pub enum Action {
    /// Return these bytes verbatim (copied out of the SecretBuffer).
    Static(Vec<u8>),
    /// Run this command line and return its stdout.
    Exec(String),
}

/// All configured tools. Wiped on exit.
pub struct ToolRegistry {
    tools: Vec<ToolDef>,
}

impl ToolRegistry {
    /// Build a registry from parsed tool definitions.
    pub fn new(tools: Vec<ToolDef>) -> Self {
        ToolRegistry { tools }
    }

    /// `(name, description)` for every tool, for building the MCP tool list.
    pub fn descriptors(&self) -> Vec<(String, String)> {
        self.tools
            .iter()
            .map(|t| (t.name.clone(), t.description.clone()))
            .collect()
    }

    /// Resolve a tool name to its [`Action`], copying any static value out from
    /// under no lock held across I/O. `None` if the tool is unknown.
    pub fn lookup(&self, name: &str) -> Option<Action> {
        let tool = self.tools.iter().find(|t| t.name == name)?;
        Some(match &tool.kind {
            ToolKind::Static { buf, len } => Action::Static(buf.as_slice()[..*len].to_vec()),
            ToolKind::Exec(cmd) => Action::Exec(cmd.clone()),
        })
    }

    /// Explicitly wipe every static value's SecretBuffer.
    pub fn wipe(&mut self) {
        for tool in &mut self.tools {
            if let ToolKind::Static { buf, .. } = &mut tool.kind {
                buf.wipe();
            }
        }
    }
}

/// Parse one `--tool 'name=...,kind=...,...'` spec into a [`ToolDef`].
pub fn parse_tool_spec(spec: &str) -> Result<ToolDef> {
    let mut name = None;
    let mut kind = None;
    let mut value = None;
    let mut cmd = None;
    let mut description = None;

    for segment in spec.split(',') {
        let (key, val) = segment
            .split_once('=')
            .ok_or_else(|| anyhow!("bad --tool segment {segment:?} (expected key=value)"))?;
        match key.trim() {
            "name" => name = Some(val.to_string()),
            "kind" => kind = Some(val.to_string()),
            "value" => value = Some(val.to_string()),
            "cmd" => cmd = Some(val.to_string()),
            "desc" | "description" => description = Some(val.to_string()),
            other => bail!("unknown --tool key {other:?}"),
        }
    }

    let name = name.ok_or_else(|| anyhow!("--tool is missing name"))?;
    let kind = kind.ok_or_else(|| anyhow!("--tool {name:?} is missing kind"))?;
    let description = description.unwrap_or_else(|| format!("motokano {kind} tool {name}"));

    let kind = match kind.as_str() {
        "static" => {
            let value = value.ok_or_else(|| anyhow!("static tool {name:?} is missing value"))?;
            let len = value.len();
            // SecretBuffer::new rejects 0; allocate at least one page-aligned byte.
            let mut buf = SecretBuffer::new(len.max(1))
                .with_context(|| format!("allocating SecretBuffer for tool {name:?}"))?;
            buf.write(value.as_bytes())
                .with_context(|| format!("storing value for tool {name:?}"))?;
            ToolKind::Static { buf, len }
        }
        "exec" => {
            let cmd = cmd.ok_or_else(|| anyhow!("exec tool {name:?} is missing cmd"))?;
            ToolKind::Exec(cmd)
        }
        other => bail!("unknown tool kind {other:?} (expected static or exec)"),
    };

    Ok(ToolDef {
        name,
        description,
        kind,
    })
}

/// Run an `exec` command line and return its stdout.
///
/// The command is split on whitespace and run **without a shell** — no quoting,
/// globbing, or expansion — so the configured command string is the only thing
/// executed (no injection from a caller). stdout is OS-buffered and is **not**
/// held in a SecretBuffer.
pub async fn run_command(cmd: &str) -> Result<String> {
    let mut parts = cmd.split_whitespace();
    let program = parts.next().ok_or_else(|| anyhow!("empty exec command"))?;
    let args: Vec<&str> = parts.collect();
    let output = tokio::process::Command::new(program)
        .args(&args)
        .output()
        .await
        .with_context(|| format!("executing {cmd:?}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
