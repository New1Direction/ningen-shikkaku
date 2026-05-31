# how it works

Three moving parts: **secret memory** that is hard to leak, a **liveness channel** that detects session loss, and a **panic policy** that destroys everything when liveness is lost.

## 1. Secrets live in locked memory

Secret material is held in a [`goodnight::SecretBuffer`](./components/goodnight.md) — an anonymous `mmap` mapping that is:

- **`mlock`ed** — pinned into physical RAM, never written to swap;
- **`madvise(MADV_DONTDUMP)`** — excluded from core dumps (Linux);
- **move-only** — no `Clone`/`Copy`, so the bytes are never silently duplicated onto a GC heap;
- **explicitly wipeable** — overwritten with `explicit_bzero` (Linux) / `memset_s` (macOS) / a volatile loop, a wipe the optimizer is contractually forbidden to elide.

The process additionally raises `RLIMIT_MEMLOCK` and sets `prctl(PR_SET_DUMPABLE, 0)` (Linux) to disable core dumps and `ptrace` attachment.

## 2. A heartbeat tracks your session

The [daemon](./components/dazai.md) binds a `0600` UNIX socket and listens. A client (`dazai client`, run from your shell or SSH session) connects and holds the connection open, optionally sending `PING`. That connection *is* the liveness signal:

- the client process dies, the shell closes, the SSH session drops → the socket closes → **liveness lost**;
- with `--ping-timeout N`, a missing `PING` for `N` seconds → **liveness lost**.

A single heartbeat is honored; a second concurrent one is refused with `BUSY`. See the [socket protocol](./reference/protocol.md).

## 3. Loss of liveness triggers the panic

Liveness loss — or an explicit panic signal — runs the [kill sequence](./kill-sequence.md): kill every registered process, wipe the secret buffers, then `SIGKILL` self.

```admonish note title="Signals"
- `SIGUSR1` → graceful panic (honors the grace window when armed)
- `SIGUSR2` → **hard** panic (bypasses the grace window)
- `SIGTERM` / `SIGINT` → clean shutdown: wipe, but no kill
```

## 4. Confinement seals the daemon

On Linux, after binding the socket and allocating buffers — but **before** entering the event loop — the daemon installs a [seccomp allowlist](./components/kekkai.md). Any syscall outside the allowlist (`execve`, `open`, `connect`, `ptrace`, …) terminates the process via `KillProcess`. The confinement is applied last so the locked, non-dumpable, syscall-restricted state is the state the daemon spends its whole life in.

## Startup order matters

```admonish warning title="The order is a security property, not a style choice"
1. raise `RLIMIT_MEMLOCK`
2. `prctl(PR_SET_DUMPABLE, 0)` (Linux)
3. allocate + `mlock` the secret buffers
4. spawn the child process (if `--exec`)
5. bind the UNIX socket (`0600`) + write the pidfile (`0600`)
6. **apply seccomp** (Linux + `seccomp` feature)
7. enter the accept/event loop
```

Buffers are locked before anything can fault them to disk; seccomp is applied after every privileged setup syscall is done, so the allowlist can be as small as the steady-state loop needs.
