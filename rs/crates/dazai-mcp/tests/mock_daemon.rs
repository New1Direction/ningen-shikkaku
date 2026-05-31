//! Tests the MCP client logic against a mock daemon (a tokio UnixListener
//! speaking the protocol) and an injected fake signal sender — no live daemon.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use dazai_mcp::{DazaiClient, SignalFn};
use signal_hook::consts::{SIGUSR1, SIGUSR2};

/// A mock daemon's per-line reply function.
type ReplyFn = Arc<dyn Fn(&str) -> String + Send + Sync>;
/// Recorded `(pid, signum)` signal-send calls.
type Calls = Arc<Mutex<Vec<(u32, i32)>>>;

fn unique_path(kind: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    PathBuf::from(format!(
        "/tmp/dazai-mcp-{kind}-{}-{}",
        std::process::id(),
        n
    ))
}

/// Spawn a mock daemon that replies to each line via `reply`. The listener is
/// bound *before* returning, so a client may connect immediately without a
/// bind-vs-connect race. Returns the socket path and the task handle.
async fn spawn_mock(reply: ReplyFn) -> (PathBuf, tokio::task::JoinHandle<()>) {
    let sock = unique_path("sock");
    let _ = std::fs::remove_file(&sock);
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let handle = tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => return,
            };
            let reply = Arc::clone(&reply);
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
                let mut r = BufReader::new(stream);
                let mut line = String::new();
                if r.read_line(&mut line).await.unwrap_or(0) > 0 {
                    let resp = reply(line.trim());
                    let _ = r.get_mut().write_all(resp.as_bytes()).await;
                    let _ = r.get_mut().write_all(b"\n").await;
                }
            });
        }
    });
    (sock, handle)
}

fn fixed(resp: &'static str) -> ReplyFn {
    Arc::new(move |_| resp.to_string())
}

#[tokio::test]
async fn status_reports_dead_when_socket_missing() {
    let client = DazaiClient::new(unique_path("nope.sock"));
    let s = client.status().await;
    assert!(!s.alive);
    assert_eq!(s.registered_pids, 0);
}

#[tokio::test]
async fn status_parses_mock_reply() {
    let (sock, h) = spawn_mock(fixed("STATUS alive=1 armed=1 grace=7 registered=3")).await;
    let client = DazaiClient::new(sock.clone());
    let s = client.status().await;
    assert!(s.alive);
    assert!(s.armed);
    assert_eq!(s.grace_seconds, 7);
    assert_eq!(s.registered_pids, 3);
    h.abort();
    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn register_happy_path_and_rejections() {
    let (sock, h) = spawn_mock(Arc::new(|line: &str| {
        if line.contains("pid=999") {
            "OK".into()
        } else if line.contains("pid=0") {
            "ERROR invalid pid".into()
        } else {
            "BUSY".into()
        }
    }))
    .await;
    let client = DazaiClient::new(sock.clone());
    assert!(client.register(999).await.ok); // OK
    assert!(!client.register(0).await.ok); // ERROR invalid pid
    assert!(!client.register(5).await.ok); // BUSY (at cap)
    assert!(client.register(0).await.message.contains("ERROR"));
    h.abort();
    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn unregister_ok() {
    let (sock, h) = spawn_mock(fixed("OK")).await;
    let client = DazaiClient::new(sock.clone());
    assert!(client.unregister(123).await.ok);
    h.abort();
    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn arm_ok_and_already_armed_both_report_armed() {
    let (sock1, h1) = spawn_mock(fixed("OK")).await;
    assert!(DazaiClient::new(sock1.clone()).arm().await.armed);
    h1.abort();
    let _ = std::fs::remove_file(&sock1);

    let (sock2, h2) = spawn_mock(fixed("ALREADY_ARMED")).await;
    let armed = DazaiClient::new(sock2.clone()).arm().await;
    assert!(armed.armed);
    assert!(armed.message.contains("ALREADY_ARMED"));
    h2.abort();
    let _ = std::fs::remove_file(&sock2);
}

#[tokio::test]
async fn arm_reports_unarmed_when_daemon_unreachable() {
    let client = DazaiClient::new(unique_path("nope.sock"));
    assert!(!client.arm().await.armed);
}

fn recording_signal_fn() -> (SignalFn, Calls) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let inner = Arc::clone(&calls);
    let f: SignalFn = Arc::new(move |pid, sig| {
        inner.lock().unwrap().push((pid, sig));
        true
    });
    (f, calls)
}

const STATUS_ALIVE: &str = "STATUS alive=1 armed=0 grace=0 registered=0";

#[tokio::test]
async fn panic_sends_sigusr1_to_pidfile_pid() {
    let (sock, h) = spawn_mock(fixed(STATUS_ALIVE)).await; // live daemon (gate passes)
    let pidfile = unique_path("pid");
    std::fs::write(&pidfile, "4242\n").unwrap();
    let (sig, calls) = recording_signal_fn();
    let client = DazaiClient::new(sock.clone())
        .with_pidfile(pidfile.clone())
        .with_signal_fn(sig);

    assert!(client.panic(false).await.triggered);
    assert_eq!(*calls.lock().unwrap(), vec![(4242, SIGUSR1)]);
    h.abort();
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_file(&pidfile);
}

#[tokio::test]
async fn hard_panic_sends_sigusr2() {
    let (sock, h) = spawn_mock(fixed(STATUS_ALIVE)).await;
    let pidfile = unique_path("pid");
    std::fs::write(&pidfile, "777\n").unwrap();
    let (sig, calls) = recording_signal_fn();
    let client = DazaiClient::new(sock.clone())
        .with_pidfile(pidfile.clone())
        .with_signal_fn(sig);

    assert!(client.panic(true).await.triggered);
    assert_eq!(*calls.lock().unwrap(), vec![(777, SIGUSR2)]);
    h.abort();
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_file(&pidfile);
}

#[tokio::test]
async fn panic_with_no_pidfile_does_not_signal() {
    let (sock, h) = spawn_mock(fixed(STATUS_ALIVE)).await; // daemon alive, but no pidfile
    let (sig, calls) = recording_signal_fn();
    let client = DazaiClient::new(sock.clone())
        .with_pidfile(unique_path("missing.pid"))
        .with_signal_fn(sig);

    assert!(!client.panic(false).await.triggered);
    assert!(calls.lock().unwrap().is_empty());
    h.abort();
    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn panic_with_dead_daemon_does_not_signal_stale_pid() {
    // No mock daemon -> socket unreachable -> status().alive == false. Even with
    // a (stale) pidfile present, panic must NOT signal — guarding against PID
    // reuse after a SIGKILL self-destruct.
    let pidfile = unique_path("pid");
    std::fs::write(&pidfile, "4242\n").unwrap();
    let (sig, calls) = recording_signal_fn();
    let client = DazaiClient::new(unique_path("dead.sock"))
        .with_pidfile(pidfile.clone())
        .with_signal_fn(sig);

    assert!(!client.panic(false).await.triggered);
    assert!(
        calls.lock().unwrap().is_empty(),
        "must not signal a stale PID when the daemon is gone"
    );
    let _ = std::fs::remove_file(&pidfile);
}
