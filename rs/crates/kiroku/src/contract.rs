//! Strict validator for `ningen-korg-lifecycle@1` records.
//!
//! This is the contract written as code: a record is valid only if every key is
//! on the allowlist for its step, every string is drawn from the fixed
//! vocabulary (or is the 32-hex session id), and every number is a small
//! integer inside Korg's ±(2^53−1) domain. Anything else — an extra field, a
//! free-form string, a Korg-owned field such as `seq_id`/`entry_hash`/
//! `event_sig` — is rejected. A Korg-side ingester can use it to refuse
//! records that drift from the contract.

use serde_json::{Map, Value};

use crate::{
    ArmVia, Component, KillTarget, ReadKind, Reason, CONTRACT, MAX_RECORD_BYTES, PRODUCER,
};

const TOP_KEYS: &[&str] = &[
    "args",
    "duration_ms",
    "event_type",
    "payload_refs",
    "result",
    "source_agent",
    "success",
    "tool_name",
];

const COMMON_ARGS: &[&str] = &[
    "component",
    "contract",
    "observed_unix_ms",
    "producer",
    "session",
];

const MAX_SAFE_INT: u64 = (1 << 53) - 1;

fn ensure(cond: bool, msg: impl FnOnce() -> String) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(msg())
    }
}

fn exact_keys(obj: &Map<String, Value>, want: &[&str], what: &str) -> Result<(), String> {
    let mut have: Vec<&str> = obj.keys().map(String::as_str).collect();
    have.sort_unstable();
    let mut want: Vec<&str> = want.to_vec();
    want.sort_unstable();
    ensure(have == want, || {
        format!("{what} keys {have:?} != allowed {want:?}")
    })
}

fn str_in<'a>(obj: &'a Map<String, Value>, key: &str, vocab: &[&str]) -> Result<&'a str, String> {
    let s = obj
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be a string"))?;
    ensure(vocab.contains(&s), || {
        format!("{key}={s:?} is not in the vocabulary {vocab:?}")
    })?;
    Ok(s)
}

fn int_le(obj: &Map<String, Value>, key: &str, max: u64) -> Result<u64, String> {
    let n = obj
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{key} must be a non-negative integer"))?;
    ensure(n <= max, || format!("{key}={n} exceeds {max}"))?;
    Ok(n)
}

fn boolean(obj: &Map<String, Value>, key: &str) -> Result<bool, String> {
    obj.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{key} must be a boolean"))
}

fn names<T: Copy>(all: &[T], f: fn(T) -> &'static str) -> Vec<&'static str> {
    all.iter().map(|&v| f(v)).collect()
}

/// Validate one newline-terminated record. `Ok(())` iff it conforms exactly.
pub fn validate(line: &str) -> Result<(), String> {
    ensure(line.len() <= MAX_RECORD_BYTES, || {
        format!("record is {} bytes (max {MAX_RECORD_BYTES})", line.len())
    })?;
    ensure(line.is_ascii(), || "record is not pure ASCII".into())?;
    let body = line
        .strip_suffix('\n')
        .ok_or_else(|| "record is not newline-terminated".to_string())?;
    ensure(!body.contains(['\n', '\r']), || {
        "record has an embedded line break".into()
    })?;

    let value: Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    let top = value.as_object().ok_or("record is not an object")?;
    exact_keys(top, TOP_KEYS, "record")?;

    str_in(top, "event_type", &["AgentToolCall"])?;
    let agents = names(Component::ALL, Component::source_agent);
    let source_agent = str_in(top, "source_agent", &agents)?;
    let tool_name = str_in(
        top,
        "tool_name",
        &[
            "ningen.session_start",
            "ningen.arm",
            "ningen.read",
            "ningen.wipe",
            "ningen.kill",
            "ningen.session_end",
        ],
    )?;
    int_le(top, "duration_ms", 0)?;
    ensure(
        top.get("payload_refs") == Some(&Value::Array(vec![])),
        || "payload_refs must be []".into(),
    )?;
    let success = boolean(top, "success")?;

    let args = top
        .get("args")
        .and_then(Value::as_object)
        .ok_or("args must be an object")?;
    let result = top
        .get("result")
        .and_then(Value::as_object)
        .ok_or("result must be an object")?;

    // Common args.
    let component = str_in(args, "component", &names(Component::ALL, Component::as_str))?;
    let expected_agent = Component::ALL
        .iter()
        .find(|c| c.as_str() == component)
        .map(|c| c.source_agent());
    ensure(expected_agent == Some(source_agent), || {
        format!("component {component:?} does not match source_agent {source_agent:?}")
    })?;
    str_in(args, "contract", &[CONTRACT])?;
    str_in(args, "producer", &[PRODUCER])?;
    int_le(args, "observed_unix_ms", MAX_SAFE_INT)?;
    let session = args
        .get("session")
        .and_then(Value::as_str)
        .ok_or("session must be a string")?;
    ensure(
        session.len() == 32
            && session
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        || format!("session {session:?} is not 32 lowercase hex chars"),
    )?;

    let reasons = names(Reason::ALL, Reason::as_str);
    let (extra_args, result_keys): (&[&str], &[&str]) = match tool_name {
        "ningen.session_start" => {
            boolean(args, "armed")?;
            (&["armed"], &["success"])
        }
        "ningen.arm" => {
            str_in(args, "via", &names(ArmVia::ALL, ArmVia::as_str))?;
            (&["via"], &["success"])
        }
        "ningen.read" => {
            int_le(args, "tool", u64::from(u32::MAX))?;
            str_in(args, "kind", &names(ReadKind::ALL, ReadKind::as_str))?;
            let served = boolean(result, "served")?;
            ensure(served == success, || {
                "read: success must equal served".into()
            })?;
            (&["tool", "kind"], &["served"])
        }
        "ningen.wipe" => {
            str_in(args, "reason", &reasons)?;
            int_le(args, "buffers", u64::from(u32::MAX))?;
            (&["reason", "buffers"], &["success"])
        }
        "ningen.kill" => {
            str_in(args, "reason", &reasons)?;
            str_in(args, "target", &names(KillTarget::ALL, KillTarget::as_str))?;
            int_le(args, "count", u64::from(u32::MAX))?;
            boolean(result, "executed")?;
            ensure(success, || "kill: success must be true".into())?;
            (&["reason", "target", "count"], &["executed"])
        }
        "ningen.session_end" => {
            str_in(args, "reason", &reasons)?;
            (&["reason"], &["success"])
        }
        _ => unreachable!("tool_name was checked against the vocabulary"),
    };
    let mut allowed_args: Vec<&str> = COMMON_ARGS.to_vec();
    allowed_args.extend_from_slice(extra_args);
    exact_keys(args, &allowed_args, "args")?;
    exact_keys(result, result_keys, "result")?;
    if result_keys == ["success"] {
        ensure(
            result.get("success") == Some(&Value::Bool(true)) && success,
            || "non-read, non-kill steps always succeed".into(),
        )?;
    }
    Ok(())
}
