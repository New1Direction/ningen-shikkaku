//! Lifecycle tests: spawn the real binary, drive real MCP tool calls over
//! stdio, and assert the process dies under each death condition.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_motokano");

/// A spawned motokano driven as an MCP client over stdio.
struct McpChild {
    child: Child,
    stdin: Option<ChildStdin>,
    reader: BufReader<ChildStdout>,
    id: i64,
}

impl McpChild {
    fn spawn(args: &[&str]) -> Self {
        Self::spawn_env(args, &[])
    }

    fn spawn_env(args: &[&str], env: &[(&str, &str)]) -> Self {
        let mut child = Command::new(BIN)
            .args(args)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn motokano");
        let stdin = child.stdin.take().unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        let mut m = McpChild {
            child,
            stdin: Some(stdin),
            reader,
            id: 1,
        };
        m.initialize();
        m
    }

    fn send(&mut self, msg: &str) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{msg}").expect("write to child stdin");
        stdin.flush().expect("flush child stdin");
    }

    fn recv(&mut self) -> String {
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("read child stdout");
        line
    }

    fn initialize(&mut self) {
        self.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"lifecycle","version":"1"}}}"#);
        let _ = self.recv(); // initialize response
        self.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    }

    /// Call a tool and return the raw JSON-RPC response line.
    fn call(&mut self, name: &str) -> String {
        self.id += 1;
        let id = self.id;
        self.send(&format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}","arguments":{{}}}}}}"#
        ));
        self.recv()
    }

    fn close_stdin(&mut self) {
        self.stdin = None; // drop -> EOF to the child
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn wait_dead(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait().expect("try_wait") {
                Some(status) => return Some(status),
                None if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
                None => return None,
            }
        }
    }
}

impl Drop for McpChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const PING: &str = "name=ping,kind=static,value=pong";

#[test]
fn single_call_dies() {
    let mut m = McpChild::spawn(&["--calls", "1", "--tool", PING]);
    assert!(m.call("ping").contains("pong"));
    assert!(
        m.wait_dead(Duration::from_secs(5)).is_some(),
        "process should die after 1 call"
    );
}

#[test]
fn three_calls_dies_after_third() {
    let mut m = McpChild::spawn(&["--calls", "3", "--tool", PING]);
    assert!(m.call("ping").contains("pong"));
    assert!(m.alive(), "alive after 1/3");
    assert!(m.call("ping").contains("pong"));
    assert!(m.alive(), "alive after 2/3");
    assert!(m.call("ping").contains("pong"));
    assert!(
        m.wait_dead(Duration::from_secs(5)).is_some(),
        "dead after 3/3"
    );
}

#[test]
fn session_dies_on_disconnect() {
    let mut m = McpChild::spawn(&["--session", "--tool", PING]);
    assert!(m.alive(), "alive before disconnect");
    m.close_stdin();
    assert!(
        m.wait_dead(Duration::from_secs(5)).is_some(),
        "session server should die on disconnect"
    );
}

#[test]
fn grace_window() {
    let mut m = McpChild::spawn(&["--grace", "2", "--calls", "1", "--tool", PING]);
    m.call("ping");
    thread::sleep(Duration::from_secs(1));
    assert!(m.alive(), "should be alive 1s into a 2s grace");
    assert!(
        m.wait_dead(Duration::from_secs(4)).is_some(),
        "should be dead after the 2s grace"
    );
}

#[test]
fn arm_exits_by_signal() {
    let mut m = McpChild::spawn(&["--arm", "--calls", "1", "--tool", PING]);
    m.call("ping");
    let status = m
        .wait_dead(Duration::from_secs(5))
        .expect("armed should die");
    assert_eq!(
        status.signal(),
        Some(9),
        "armed exit must be SIGKILL, got {status:?}"
    );
}

#[test]
fn static_tool_returns_value() {
    let mut m = McpChild::spawn(&[
        "--calls",
        "1",
        "--tool",
        "name=get_key,kind=static,value=s3cr3t",
    ]);
    assert!(m.call("get_key").contains("s3cr3t"));
    let _ = m.wait_dead(Duration::from_secs(5));
}

#[test]
fn no_secret_served_after_exit_fires() {
    // --grace keeps the process alive after the budget is spent; a second call
    // during that window must be refused, not handed the un-wiped secret.
    let mut m = McpChild::spawn(&[
        "--calls",
        "1",
        "--grace",
        "3",
        "--tool",
        "name=get_key,kind=static,value=s3cr3t",
    ]);
    assert!(m.call("get_key").contains("s3cr3t"), "call 1 is served");
    assert!(m.alive(), "still alive during the grace window");
    let second = m.call("get_key");
    assert!(
        !second.contains("s3cr3t"),
        "secret must NOT be served after the exit fired: {second}"
    );
    let _ = m.wait_dead(Duration::from_secs(5));
}

#[test]
fn exec_tool_runs_command() {
    let mut m = McpChild::spawn(&[
        "--calls",
        "1",
        "--tool",
        "name=echo,kind=exec,cmd=echo hello",
    ]);
    assert!(m.call("echo").contains("hello"));
    let _ = m.wait_dead(Duration::from_secs(5));
}

// ---- Korg lifecycle contract (`--korg-sink`) -------------------------------

/// A per-run unique token, so a leak check can never pass by coincidence.
fn canary(tag: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!(
        "{tag}{}x{nanos:08x}x{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )
}

/// Encodings and digests a careless emitter might write instead of the raw
/// value — each would let a reader confirm a guess.
fn leak_forms(secret: &str) -> Vec<String> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD};
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let raw = secret.as_bytes();
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let digest = Sha256::digest(raw);
    vec![
        secret.to_string(),
        secret.chars().rev().collect(),
        hex(raw),
        hex(raw).to_uppercase(),
        STANDARD.encode(raw),
        STANDARD_NO_PAD.encode(raw),
        URL_SAFE_NO_PAD.encode(raw),
        hex(&digest),
        hex(&digest)[..16].to_string(),
        STANDARD_NO_PAD.encode(digest),
    ]
}

fn assert_no_leak(sink: &str, secrets: &[&str]) {
    for secret in secrets {
        for form in leak_forms(secret) {
            assert!(
                !sink.contains(&form),
                "sink leaked {form:?} (of {secret:?}):\n{sink}"
            );
        }
    }
}

/// Read the sink, validate every record against the contract, and return
/// the records with the two per-run values (session, clock) masked.
fn read_sink(path: &std::path::Path) -> (Vec<serde_json::Value>, String) {
    let text = std::fs::read_to_string(path).expect("read sink");
    let mut out = Vec::new();
    for line in text.split_inclusive('\n') {
        kiroku::contract::validate(line).unwrap_or_else(|e| panic!("{e}: {line}"));
        let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
        v["args"]["session"] = "S".into();
        v["args"]["observed_unix_ms"] = 0.into();
        out.push(v);
    }
    (out, text)
}

/// `tool_name` plus its step-specific fields, for compact sequence asserts.
fn step(v: &serde_json::Value) -> String {
    let a = &v["args"];
    let r = &v["result"];
    match v["tool_name"].as_str().unwrap() {
        "ningen.session_start" => format!("session_start armed={}", a["armed"]),
        "ningen.arm" => format!("arm via={}", a["via"].as_str().unwrap()),
        "ningen.read" => format!(
            "read tool={} kind={} served={}",
            a["tool"],
            a["kind"].as_str().unwrap(),
            r["served"]
        ),
        "ningen.wipe" => format!(
            "wipe {} buffers={}",
            a["reason"].as_str().unwrap(),
            a["buffers"]
        ),
        "ningen.kill" => format!(
            "kill {} {} count={} executed={}",
            a["target"].as_str().unwrap(),
            a["reason"].as_str().unwrap(),
            a["count"],
            r["executed"]
        ),
        "ningen.session_end" => format!("session_end {}", a["reason"].as_str().unwrap()),
        other => panic!("unexpected {other}"),
    }
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Run {
    records: Vec<serde_json::Value>,
    raw: String,
    secrets: Vec<String>,
}

/// Static read, exec read, an unknown-tool probe, then a refused re-read in
/// the grace window; unarmed, so the process exits cleanly.
fn run_unarmed_scenario(value: &str) -> Run {
    let static_name = canary("tok");
    let exec_name = canary("cmd");
    let exec_arg = canary("arg");
    let env_secret = canary("env");
    let unknown = canary("probe");
    let dir = scratch(&canary("motokano-korg-dir"));
    let sink = dir.join("lifecycle.jsonl");

    let static_spec = format!("name={static_name},kind=static,value={value}");
    let exec_spec = format!("name={exec_name},kind=exec,cmd=echo {exec_arg}");
    let mut m = McpChild::spawn_env(
        &[
            "--calls",
            "2",
            "--grace",
            "1",
            "--tool",
            &static_spec,
            "--tool",
            &exec_spec,
            "--korg-sink",
            sink.to_str().unwrap(),
        ],
        &[("MOTOKANO_TEST_SECRET", &env_secret)],
    );
    assert!(m.call(&static_name).contains(value));
    assert!(
        m.call(&unknown).contains("error"),
        "unknown tool is an error"
    );
    assert!(m.call(&exec_name).contains(&exec_arg));
    assert!(!m.call(&static_name).contains(value), "refused after exit");
    let status = m.wait_dead(Duration::from_secs(5)).expect("exits");
    assert!(status.success(), "unarmed exit is clean: {status:?}");

    let (records, raw) = read_sink(&sink);
    let _ = std::fs::remove_dir_all(&dir);
    Run {
        records,
        raw,
        secrets: vec![
            value.to_string(),
            static_name,
            exec_name,
            exec_arg,
            env_secret,
            unknown,
            dir.display().to_string(),
        ],
    }
}

#[test]
fn korg_sink_records_reads_wipe_and_exit_without_leaking() {
    let secret = canary("s3cr3t");
    let run = run_unarmed_scenario(&secret);
    let steps: Vec<String> = run.records.iter().map(step).collect();
    assert_eq!(
        steps,
        [
            "session_start armed=false",
            "read tool=0 kind=static served=true",
            "read tool=1 kind=exec served=true",
            "read tool=0 kind=static served=false",
            "wipe calls_exhausted buffers=1",
            "session_end calls_exhausted",
        ]
    );
    let secrets: Vec<&str> = run.secrets.iter().map(String::as_str).collect();
    assert_no_leak(&run.raw, &secrets);
}

#[test]
fn korg_records_are_independent_of_every_secret_and_input() {
    // Noninterference: different secret values (and lengths), tool names,
    // command arguments, environment and sink paths must yield byte-identical
    // records once the random session id and the clock are masked. Nothing in
    // the sink is a function of the secret, so nothing in it helps a guess.
    let a = run_unarmed_scenario("Zq");
    let b = run_unarmed_scenario(&canary("a-much-longer-and-different-secret-"));
    assert_eq!(a.records, b.records);

    let session = |raw: &str| {
        let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        v["args"]["session"].as_str().unwrap().to_string()
    };
    assert_ne!(
        session(&a.raw),
        session(&b.raw),
        "session id is per-run random"
    );
}

#[test]
fn korg_sink_records_armed_self_kill_before_dying() {
    let secret = canary("armed");
    let dir = scratch(&canary("motokano-korg-armed"));
    let sink = dir.join("lifecycle.jsonl");
    let spec = format!("name=get_key,kind=static,value={secret}");
    let mut m = McpChild::spawn(&[
        "--arm",
        "--calls",
        "1",
        "--tool",
        &spec,
        "--korg-sink",
        sink.to_str().unwrap(),
    ]);
    assert!(m.call("get_key").contains(&secret));
    let status = m.wait_dead(Duration::from_secs(5)).expect("armed dies");
    assert_eq!(status.signal(), Some(9), "{status:?}");

    let (records, raw) = read_sink(&sink);
    let steps: Vec<String> = records.iter().map(step).collect();
    assert_eq!(
        steps,
        [
            "session_start armed=true",
            "arm via=startup",
            "read tool=0 kind=static served=true",
            "wipe calls_exhausted buffers=1",
            "kill self calls_exhausted count=1 executed=true",
        ]
    );
    assert_no_leak(&raw, &[&secret, "get_key"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn korg_sink_that_cannot_be_opened_fails_before_serving() {
    let out = Command::new(BIN)
        .args([
            "--tool",
            PING,
            "--korg-sink",
            "/nonexistent-dir/korg/lifecycle.jsonl",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run motokano");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--korg-sink"));
}
