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

// ---- Korg lifecycle contract (`--korg-sink`) -------------------------------

/// Read the sink, validate every record against the contract, and render each
/// as a compact step string.
fn sink_steps(path: &Path) -> (Vec<String>, String) {
    let text = std::fs::read_to_string(path).expect("read sink");
    let mut steps = Vec::new();
    for line in text.split_inclusive('\n') {
        kiroku::contract::validate(line).unwrap_or_else(|e| panic!("{e}: {line}"));
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let (a, r) = (&v["args"], &v["result"]);
        let s = |k: &str| a[k].as_str().unwrap().to_string();
        steps.push(match v["tool_name"].as_str().unwrap() {
            "ningen.session_start" => format!("session_start armed={}", a["armed"]),
            "ningen.arm" => format!("arm via={}", s("via")),
            "ningen.wipe" => format!("wipe {} buffers={}", s("reason"), a["buffers"]),
            "ningen.kill" => format!(
                "kill {} {} count={} executed={}",
                s("target"),
                s("reason"),
                a["count"],
                r["executed"]
            ),
            "ningen.session_end" => format!("session_end {}", s("reason")),
            other => panic!("dazai must not emit {other}"),
        });
    }
    (steps, text)
}

fn sessions(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = raw
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["args"]["session"].as_str().unwrap().to_string()
        })
        .collect();
    out.dedup();
    out
}

/// What dazai holds or is configured with; none of it may reach the sink in
/// any encoding or digest.
fn assert_dazai_no_leak(raw: &str, extra: &[&str]) {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let mut secrets = vec!["synthetic-key-material", "SYNTHETIC SECRET", "dazai-it-"];
    secrets.extend_from_slice(extra);
    for secret in secrets {
        let hex: String = secret.bytes().map(|b| format!("{b:02x}")).collect();
        let digest: String = Sha256::digest(secret.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        for form in [secret.to_string(), hex, STANDARD.encode(secret), digest] {
            assert!(!raw.contains(&form), "sink leaked {form:?} (of {secret:?})");
        }
    }
}

fn sink_path(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let p = PathBuf::from(format!(
        "/tmp/dazai-korg-{tag}-{}-{}.jsonl",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn korg_sink_on_preopened_fd_records_armed_kill_sequence_under_seccomp() {
    // The parent opens fd 3; dazai only ever sees `/dev/fd/3`. Under
    // `--features seccomp` every record is written after the filter is live:
    // a denied syscall would end the daemon with SIGSYS, not SIGKILL.
    let sock = unique_socket();
    let sink = sink_path("fd");
    let env_secret = format!("ENV-CANARY-{}", std::process::id());
    let mut victim = spawn_victim();
    let mut daemon = Command::new("sh")
        .arg("-c")
        .arg(r#"exec "$@" 3>>"$KORG_SINK""#)
        .arg("sh")
        .arg(BIN)
        .args([
            "daemon",
            "--size",
            "256",
            "--korg-sink",
            "/dev/fd/3",
            "--socket",
        ])
        .arg(&sock)
        .env("KORG_SINK", &sink)
        .env("DAZAI_TEST_SECRET", &env_secret)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn dazai via sh");
    assert!(wait_for_socket(&sock));
    assert!(control_req(&sock, &format!("REGISTER pid={}", victim.id())).contains("OK"));
    assert!(control_req(&sock, "ARM").contains("OK"));
    let mut hb = connect(&sock);
    hb.write_all(b"HELLO 1\n").unwrap();
    assert!(recv(&mut hb).contains("WELCOME"));

    send_signal(daemon.id(), "USR2");
    let status = daemon.wait().unwrap();
    assert_eq!(
        status.signal(),
        Some(9),
        "self-SIGKILL, not SIGSYS: {status:?}"
    );
    assert_eq!(victim.wait().unwrap().signal(), Some(9));

    let (steps, raw) = sink_steps(&sink);
    assert_eq!(
        steps,
        [
            "session_start armed=false",
            "arm via=control",
            "kill registered hard_panic_signal count=1 executed=true",
            "wipe hard_panic_signal buffers=2",
            "kill self hard_panic_signal count=1 executed=true",
        ]
    );
    assert_dazai_no_leak(&raw, &[&env_secret, "/dev/fd/3"]);
    let _ = std::fs::remove_file(&sink);
    let _ = std::fs::remove_file(&sock);
}

#[test]
fn korg_sink_records_dry_run_trigger_then_clean_shutdown() {
    let sink = sink_path("dry");
    let sink_arg = sink.to_str().unwrap();

    // Run 1: dry-run SIGUSR1 with a registered holder -> records what it
    // WOULD have killed, kills nothing.
    let sock = unique_socket();
    let mut victim = spawn_victim();
    let mut daemon = spawn_daemon(&sock, &["--korg-sink", sink_arg]);
    assert!(wait_for_socket(&sock));
    assert!(control_req(&sock, &format!("REGISTER pid={}", victim.id())).contains("OK"));
    let mut hb = connect(&sock);
    hb.write_all(b"HELLO 1\n").unwrap();
    assert!(recv(&mut hb).contains("WELCOME"));
    send_signal(daemon.id(), "USR1");
    assert!(daemon.wait().unwrap().success());
    assert!(
        matches!(victim.try_wait(), Ok(None)),
        "dry-run kills nobody"
    );
    let _ = victim.kill();
    let _ = victim.wait();
    drop(hb);

    // Run 2 (same sink, appended): SIGTERM clean shutdown -> wipe, no kill.
    let sock2 = unique_socket();
    let mut daemon = spawn_daemon(&sock2, &["--korg-sink", sink_arg]);
    assert!(wait_for_socket(&sock2));
    let mut hb = connect(&sock2);
    hb.write_all(b"HELLO 1\n").unwrap();
    assert!(recv(&mut hb).contains("WELCOME"));
    send_signal(daemon.id(), "TERM");
    assert!(daemon.wait().unwrap().success());

    let (steps, raw) = sink_steps(&sink);
    assert_eq!(
        steps,
        [
            "session_start armed=false",
            "wipe panic_signal buffers=2",
            "kill registered panic_signal count=1 executed=false",
            "kill self panic_signal count=1 executed=false",
            "session_end panic_signal",
            "session_start armed=false",
            "wipe shutdown buffers=2",
            "session_end shutdown",
        ]
    );
    assert_eq!(sessions(&raw).len(), 2, "one random session id per run");
    assert_dazai_no_leak(&raw, &[]);
    let _ = std::fs::remove_file(&sink);
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_file(&sock2);
}
