//! Lifecycle tests: spawn the real binary, drive real MCP tool calls over
//! stdio, and assert the process dies under each death condition.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_dazai-oneshot");

/// A spawned dazai-oneshot driven as an MCP client over stdio.
struct McpChild {
    child: Child,
    stdin: Option<ChildStdin>,
    reader: BufReader<ChildStdout>,
    id: i64,
}

impl McpChild {
    fn spawn(args: &[&str]) -> Self {
        let mut child = Command::new(BIN)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn dazai-oneshot");
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
