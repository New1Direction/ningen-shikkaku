#![deny(unsafe_code)]
//! LLM child-process wrapper for dazai.
//!
//! The parent spawns the LLM runtime as a child process (a fork+exec performed
//! by [`std::process::Command`]), retains the child's PID, and can kill it on
//! any panic trigger *before* the parent self-destructs — the
//! parent-owns-the-kill-switch pattern. The backend is a stub: any executable
//! path works for Phase 2; a real LLM runtime slots in without interface
//! changes.
//!
//! # Restricted file descriptors
//! `std::process::Command` marks every descriptor Rust opens as `CLOEXEC`, so
//! the child inherits only stdin/stdout/stderr (0/1/2) — no stray parent fds
//! leak into the LLM runtime unless explicitly configured.

use std::path::Path;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

/// A handle to the (optional) child LLM process.
pub struct ChildProcess {
    child: Option<Child>,
}

impl ChildProcess {
    /// Spawn `exec_path` with `args`, inheriting only stdin/stdout/stderr.
    pub fn spawn(exec_path: &Path, args: &[String]) -> Result<Self> {
        let child = Command::new(exec_path)
            .args(args)
            // Only 0/1/2 are inherited; all other parent fds are CLOEXEC.
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning child {}", exec_path.display()))?;
        eprintln!(
            "[sienna] spawned LLM child pid={} ({})",
            child.id(),
            exec_path.display()
        );
        Ok(ChildProcess { child: Some(child) })
    }

    /// A handle with no child attached (used when no `--exec` is given).
    pub fn none() -> Self {
        ChildProcess { child: None }
    }

    /// The child's PID, if a child is attached.
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    /// Whether the child is still running (reaps nothing).
    pub fn is_running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Kill the child with `SIGKILL` and reap it. Idempotent and safe to call
    /// when no child is attached.
    pub fn kill(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let pid = child.id();
            let _ = child.kill(); // SIGKILL on Unix
            let _ = child.wait(); // reap, avoid a zombie
            eprintln!("[sienna] killed child pid={pid}");
        }
        self.child = None;
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        // Clean-shutdown path also tears down the child.
        self.kill();
    }
}
