//! End-to-end integration tests: spawn the real `dazai` binary, drive it over
//! the socket and with signals, and assert on exit status / logs. These cover
//! the heartbeat, armed-kill, grace-cancel, single-client, ping-timeout, and
//! client-subcommand behaviors (the integration half of the Phase 1 suite).
//!
//! Signals are delivered via the `kill` command so the tests need no `unsafe`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_dazai");

fn unique_socket() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    // Use /tmp (short) to stay well under the AF_UNIX sun_path limit.
    PathBuf::from(format!("/tmp/dazai-it-{}-{}.sock", std::process::id(), n))
}

fn spawn_daemon(sock: &Path, extra: &[&str]) -> Child {
    Command::new(BIN)
        .arg("daemon")
        .arg("--socket")
        .arg(sock)
        .arg("--size")
        .arg("256")
        .args(extra)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn dazai daemon")
}

fn wait_for_socket(p: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if p.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn connect(sock: &Path) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(sock) {
            Ok(s) => return s,
            Err(e) => {
                if Instant::now() >= deadline {
                    panic!("connect to {} failed: {e}", sock.display());
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn recv(stream: &mut UnixStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut buf = [0u8; 128];
    let n = stream.read(&mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).to_string()
}

fn send_signal(pid: u32, sig: &str) {
    let status = Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(pid.to_string())
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{sig} {pid} failed");
}

fn drain_stderr(child: &mut Child) -> String {
    let mut s = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut s);
    }
    s
}

fn reap(child: &mut Child) {
    if let Ok(None) = child.try_wait() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// One-shot control request: connect, send `msg\n`, return the reply.
fn control_req(sock: &Path, msg: &str) -> String {
    let mut s = connect(sock);
    s.write_all(msg.as_bytes()).unwrap();
    s.write_all(b"\n").unwrap();
    recv(&mut s)
}

fn spawn_victim() -> Child {
    Command::new("/bin/sleep")
        .arg("60")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn victim sleep")
}

#[test]
fn register_status_unregister_protocol() {
    let sock = unique_socket();
    let mut daemon = spawn_daemon(&sock, &[]); // dry-run: never actually kills
    assert!(wait_for_socket(&sock));
    let mut victim = spawn_victim();
    let vpid = victim.id();

    assert!(control_req(&sock, "STATUS").contains("registered=0"));
    assert!(control_req(&sock, "REGISTER pid=0").contains("ERROR")); // invalid
    assert!(control_req(&sock, &format!("REGISTER pid={vpid}")).contains("OK"));
    assert!(control_req(&sock, "STATUS").contains("registered=1"));
    assert!(control_req(&sock, &format!("REGISTER pid={vpid}")).contains("OK")); // idempotent
    assert!(control_req(&sock, "STATUS").contains("registered=1"));
    assert!(control_req(&sock, &format!("UNREGISTER pid={vpid}")).contains("OK"));
    assert!(control_req(&sock, "STATUS").contains("registered=0"));

    let _ = victim.kill();
    let _ = victim.wait();
    reap(&mut daemon);
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn control_connection_coexists_with_heartbeat() {
    let sock = unique_socket();
    let mut daemon = spawn_daemon(&sock, &[]);
    assert!(wait_for_socket(&sock));
    // Heartbeat holds the single-client slot.
    let mut hb = connect(&sock);
    hb.write_all(b"HELLO 1\n").unwrap();
    assert!(recv(&mut hb).contains("WELCOME"));
    // A control connection is NOT refused while the heartbeat is held.
    assert!(control_req(&sock, "STATUS").contains("alive=1"));
    // But a second heartbeat IS refused.
    let mut hb2 = connect(&sock);
    hb2.write_all(b"HELLO 2\n").unwrap();
    assert!(recv(&mut hb2).contains("BUSY"));
    drop(hb);
    drop(hb2);
    reap(&mut daemon);
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn runtime_arm_via_control() {
    let sock = unique_socket();
    let mut daemon = spawn_daemon(&sock, &[]); // starts in dry-run
    assert!(wait_for_socket(&sock));
    assert!(control_req(&sock, "STATUS").contains("armed=0"));
    assert!(control_req(&sock, "ARM").contains("OK"));
    assert!(control_req(&sock, "STATUS").contains("armed=1"));
    assert!(control_req(&sock, "ARM").contains("ALREADY_ARMED"));
    // No heartbeat connected, so nothing triggers; just reap it.
    reap(&mut daemon);
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn registered_pid_is_sigkilled_on_armed_trigger() {
    let sock = unique_socket();
    let mut victim = spawn_victim();
    let vpid = victim.id();
    let mut daemon = spawn_daemon(&sock, &["--arm", "--grace", "1"]);
    assert!(wait_for_socket(&sock));
    assert!(control_req(&sock, &format!("REGISTER pid={vpid}")).contains("OK"));

    // Heartbeat, then drop -> 1s grace -> SIGKILL registered victim + self.
    let mut hb = connect(&sock);
    hb.write_all(b"HELLO 1\n").unwrap();
    let _ = recv(&mut hb);
    drop(hb);

    let dstatus = daemon.wait().unwrap();
    assert_eq!(
        dstatus.signal(),
        Some(9),
        "armed daemon should self-SIGKILL"
    );
    let vstatus = victim.wait().unwrap();
    assert_eq!(
        vstatus.signal(),
        Some(9),
        "registered victim must be SIGKILLed before the daemon dies"
    );
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn protocol_then_connection_drop_triggers_dryrun_exit() {
    let sock = unique_socket();
    let mut child = spawn_daemon(&sock, &[]);
    assert!(wait_for_socket(&sock), "daemon never bound socket");

    let mut c = connect(&sock);
    c.write_all(b"HELLO 1\n").unwrap();
    assert!(recv(&mut c).contains("WELCOME"));
    c.write_all(b"PING\n").unwrap();
    assert!(recv(&mut c).contains("PONG"));
    drop(c); // drop heartbeat -> dry-run wipe -> exit 0

    let status = child.wait().unwrap();
    assert!(status.success(), "dry-run should exit 0, got {status:?}");
    assert!(drain_stderr(&mut child).contains("WOULD SIGKILL"));
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn sigusr1_triggers_dryrun_exit() {
    let sock = unique_socket();
    let mut child = spawn_daemon(&sock, &[]);
    assert!(wait_for_socket(&sock));
    // Handshake proves the loop + signal thread are up.
    let mut c = connect(&sock);
    c.write_all(b"HELLO 1\n").unwrap();
    assert!(recv(&mut c).contains("WELCOME"));

    send_signal(child.id(), "USR1");
    let status = child.wait().unwrap();
    assert!(status.success());
    assert!(drain_stderr(&mut child).contains("WOULD SIGKILL"));
    drop(c);
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn sigusr2_hard_panic_dryrun_exit() {
    let sock = unique_socket();
    let mut child = spawn_daemon(&sock, &[]);
    assert!(wait_for_socket(&sock));
    let mut c = connect(&sock);
    c.write_all(b"HELLO 1\n").unwrap();
    assert!(recv(&mut c).contains("WELCOME"));

    send_signal(child.id(), "USR2");
    let status = child.wait().unwrap();
    assert!(status.success());
    let err = drain_stderr(&mut child);
    assert!(err.contains("HARD"), "expected HARD-panic log, got: {err}");
    assert!(err.contains("WOULD SIGKILL"));
    drop(c);
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn armed_grace_drop_really_sigkills() {
    let sock = unique_socket();
    let mut child = spawn_daemon(&sock, &["--arm", "--grace", "1"]);
    assert!(wait_for_socket(&sock));
    let mut c = connect(&sock);
    c.write_all(b"HELLO 1\n").unwrap();
    let _ = recv(&mut c);
    drop(c); // drop -> 1s grace -> real SIGKILL of the daemon itself

    let status = child.wait().unwrap();
    assert_eq!(
        status.signal(),
        Some(9),
        "armed daemon should die by SIGKILL"
    );
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn armed_with_child_reaps_and_self_sigkills() {
    // Exercises the child-reap path: the armed kill closure runs
    // child.kill()+child.wait() (waitpid -> wait4) before raise(SIGKILL).
    // On Linux + --features seccomp this is the regression test for the wait4
    // allowlist gap: without it the daemon dies by SIGSYS, not SIGKILL.
    let sock = unique_socket();
    let mut child = spawn_daemon(&sock, &["--arm", "--grace", "1", "--exec", "/bin/sleep"]);
    assert!(wait_for_socket(&sock));
    let mut c = connect(&sock);
    c.write_all(b"HELLO 1\n").unwrap();
    let _ = recv(&mut c);
    drop(c); // drop -> 1s grace -> kill child (reap via wait4) -> SIGKILL self

    let status = child.wait().unwrap();
    assert_eq!(
        status.signal(),
        Some(9),
        "armed daemon with a child should self-SIGKILL (not die by SIGSYS)"
    );
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn armed_reconnect_within_grace_cancels() {
    let sock = unique_socket();
    let mut child = spawn_daemon(&sock, &["--arm", "--grace", "3"]);
    assert!(wait_for_socket(&sock));

    let mut c1 = connect(&sock);
    c1.write_all(b"HELLO 1\n").unwrap();
    let _ = recv(&mut c1);
    drop(c1); // start the 3s grace countdown
    thread::sleep(Duration::from_millis(600));

    let mut c2 = connect(&sock); // reconnect well within grace -> cancel
    c2.write_all(b"HELLO 2\n").unwrap();
    assert!(recv(&mut c2).contains("WELCOME"));

    thread::sleep(Duration::from_millis(3200)); // outlive the original deadline
    assert!(
        matches!(child.try_wait(), Ok(None)),
        "daemon should have survived the cancelled panic"
    );

    send_signal(child.id(), "TERM"); // clean shutdown
    let status = child.wait().unwrap();
    assert!(status.success(), "clean shutdown should exit 0");
    assert!(drain_stderr(&mut child).contains("CANCELLED"));
    drop(c2);
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn second_client_is_refused_with_busy() {
    let sock = unique_socket();
    let mut child = spawn_daemon(&sock, &[]);
    assert!(wait_for_socket(&sock));

    let mut c1 = connect(&sock);
    c1.write_all(b"HELLO 1\n").unwrap();
    assert!(recv(&mut c1).contains("WELCOME"));

    let mut c2 = connect(&sock);
    c2.write_all(b"HELLO 2\n").unwrap();
    assert!(
        recv(&mut c2).contains("BUSY"),
        "second client must be refused"
    );

    // First heartbeat unaffected; daemon did not panic.
    c1.write_all(b"PING\n").unwrap();
    assert!(recv(&mut c1).contains("PONG"));
    assert!(matches!(child.try_wait(), Ok(None)));

    drop(c1);
    drop(c2);
    reap(&mut child);
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn ping_timeout_triggers_dryrun_exit() {
    let sock = unique_socket();
    let mut child = spawn_daemon(&sock, &["--ping-timeout", "1"]);
    assert!(wait_for_socket(&sock));
    let mut c = connect(&sock);
    c.write_all(b"HELLO 1\n").unwrap();
    assert!(recv(&mut c).contains("WELCOME"));
    // Stay silent: no PING within 1s -> dry-run wipe -> exit 0.

    let status = child.wait().unwrap();
    assert!(status.success());
    assert!(drain_stderr(&mut child).contains("ping deadline missed"));
    drop(c);
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn byte_trickle_does_not_defeat_ping_timeout() {
    // Regression: a client dribbling bytes without ever completing a PING line
    // must still trip the per-line ping deadline (not be reset per byte).
    let sock = unique_socket();
    let mut child = spawn_daemon(&sock, &["--ping-timeout", "1"]);
    assert!(wait_for_socket(&sock));
    let mut c = connect(&sock);
    c.write_all(b"HELLO 1\n").unwrap(); // completes a line -> resets deadline
    assert!(recv(&mut c).contains("WELCOME"));

    // Dribble one non-newline byte every 400ms for ~1.6s: each byte would reset
    // a per-read timeout, but must NOT reset the per-line ping deadline.
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(1600) && child.try_wait().unwrap().is_none() {
        let _ = c.write_all(b"x");
        thread::sleep(Duration::from_millis(400));
    }

    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "ping timeout should still fire -> dry-run exit 0"
    );
    assert!(drain_stderr(&mut child).contains("ping deadline missed"));
    drop(c);
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn invalid_duration_arg_errors_cleanly_without_panic() {
    // `--grace nan` must exit via the clean error path (code 1), not panic (101)
    // or run misconfigured.
    let sock = unique_socket();
    let status = Command::new(BIN)
        .arg("daemon")
        .arg("--socket")
        .arg(&sock)
        .arg("--grace")
        .arg("nan")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn");
    assert_eq!(
        status.code(),
        Some(1),
        "expected clean failure exit, not a panic"
    );
    assert!(!sock.exists());
}

#[test]
fn client_subcommand_drop_triggers_daemon() {
    let sock = unique_socket();
    let mut daemon = spawn_daemon(&sock, &[]);
    assert!(wait_for_socket(&sock));

    let mut client = Command::new(BIN)
        .arg("client")
        .arg("--interval")
        .arg("1")
        .arg("--socket")
        .arg(&sock)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn dazai client");

    thread::sleep(Duration::from_millis(800));
    // Kill the client -> connection drops -> dry-run wipe -> daemon exits 0.
    let _ = client.kill();
    let _ = client.wait();

    let status = daemon.wait().unwrap();
    assert!(status.success());
    assert!(drain_stderr(&mut daemon).contains("WOULD SIGKILL"));
    let _ = std::fs::remove_file(&sock);
}
