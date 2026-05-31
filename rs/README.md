# dazai Phase 2 — hardened Rust dead-man's-switch

Phase 2 is a Rust rewrite of the Phase 1 Python reference (`../`). The mechanism
is identical — a daemon holds secret material in page-locked RAM, keeps a UNIX
socket heartbeat, and on a trigger wipes the secrets and `SIGKILL`s itself — but
Rust lets it provide guarantees the CPython runtime cannot.

## What Phase 2 adds over Phase 1

| Concern | Phase 1 (Python) | Phase 2 (Rust) |
|---|---|---|
| Plaintext residue | CPython copies `bytes` freely; a secret may sit in unlocked heap before/after reaching the locked buffer (documented, unavoidable) | `SecretBuffer` owns its allocation; data is written **into** the locked mapping via borrow-checked slices and never copied into GC heap. Move-only (no `Clone`/`Copy`), so no silent duplication. |
| Wipe | `ctypes.memset` (can in principle be elided) | `explicit_bzero` (Linux) / `memset_s` (macOS) / volatile loop — **non-elidable** by contract |
| Core dumps | none | `madvise(MADV_DONTDUMP)` + `prctl(PR_SET_DUMPABLE, 0)` on Linux |
| Swap | `mlock` best-effort | `mlock` + raise `RLIMIT_MEMLOCK` toward `RLIM_INFINITY` |
| Syscall surface | full Python interpreter | **seccomp allowlist** (Linux): default `KillProcess`, only the needed syscalls permitted |
| Signal safety | self-pipe in Python handler | `signal-hook`'s async-signal-safe self-pipe; no work in handler context |
| Memory safety | n/a (Python) | all `unsafe` confined to one crate (`dazai-secmem`), every block justified; every other crate `#![deny(unsafe_code)]` |

**The CPython residue limitation is eliminated** for the buffers `dazai` owns:
secrets live only inside the locked, non-dumpable, explicitly-wiped mapping.

## Crates

```
dazai-secmem    SecretBuffer (mmap+mlock+madvise+wipe+Drop). The ONLY unsafe crate.
dazai-watchdog  PanicController (policy) + Watchdog (socket listener, signal thread,
                per-connection reader threads, event loop). unsafe-free.
dazai-child     LLM child wrapper: parent spawns (fork+exec via Command), owns the PID,
                kills the child on any trigger before self-destruct. unsafe-free.
dazai-seccomp   Linux seccomp allowlist (feature `seccomp`); no-op stub elsewhere.
dazai-mcp       MCP server (rmcp) exposing the daemon as tools any agent can use.
dazai-oneshot   Self-immolating MCP server: serve N tool calls, then wipe + exit.
dazai           CLI binary: `daemon`, `client`, and `mcp` subcommands.
```

## Usage

```bash
# rehearse safely (dry-run: wipes + logs WOULD-SIGKILL, never kills)
cargo run -p dazai -- daemon --ping-timeout 15
cargo run -p dazai -- client --interval 5
# Ctrl-C the client -> daemon wipes, logs WOULD SIGKILL, exits 0

# arm it for real (the daemon will actually SIGKILL itself on trigger)
cargo run -p dazai -- daemon --arm --grace 5 --ping-timeout 15 --exec /path/to/llm

# Linux, with seccomp confinement:
cargo run -p dazai --features seccomp -- daemon --arm
```

CLI: `dazai daemon [--arm] [--grace N] [--ping-timeout N] [--socket PATH] [--exec PATH] [--size BYTES]`
and `dazai client [--interval N] [--socket PATH]`. Default socket:
`${XDG_RUNTIME_DIR:-/tmp}/dazai-$UID.sock`, mode `0600`.

### Triggers (same model as Phase 1)

| Event | Behavior |
|---|---|
| heartbeat connection dropped | graceful panic |
| `--ping-timeout` deadline missed | graceful panic |
| `SIGUSR1` | graceful panic (grace window when armed) |
| `SIGUSR2` | **hard** panic: bypasses the grace window |
| `SIGTERM`/`SIGINT` | clean shutdown (wipe, no kill) |

A single heartbeat is honored; a second concurrent connection is refused with
`BUSY`. Only a reconnect *after* a real loss cancels an armed grace window.

## Phase 3: MCP server — session-bound protection for any agent

Phase 3 adds **zero new mechanism** — it is a thin MCP adapter over the daemon.
Any MCP client (an LLM agent, a recon swarm, a tool runner) registers its PID
and gets the same session-bound protection: when the operator's session dies,
dazai SIGKILLs every registered PID, then wipes and kills itself. No agent needs
to know how dazai launches; dazai needs to know nothing about any agent.

### Why MCP over `--exec`

`--exec` supervises **one** child dazai launches itself. MCP inverts that: agents
opt in by registering their own PID over a standard protocol, so protection is
**loosely coupled** (dazai and the agent launch independently), **stack-wide**
(any number of agents/tools register, capped at 32), and still preserves the
**hard-kill guarantee** — a registered PID is SIGKILLed by the daemon on trigger
exactly as an `--exec` child would be. `--exec` still works, unchanged; MCP is
purely additive.

### Run it

```bash
dazai daemon --arm --grace 5     # terminal 1: the daemon (writes <socket>.pid, 0600)
dazai mcp                        # terminal 2: the MCP server (stdio transport)
# point any MCP client at `dazai mcp`; the agent calls dazai_register(its pid)
```

`dazai mcp [--socket PATH] [--transport stdio]` — stdio is the standard MCP
transport. The MCP server relays tool calls to the daemon socket, and reads the
pidfile to signal the daemon for panic / hard-panic.

### Tools

| Tool | Effect | Returns |
|---|---|---|
| `dazai_status()` | `STATUS` round-trip (never fails; a dead daemon is valid) | `{alive, armed, grace_seconds, registered_pids}` |
| `dazai_register(pid)` | `REGISTER pid=<pid>` | `{ok, message}` |
| `dazai_unregister(pid)` | `UNREGISTER pid=<pid>` | `{ok, message}` |
| `dazai_arm()` | `ARM` (runtime arm) | `{armed, message}` |
| `dazai_panic()` | `SIGUSR1` to the daemon (graceful) | `{triggered}` |
| `dazai_hard_panic()` | `SIGUSR2` to the daemon (bypass grace) | `{triggered}` |

### Daemon control protocol (additive to the heartbeat protocol)

A connection whose first verb is **not** `HELLO` is a *control* connection —
request/response, any number, never touching the single-client heartbeat lock:

```
REGISTER pid=<N>    -> OK | BUSY (at 32) | ERROR invalid pid
UNREGISTER pid=<N>  -> OK
ARM                 -> OK (now armed) | ALREADY_ARMED
STATUS              -> STATUS alive=1 armed=<0|1> grace=<n> registered=<n>
```

PIDs are validated with `kill(pid,0)` (must be > 0 and exist) and stored in a
plain `Vec<u32>` — not sensitive, no `SecretBuffer`. On any **armed** trigger the
daemon SIGKILLs every registered PID *before* wiping its buffers and SIGKILLing
itself; dry-run only logs `WOULD` and never kills.

### End-to-end verification (manual)

```bash
# terminal 1 — armed daemon
dazai daemon --arm --grace 2

# terminal 2 — MCP server
dazai mcp

# terminal 3 — an MCP client connected to `dazai mcp`:
#   dazai_register(pid = <a process to protect>)
#   dazai_status()  -> { alive: true, armed: true, registered_pids: 1 }

# kill terminal 1's shell (or drop the heartbeat). Verify:
#   - the daemon is gone (wiped + SIGKILL self)
#   - the registered process is dead (SIGKILL)
#   - dazai_status() from the MCP server now returns { alive: false }
```

## dazai-oneshot — a self-immolating MCP server

A standalone MCP server that serves a configurable number of tool calls, then
wipes its in-memory secret state and exits. It adds no new mechanism: static
tool values are held in `dazai_secmem::SecretBuffer`s, and the optional daemon
integration reuses the dazai control protocol. `#![deny(unsafe_code)]`.

### Three death conditions (whichever fires first)

| Flag | Condition |
|---|---|
| `--calls N` (default 1) | exit after N tool calls complete |
| `--session` | exit when the client disconnects (stdin EOF) |
| `--dazai-socket PATH` | register with a dazai daemon; self-destruct if it dies |

(`--calls` + `--session` together = exit on *either*.) All paths funnel through
one wipe+exit routine that fires at most once: best-effort daemon `UNREGISTER` →
a brief flush window (so the final MCP response reaches the client — rmcp has no
post-send hook) → optional `--grace N` wait → wipe every `SecretBuffer` → exit.
`--arm` makes the exit a non-elidable wipe + `raise(SIGKILL)`; the default is a
clean `exit(0)`.

### Tool spec format

```bash
--tool 'name=get_key,kind=static,value=s3cr3t'   # fixed value, held in a SecretBuffer
--tool 'name=run,kind=exec,cmd=/path/to/script'  # runs a command, returns its stdout
```

`--tool` is repeatable. Segments are comma-separated `key=value`, so values and
commands must not contain commas. `exec` commands are split on whitespace and
run **without a shell** (no quoting/globbing/expansion) — the configured command
is the only thing executed, so a caller cannot inject anything.

### Run it

```bash
dazai-oneshot --calls 1 --tool 'name=get_key,kind=static,value=s3cr3t' --arm
# an MCP client calls get_key once -> receives 's3cr3t' -> the server wipes + SIGKILLs itself
# a second call -> the process is gone (connection closed)

# tie a tool server's lifetime to a dazai daemon:
dazai-oneshot --session --dazai-socket "$XDG_RUNTIME_DIR/dazai-$UID.sock" \
  --tool 'name=key,kind=static,value=...'
```

### Honest limitations

- **`exec` stdout is NOT in a SecretBuffer** — it is buffered by the OS and the
  process pipe. If you need the wipe guarantee for a value, use `kind=static`
  with a pre-loaded value (held in a locked, wipeable buffer at rest).
- The static value is copied out of its `SecretBuffer` to be returned to the
  client (it must leave the buffer to be served); the buffer protects it **at
  rest** between calls and wipes it on exit.
- A hard crash (no clean exit and no `--arm` path) cannot run the wipe; the
  daemon also retains the registration until its own next trigger. `wipe_and_exit`
  attempts `UNREGISTER` first even on the armed path to minimize this window.

## seccomp allowlist (Linux, `--features seccomp`)

The filter is installed **after** the socket is bound and the buffers are
allocated, just before the accept loop, with default action `KillProcess`. The
conceptual allowlist (the security intent) is:

```
read write close mmap mlock munlock madvise prctl socket bind accept kill
exit_group futex sigprocmask rt_sigaction
```

The effective filter additionally permits the syscalls the Rust runtime, its
threads, the allocator, and `signal-hook` unavoidably issue — verified by
`strace`-ing the running daemon: `clone3`, `munmap`, `mprotect`, `socketpair`
(signal-hook's wakeup pipe), `sigaltstack` (per-thread stack guard),
`sched_getaffinity`, `setsockopt`, `fcntl`, `fstat`, `rt_sigreturn`, `ppoll`,
`clock_*`, `getrandom`, `accept4`, `recvfrom`/`sendto`, `wait4`/`waitid` (child
reap), `statx`/`unlinkat` (socket teardown), … — without these the process
`SIGSYS`es on its own machinery. The dangerous syscalls remain **denied**:
`execve`, `open`/`openat`, `connect`, `ptrace`, `mount`, `setns`, etc. — these
fire only *before* the filter is applied (own exec, glibc startup, the
pre-seccomp child spawn), so the steady-state daemon cannot open files or exec.
(See `dazai_seccomp::conceptual_allowlist`.)

## Portability

- **Linux**: full feature set (`MADV_DONTDUMP`, `PR_SET_DUMPABLE`, `RLIM_INFINITY`,
  seccomp).
- **macOS** (the build machine): `mlock` + non-elidable wipe (`memset_s`) are
  active; `MADV_DONTDUMP`, `prctl`, and seccomp are absent — the daemon prints a
  loud `PLATFORM GUARANTEE NOTICE` at startup listing exactly what is missing.
- OS-specific code uses `#[cfg(target_os = "…")]`, not runtime checks.

## Tests

```bash
cargo test                       # macOS: all non-Linux tests
cargo test --features seccomp    # Linux: full suite incl. live seccomp filter
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

- `dazai-secmem`: alloc/write/read/wipe/Drop cycle, double-wipe safety,
  move-only, no escaping pointer, `mlock` success.
- `dazai-watchdog`: full `PanicController` policy (dry-run never kills; armed
  grace fires only after the deadline; reconnect cancels; hard bypasses grace;
  one-shot guard; **registered → wipe → self ordering**) — all via injected fakes.
- `dazai-child`: spawn/pid/kill/Drop.
- `dazai-seccomp`: allowlist contents and dangerous-syscall omission.
- `dazai-mcp`: client logic against a **mock daemon** (status/register/unregister/
  arm), and panic/hard-panic against an **injected fake signal sender** — no live
  daemon needed.
- `dazai` integration: connection-drop / `SIGUSR1` / `SIGUSR2` / ping-timeout
  dry-run exits, **armed drop really SIGKILLs**, armed reconnect cancels, second
  client refused, control-coexists-with-heartbeat, runtime `ARM`, and a
  **registered PID is SIGKILLed on an armed trigger**.

## Honest remaining limitations

- **seccomp is Linux-only** and is a no-op on macOS — there is no equivalent.
  The macOS build is for development/parity; the confinement guarantee holds on
  Linux only.
- **The effective seccomp allowlist is broader than the conceptual one** (it
  must permit `mmap`/`mprotect`/`clone3`/… for the runtime). `mmap`+`mprotect`
  being allowed means a memory-corruption bug could still mark pages
  executable; seccomp narrows, it does not eliminate, the attack surface.
- **An attached LLM child is a separate process.** `dazai` kills it before
  self-destruct, but secrets the LLM copied into **GPU VRAM**, its own heap, or
  files are outside dazai's reach — VRAM in particular is not wiped by killing
  the host process.
- **`mlock` only prevents swap**, not access by root / same-UID processes (with
  `ptrace`, which `PR_SET_DUMPABLE=0` and seccomp mitigate on Linux but not
  macOS) or DMA/cold-boot attacks.
- **Threads spawn after seccomp is applied**, so `clone3` is necessarily
  allowed; a tighter design would pre-spawn and then seal.
- **`kill` is allowed unconditionally** (no argument restriction). The daemon
  only needs it to signal its single child, but pinning it to that PID requires
  a seccomp argument comparator (`add_rule_conditional` + `ScmpArgCompare`).
  This is a defense-in-depth gap (confined code could signal other same-UID
  PIDs), not a confinement break — `execve`/`open`/`connect`/`ptrace` remain
  denied, and self-termination uses `tgkill` via `raise`, not `kill`.
