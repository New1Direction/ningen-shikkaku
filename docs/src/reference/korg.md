# Korg lifecycle records

`dazai daemon` and `motokano` can append a **non-secret lifecycle record** for every arm, session, read, wipe and kill step to a sink that [Korg](https://github.com/New1Direction/korg) imports. The contract is `ningen-korg-lifecycle@1`, implemented by the `kiroku` crate. It is narrow on purpose. ningen-shikkaku reports *that* something happened, and Korg decides *where* it goes in a tamper-evident ledger.

```bash
dazai daemon --arm --korg-sink dazai-run.jsonl
motokano --calls 1 --tool 'name=get_key,kind=static,value=…' --korg-sink motokano-run.jsonl

# pre-opened descriptor: the parent opens it, the daemon never opens a path
dazai daemon --arm --korg-sink /dev/fd/3 3>>dazai-run.jsonl
```

The flag is opt-in. Without it, nothing is written.

## Getting the records into Korg

Korg takes outside evidence only through its allowlisted import envelopes
([`korg-import@v1`](https://github.com/New1Direction/korg/blob/main/spec/korg-ledger-v1/IMPORT.md)).
The profile for this sink is `ningen-shikkaku.lifecycle@v2`. Once the process
has exited:

```bash
# from a Korg checkout (korg-ledger-py)
python -m korg_ledger envelope ningen-sink dazai-run.jsonl > dazai-run.envelope.json
python -m korg_ledger import dazai-run.envelope.json --ledger .korg/ledger.jsonl
python -m korg_ledger lint .korg/ledger.jsonl    # chain + causal DAG + redaction
```

Korg rechecks every record against the profile's allowlist and its redaction
scanner. If any record fails, the whole envelope is rejected and nothing is
appended. Imported events appear as `ningen-shikkaku.<step>` under
`source_agent = "import:ningen-shikkaku@<version>"`, hanging off one
`korg.import` batch event that records the envelope's SHA-256. Use one sink file
per run, or rotate it, so a later import doesn't repeat records.

## Who owns what

| ningen-shikkaku (producer) | Korg (ledger) |
|---|---|
| one JSON line per lifecycle step, in Korg's **append shape** | `seq_id`, `prev_hash` / `entry_hash`, HLC `emitted_at`, `event_id`, `actor_id`, `recorded_at` |
| a random per-process `session` id and a wall-clock `observed_unix_ms` | per-event `event_sig` (Ed25519), external anchors, korgcert seals |
| nothing else | causal links (`triggered_by`): records are emitted as roots |

Records never contain `seq_id`, any hash, a signature or `triggered_by`. The validator in `kiroku::contract` rejects a record that carries any of them. Each line deserializes as Korg's public `korg_core::event::NormalizedEvent` (the `POST /api/agent/tool-call` body). The extra `event_type: "AgentToolCall"` that [agent_event_spec §1.2.1](https://github.com/New1Direction/korg/blob/main/agent_event_spec.md) asks for is ignored by that type. Korg's import tests run on real sink output from both binaries and check that the projection, import and lint all pass.

## Record shape

```json
{"args":{"component":"dazai","contract":"ningen-korg-lifecycle@1","count":1,"observed_unix_ms":1791903342673,
 "producer":"ningen-shikkaku@0.2.0","reason":"heartbeat_lost","session":"5e920c9280150ac7f2d2577346e1edbe",
 "target":"registered"},"duration_ms":0,"event_type":"AgentToolCall","payload_refs":[],
 "result":{"executed":true},"source_agent":"mcp:dazai","success":true,"tool_name":"ningen.kill"}
```

(wrapped here; on disk it is one line, keys sorted, ASCII, at most 512 bytes)

Every record carries `args.component` (`dazai` | `motokano`), `args.contract`, `args.producer`, `args.session` and `args.observed_unix_ms`. `source_agent` is `mcp:dazai` or `mcp:motokano` ([spec §1.1](https://github.com/New1Direction/korg/blob/main/agent_event_spec.md)). `duration_ms` is always `0` and `payload_refs` is always `[]`.

| `tool_name` | extra `args` | `result` | emitted by |
|---|---|---|---|
| `ningen.session_start` | `armed` | `{"success":true}` | both, once at startup (dazai: after seccomp is live) |
| `ningen.arm` | `via`: `startup` \| `control` | `{"success":true}` | `--arm`; dazai runtime `ARM` |
| `ningen.read` | `tool` (0-based index), `kind`: `static` \| `exec` | `{"served":bool}` | motokano, per tool call; `served:false` = refused after the exit fired, or `exec` failed |
| `ningen.wipe` | `reason`, `buffers` (a count) | `{"success":true}` | after the buffers are zeroized |
| `ningen.kill` | `reason`, `target`: `registered` \| `self`, `count` | `{"executed":bool}` | armed kills, plus dazai dry-run "would kill" (`executed:false`) |
| `ningen.session_end` | `reason` | `{"success":true}` | clean exits (no kill) |

`reason` comes from a fixed vocabulary: `heartbeat_lost`, `ping_timeout`, `panic_signal` (SIGUSR1), `hard_panic_signal` (SIGUSR2), `shutdown` (SIGTERM/SIGINT), `client_quit`, `calls_exhausted`, `client_disconnect`, `daemon_lost`.

Records are written in the order the steps actually happen. An armed dazai trigger writes `kill registered` → `wipe` → `kill self`. The `kill self` record is written before `raise(SIGKILL)`.

## What is never recorded

```admonish danger title="Non-leakage is structural"
The event type (`kiroku::Lifecycle`) contains only enums, booleans and small counters. It has no `String`, byte or generic field, so the emitter cannot be handed any of the following:

- secret values, their **length**, or any **hash / encoding** of them (a digest of a low-entropy secret makes it guessable offline);
- tool **names**, raw MCP **arguments**, `exec` **command lines** or their stdout;
- **environment** variables, socket / sink / pidfile **paths**, PIDs, or free-form log and error text.
```

The tests enforce this from three directions:

- **Exhaustive contract test.** Every record the type can express (every variant × every enum value × boundary counters) is validated against a strict allowlist of keys and a fixed string vocabulary.
- **Leak scans.** These run against the real binaries. The sink is searched for canary secrets, tool names, command args, env values and paths, raw and as hex, base64 and SHA-256, under the live seccomp filter for dazai.
- **Noninterference.** Two motokano runs with different secret values and lengths, tool names, command args, environment and sink path must produce byte-identical records once `session` and the clock are masked. The two runs must also have different `session` ids, so nothing in a record depends on a secret.

## Daemon and seccomp constraints

- The sink is opened, and the session id read from `/dev/urandom`, **before** seccomp is applied: `openat` is denied afterwards. From then on, each record costs one `write(2)` on the existing descriptor, and the filter allows that.
- The descriptor is opened `O_APPEND | O_NONBLOCK | O_CLOEXEC`, created `0600` if it doesn't exist. Records are at most 512 bytes (the POSIX `PIPE_BUF` minimum), so one write to a pipe is atomic.
- **The kill path never waits on Korg.** If a pipe's reader stalls, records are dropped (and counted), never half-written, and the wipe/kill continues. If the sink is a FIFO with no reader, startup fails instead of hanging. If the sink can't be opened, the process refuses to start before it loads any secret.
- `/dev/fd/N` writes to a descriptor the parent pre-opened. That descriptor can be a file or a pipe into a process that collects the records for import.

## Honest limits

- Records are **best-effort telemetry**: a hard crash (OOM, external `kill -9`, power loss) can end the process before a step is recorded, the same way it skips the wipe.
- The sink file itself is not tamper-evident. Integrity starts when Korg imports and chains the records. A same-UID process could edit or truncate the file before then.
- `observed_unix_ms` is the local wall clock (Korg's HLC remains the ordering authority), and `session` only correlates records from one process run.
- `dazai mcp` (rei) only relays to the daemon. The daemon writes the records.
