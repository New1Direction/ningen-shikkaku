# the kill sequence

When liveness is lost or a panic signal arrives, the [`PanicController`](./components/kikka.md) runs a fixed, single-fire sequence. The order is deliberate: kill first, wipe second, die last.

## Armed, graceful panic

```admonish danger title="On any armed trigger"
1. **kill registered processes.** Every PID registered with the daemon is `SIGKILL`ed first, while the daemon is still alive to do it. A child spawned via `--exec` is killed the same way.
2. **wipe the secret buffers.** Each `SecretBuffer` is overwritten with a non-elidable wipe (`explicit_bzero` / `memset_s` / volatile loop).
3. **remove the pidfile**, then **`SIGKILL` self.** The daemon does not return from this.
```

Killing the holders *before* wiping means no registered process can read a half-wiped buffer; wiping *before* self-kill means the wipe is the last meaningful thing the process does.

## The grace window

When the daemon is **armed** (`--arm`) a *graceful* panic (heartbeat drop, `--ping-timeout`, `SIGUSR1`) starts a grace window of `--grace` seconds. The sequence fires when the window expires. A genuine heartbeat **reconnect** during the window cancels it — a transient blip does not destroy your secrets.

```admonish note title="Hard panic bypasses grace"
`SIGUSR2` is a *hard* panic: it skips the grace window entirely and runs the sequence immediately. Use it when you want destruction *now*, not in `--grace` seconds.
```

## Single-fire

The controller is gated by an atomic flag and a `compare_exchange`: no matter how many triggers race (a dropped socket *and* a signal *and* a timeout in the same instant), the sequence runs **exactly once**.

## Dry-run is the default

```admonish warning title="Nothing is killed until you --arm it"
Without `--arm`, the daemon runs in **dry-run**: it performs the wipe and logs `WOULD SIGKILL …` for every process it *would* have killed, but never actually sends a kill. This lets you rehearse the full trigger path — heartbeat loss, grace window, reconnect-cancel — with zero risk before arming for real.
```

## What can defeat it

The sequence is best-effort against paths where the wipe code never gets to run: an external `kill -9`, a kernel OOM kill, or power loss. On those paths nothing is zeroed, and the project never claims otherwise — see [honest limitations](./reference/limitations.md).
