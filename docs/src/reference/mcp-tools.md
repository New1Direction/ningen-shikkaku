# MCP tools

The tools exposed by `dazai mcp` ([rei](../components/rei.md)). Each maps to one verb of the [socket protocol](./protocol.md) or to a panic signal. All run over the standard MCP stdio transport.

## `dazai_status()`

Round-trips `STATUS`. Never fails — a dead daemon is a valid, expected answer.

```json
{ "alive": true, "armed": true, "grace_seconds": 5, "registered_pids": 1 }
```

## `dazai_register(pid)`

Registers a PID for session-bound protection (`REGISTER pid=<pid>`). The PID is `SIGKILL`ed by the daemon on any armed trigger.

```json
{ "ok": true, "message": "registered" }
```

Returns `ok: false` if the PID is invalid or the registry is full (32).

## `dazai_unregister(pid)`

Drops a registration (`UNREGISTER pid=<pid>`).

```json
{ "ok": true, "message": "unregistered" }
```

## `dazai_arm()`

Arms the daemon at runtime (`ARM`). Idempotent — arming an already-armed daemon is fine.

```json
{ "armed": true, "message": "armed" }
```

## `dazai_panic()`

Sends `SIGUSR1` to the daemon — a **graceful** panic that honors the grace window when armed.

```json
{ "triggered": true }
```

## `dazai_hard_panic()`

Sends `SIGUSR2` to the daemon — a **hard** panic that bypasses the grace window and runs the [kill sequence](../kill-sequence.md) immediately.

```json
{ "triggered": true }
```

```admonish warning title="Panics are gated on a live daemon"
Before signaling, `rei` confirms the daemon is alive via `STATUS`. If the daemon has exited and its PID was recycled by the OS, the panic tools will not signal the recycled PID.
```
