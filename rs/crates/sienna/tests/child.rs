//! Tests for the parent-owns-kill-switch child wrapper.

use sienna::ChildProcess;
use std::path::Path;

#[test]
fn none_handle_has_no_pid_and_kill_is_noop() {
    let mut c = ChildProcess::none();
    assert_eq!(c.pid(), None);
    assert!(!c.is_running());
    c.kill(); // must not panic
}

#[test]
fn spawn_then_kill_stops_the_child() {
    // `sleep` exists on macOS and Linux; long enough that it is still running.
    let mut c = ChildProcess::spawn(Path::new("/bin/sleep"), &["30".to_string()]).unwrap();
    assert!(c.pid().is_some());
    assert!(c.is_running());
    c.kill();
    assert!(!c.is_running());
    c.kill(); // idempotent
}

#[test]
fn spawning_a_missing_executable_errors() {
    let res = ChildProcess::spawn(Path::new("/nonexistent/dazai-llm-xyz"), &[]);
    assert!(res.is_err());
}
