//! Optional dazai daemon integration: register this process with a running
//! dazai daemon and self-destruct if that daemon dies.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::{wipe_and_exit, ExitCtx};

/// A link to a dazai daemon. Registers this process's PID and monitors the
/// daemon's liveness.
pub struct DazaiLink {
    socket_path: PathBuf,
    own_pid: u32,
    /// The registered connection, taken by [`DazaiLink::spawn_monitor`].
    monitor_stream: Mutex<Option<UnixStream>>,
}

impl DazaiLink {
    /// Connect to the daemon socket and `REGISTER` this process's PID. The
    /// liveness monitor is started separately via [`DazaiLink::spawn_monitor`]
    /// (it needs the exit context, which in turn references this link).
    pub fn connect(path: &Path) -> Result<Self> {
        let own_pid = std::process::id();
        let mut stream = UnixStream::connect(path)
            .with_context(|| format!("connecting to dazai socket {}", path.display()))?;
        stream
            .write_all(format!("REGISTER pid={own_pid}\n").as_bytes())
            .context("sending REGISTER to dazai")?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = [0u8; 64];
        let n = stream.read(&mut buf).context("reading REGISTER reply")?;
        let reply = String::from_utf8_lossy(&buf[..n]);
        if !reply.contains("OK") {
            bail!("dazai daemon refused registration: {}", reply.trim());
        }
        eprintln!("[motokano] registered pid {own_pid} with dazai daemon");
        Ok(DazaiLink {
            socket_path: path.to_path_buf(),
            own_pid,
            monitor_stream: Mutex::new(Some(stream)),
        })
    }

    /// Spawn the liveness monitor thread: PING every 5s, expect PONG; a socket
    /// close or a missed PONG triggers [`wipe_and_exit`], independently of the
    /// call counter (whichever fires first wins).
    pub fn spawn_monitor(&self, ctx: Arc<ExitCtx>) {
        let taken = self
            .monitor_stream
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        let Some(mut stream) = taken else {
            return; // already started
        };
        std::thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(6)));
            loop {
                std::thread::sleep(Duration::from_secs(5));
                if stream.write_all(b"PING\n").is_err() {
                    break; // socket closed: daemon gone
                }
                let mut buf = [0u8; 64];
                match stream.read(&mut buf) {
                    Ok(0) => break,    // EOF: daemon gone
                    Ok(_) => continue, // got a reply (PONG); keep watching
                    Err(_) => break,   // timeout (no PONG) or error: daemon gone
                }
            }
            wipe_and_exit(&ctx);
        });
    }

    /// Best-effort `UNREGISTER` on clean exit. Opens a fresh short connection so
    /// it never contends with the monitor's stream; failures are ignored.
    pub fn unregister(&self) {
        if let Ok(mut s) = UnixStream::connect(&self.socket_path) {
            let _ = s.write_all(format!("UNREGISTER pid={}\n", self.own_pid).as_bytes());
        }
    }
}
