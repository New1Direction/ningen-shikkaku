//! `ningen-korg-lifecycle@1` contract tests: every possible record is valid
//! and contains only allowlisted, non-secret fields; the validator rejects
//! drift (Korg-owned fields, free-form strings, oversize/non-ASCII lines); and
//! the sink never blocks on a stalled reader.

use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use kiroku::{
    contract, ArmVia, Component, KillTarget, Lifecycle, ReadKind, Reason, Sink, MAX_RECORD_BYTES,
};
use serde_json::Value;

fn scratch_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kiroku-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Compile-time exhaustiveness guards: adding a variant without listing it in
/// `ALL` (and so in the exhaustive emission below) breaks these matches.
fn listed_reason(r: Reason) -> &'static str {
    match r {
        Reason::HeartbeatLost => "heartbeat_lost",
        Reason::PingTimeout => "ping_timeout",
        Reason::PanicSignal => "panic_signal",
        Reason::HardPanicSignal => "hard_panic_signal",
        Reason::Shutdown => "shutdown",
        Reason::ClientQuit => "client_quit",
        Reason::CallsExhausted => "calls_exhausted",
        Reason::ClientDisconnect => "client_disconnect",
        Reason::DaemonLost => "daemon_lost",
    }
}
fn listed_target(t: KillTarget) -> &'static str {
    match t {
        KillTarget::Registered => "registered",
        KillTarget::SelfProcess => "self",
    }
}
fn listed_kind(k: ReadKind) -> &'static str {
    match k {
        ReadKind::Static => "static",
        ReadKind::Exec => "exec",
    }
}
fn listed_via(v: ArmVia) -> &'static str {
    match v {
        ArmVia::Startup => "startup",
        ArmVia::Control => "control",
    }
}
fn listed_agent(c: Component) -> &'static str {
    match c {
        Component::Dazai => "mcp:dazai",
        Component::Motokano => "mcp:motokano",
    }
}

/// Every lifecycle step the type can express, with boundary counters.
fn every_event() -> Vec<Lifecycle> {
    assert_eq!(Reason::ALL.len(), 9);
    assert_eq!(KillTarget::ALL.len(), 2);
    assert_eq!(ReadKind::ALL.len(), 2);
    assert_eq!(ArmVia::ALL.len(), 2);
    let counts = [0u32, 1, u32::MAX];
    let mut out = Vec::new();
    for armed in [false, true] {
        out.push(Lifecycle::SessionStart { armed });
    }
    for &via in ArmVia::ALL {
        out.push(Lifecycle::Arm { via });
    }
    for tool in counts {
        for &kind in ReadKind::ALL {
            for served in [false, true] {
                out.push(Lifecycle::Read { tool, kind, served });
            }
        }
    }
    for &reason in Reason::ALL {
        out.push(Lifecycle::SessionEnd { reason });
        for buffers in counts {
            out.push(Lifecycle::Wipe { reason, buffers });
        }
        for &target in KillTarget::ALL {
            for count in counts {
                for executed in [false, true] {
                    out.push(Lifecycle::Kill {
                        reason,
                        target,
                        count,
                        executed,
                    });
                }
            }
        }
    }
    out
}

/// What each step must put in `args`/`result`, derived from the contract
/// table rather than from the emitter.
fn expected_fields(ev: &Lifecycle) -> (&'static str, Vec<(&'static str, Value)>, Value) {
    use serde_json::json;
    match *ev {
        Lifecycle::SessionStart { armed } => (
            "ningen.session_start",
            vec![("armed", json!(armed))],
            json!({"success": true}),
        ),
        Lifecycle::Arm { via } => (
            "ningen.arm",
            vec![("via", json!(listed_via(via)))],
            json!({"success": true}),
        ),
        Lifecycle::Read { tool, kind, served } => (
            "ningen.read",
            vec![("tool", json!(tool)), ("kind", json!(listed_kind(kind)))],
            json!({"served": served}),
        ),
        Lifecycle::Wipe { reason, buffers } => (
            "ningen.wipe",
            vec![
                ("reason", json!(listed_reason(reason))),
                ("buffers", json!(buffers)),
            ],
            json!({"success": true}),
        ),
        Lifecycle::Kill {
            reason,
            target,
            count,
            executed,
        } => (
            "ningen.kill",
            vec![
                ("reason", json!(listed_reason(reason))),
                ("target", json!(listed_target(target))),
                ("count", json!(count)),
            ],
            json!({"executed": executed}),
        ),
        Lifecycle::SessionEnd { reason } => (
            "ningen.session_end",
            vec![("reason", json!(listed_reason(reason)))],
            json!({"success": true}),
        ),
    }
}

#[test]
fn every_expressible_record_is_valid_and_carries_exactly_its_fields() {
    let events = every_event();
    for &component in Component::ALL {
        let dir = scratch_dir();
        let path = dir.join("lifecycle.jsonl");
        let sink = Sink::open(&path, component).unwrap();
        for ev in &events {
            sink.emit(*ev);
        }
        assert_eq!(sink.dropped(), 0);

        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        assert_eq!(lines.len(), events.len(), "one record per emit");

        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        for (line, ev) in lines.iter().zip(&events) {
            contract::validate(line).unwrap_or_else(|e| panic!("{ev:?}: {e}\n{line}"));

            let v: Value = serde_json::from_str(line).unwrap();
            let (tool_name, extra, result) = expected_fields(ev);
            assert_eq!(v["tool_name"], tool_name);
            assert_eq!(v["source_agent"], listed_agent(component));
            assert_eq!(v["event_type"], "AgentToolCall");
            assert_eq!(v["args"]["component"], component.as_str());
            assert_eq!(v["args"]["session"], sink.session());
            let ms = v["args"]["observed_unix_ms"].as_u64().unwrap();
            assert!(
                ms > 1_600_000_000_000 && ms <= before,
                "plausible wall clock: {ms}"
            );
            for (key, want) in &extra {
                assert_eq!(&v["args"][*key], want, "{ev:?} args.{key}");
            }
            assert_eq!(v["result"], result, "{ev:?} result");
            let want_success = !matches!(ev, Lifecycle::Read { served: false, .. });
            assert_eq!(v["success"], want_success, "{ev:?} success");
            assert_eq!(v["args"].as_object().unwrap().len(), 5 + extra.len());
        }
        let _ = fs::remove_dir_all(&dir);
    }
}

#[test]
fn wire_shape_is_pinned() {
    // Byte-exact record with the two per-run values (session, clock) masked.
    let dir = scratch_dir();
    let path = dir.join("pinned.jsonl");
    let sink = Sink::open(&path, Component::Dazai).unwrap();
    sink.emit(Lifecycle::Kill {
        reason: Reason::HardPanicSignal,
        target: KillTarget::Registered,
        count: 2,
        executed: true,
    });
    let line = fs::read_to_string(&path).unwrap();
    let v: Value = serde_json::from_str(&line).unwrap();
    let masked = line
        .replace(sink.session(), "S")
        .replace(&v["args"]["observed_unix_ms"].to_string(), "T");
    assert_eq!(
        masked,
        concat!(
            r#"{"args":{"component":"dazai","contract":"ningen-korg-lifecycle@1","count":2,"#,
            r#""observed_unix_ms":T,"producer":"ningen-shikkaku@"#,
            env!("CARGO_PKG_VERSION"),
            r#"","reason":"hard_panic_signal","session":"S","target":"registered"},"#,
            r#""duration_ms":0,"event_type":"AgentToolCall","payload_refs":[],"#,
            r#""result":{"executed":true},"source_agent":"mcp:dazai","success":true,"#,
            r#""tool_name":"ningen.kill"}"#,
            "\n"
        )
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn records_fit_an_atomic_pipe_write_even_at_maximum_width() {
    let dir = scratch_dir();
    let path = dir.join("wide.jsonl");
    let sink = Sink::open(&path, Component::Motokano).unwrap();
    let widest = Reason::ALL
        .iter()
        .copied()
        .max_by_key(|r| r.as_str().len())
        .unwrap();
    sink.emit(Lifecycle::Kill {
        reason: widest,
        target: KillTarget::Registered,
        count: u32::MAX,
        executed: false,
    });
    let line = fs::read_to_string(&path).unwrap();
    // observed_unix_ms can grow to 16 digits (2^53-1); today it is 13.
    assert!(line.len() + 3 <= MAX_RECORD_BYTES, "{} bytes", line.len());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn vocabulary_needs_no_json_escaping() {
    let mut words: Vec<&str> = vec![kiroku::CONTRACT, kiroku::PRODUCER];
    words.extend(Reason::ALL.iter().map(|r| r.as_str()));
    words.extend(KillTarget::ALL.iter().map(|t| t.as_str()));
    words.extend(ReadKind::ALL.iter().map(|k| k.as_str()));
    words.extend(ArmVia::ALL.iter().map(|v| v.as_str()));
    words.extend(Component::ALL.iter().map(|c| c.as_str()));
    words.extend(Component::ALL.iter().map(|c| c.source_agent()));
    for w in words {
        assert!(
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.:@-".contains(&b)),
            "{w:?}"
        );
    }
    // Korg actor convention (agent_event_spec §1.1).
    for c in Component::ALL {
        assert!(c.source_agent().starts_with("mcp:"));
    }
}

#[test]
fn session_ids_are_random_per_sink() {
    let dir = scratch_dir();
    let a = Sink::open(&dir.join("a"), Component::Dazai).unwrap();
    let b = Sink::open(&dir.join("b"), Component::Dazai).unwrap();
    assert_ne!(a.session(), b.session());
    for s in [a.session(), b.session()] {
        assert_eq!(s.len(), 32);
        assert!(s
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
    let _ = fs::remove_dir_all(&dir);
}

fn valid_line() -> String {
    let dir = scratch_dir();
    let path = dir.join("one.jsonl");
    Sink::open(&path, Component::Motokano)
        .unwrap()
        .emit(Lifecycle::Read {
            tool: 0,
            kind: ReadKind::Static,
            served: true,
        });
    let line = fs::read_to_string(&path).unwrap();
    let _ = fs::remove_dir_all(&dir);
    line
}

fn mutate(line: &str, f: impl FnOnce(&mut Value)) -> String {
    let mut v: Value = serde_json::from_str(line).unwrap();
    f(&mut v);
    format!("{}\n", serde_json::to_string(&v).unwrap())
}

#[test]
fn validator_rejects_contract_drift() {
    let good = valid_line();
    contract::validate(&good).unwrap();

    // Korg owns sequencing, hashing, signing and timestamps; a producer that
    // sends any of them is out of contract.
    for korg_owned in [
        "seq_id",
        "prev_hash",
        "entry_hash",
        "event_sig",
        "ledger_hash",
        "parent_hash",
        "event_id",
        "triggered_by",
        "timestamp",
        "metadata",
        "schema_version",
    ] {
        let bad = mutate(&good, |v| {
            v[korg_owned] = Value::from(1);
        });
        assert!(
            contract::validate(&bad).is_err(),
            "accepted top-level {korg_owned}"
        );
    }

    // Anything that could carry a secret, argument, environment or digest.
    for leaky in [
        "value", "secret", "name", "cmd", "argv", "env", "path", "sha256", "hash", "len",
    ] {
        let bad = mutate(&good, |v| {
            v["args"][leaky] = Value::from("x");
        });
        assert!(contract::validate(&bad).is_err(), "accepted args.{leaky}");
        let bad = mutate(&good, |v| {
            v["result"][leaky] = Value::from("x");
        });
        assert!(contract::validate(&bad).is_err(), "accepted result.{leaky}");
    }

    let cases: Vec<(&str, String)> = vec![
        (
            "free-form kind",
            mutate(&good, |v| v["args"]["kind"] = "s3cr3t".into()),
        ),
        (
            "unknown tool_name",
            mutate(&good, |v| v["tool_name"] = "get_key".into()),
        ),
        (
            "foreign agent",
            mutate(&good, |v| v["source_agent"] = "agent:x@1".into()),
        ),
        (
            "component/agent mismatch",
            mutate(&good, |v| v["args"]["component"] = "dazai".into()),
        ),
        (
            "digest-length session",
            mutate(&good, |v| v["args"]["session"] = "ab".repeat(32).into()),
        ),
        (
            "uppercase session",
            mutate(&good, |v| v["args"]["session"] = "AB".repeat(16).into()),
        ),
        (
            "nonzero duration",
            mutate(&good, |v| v["duration_ms"] = 5.into()),
        ),
        (
            "payload ref",
            mutate(&good, |v| {
                v["payload_refs"] = serde_json::json!([{"sha256": "00"}])
            }),
        ),
        (
            "unsafe integer",
            mutate(&good, |v| {
                v["args"]["observed_unix_ms"] = (1u64 << 53).into()
            }),
        ),
        (
            "success != served",
            mutate(&good, |v| v["success"] = false.into()),
        ),
        ("missing newline", good.trim_end().to_string()),
        ("embedded newline", good.replacen(',', ",\n", 1)),
        ("non-ascii", good.replace("static", "stätic")),
        (
            "oversize",
            format!("{}{}\n", good.trim_end(), " ".repeat(MAX_RECORD_BYTES)),
        ),
    ];
    for (what, bad) in cases {
        assert!(contract::validate(&bad).is_err(), "accepted {what}: {bad}");
    }
}

#[test]
fn sink_file_is_created_owner_only_and_appended_not_truncated() {
    let dir = scratch_dir();
    let path = dir.join("new.jsonl");
    Sink::open(&path, Component::Dazai)
        .unwrap()
        .emit(Lifecycle::SessionStart { armed: false });
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    Sink::open(&path, Component::Dazai)
        .unwrap()
        .emit(Lifecycle::SessionStart { armed: true });
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 2, "second open must append");
    let _ = fs::remove_dir_all(&dir);
}

fn mkfifo(dir: &std::path::Path) -> PathBuf {
    let fifo = dir.join("korg.fifo");
    let ok = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success();
    assert!(ok, "mkfifo");
    fifo
}

#[test]
fn fifo_without_a_reader_fails_at_open_instead_of_blocking() {
    let dir = scratch_dir();
    let fifo = mkfifo(&dir);
    let started = Instant::now();
    assert!(Sink::open(&fifo, Component::Dazai).is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn stalled_reader_drops_whole_records_and_never_blocks() {
    let dir = scratch_dir();
    let fifo = mkfifo(&dir);
    let mut reader = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .unwrap();
    let sink = Sink::open(&fifo, Component::Dazai).unwrap();

    // Far more than any pipe buffer holds; the reader does not drain.
    let n = 20_000u64;
    let started = Instant::now();
    for _ in 0..n {
        sink.emit(Lifecycle::Kill {
            reason: Reason::HardPanicSignal,
            target: KillTarget::SelfProcess,
            count: 1,
            executed: true,
        });
    }
    assert!(started.elapsed() < Duration::from_secs(5), "emit blocked");
    assert!(sink.dropped() > 0, "a full pipe must drop, not block");

    let mut buf = Vec::new();
    let _ = reader.read_to_end(&mut buf);
    let text = String::from_utf8(buf).unwrap();
    let delivered = text.split_inclusive('\n').count() as u64;
    assert_eq!(
        delivered + sink.dropped(),
        n,
        "every record is delivered or counted"
    );
    for line in text.split_inclusive('\n') {
        contract::validate(line).unwrap_or_else(|e| panic!("torn record: {e}: {line:?}"));
    }
    let _ = fs::remove_dir_all(&dir);
}
