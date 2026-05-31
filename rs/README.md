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
dazai           CLI binary: `daemon` and `client` subcommands.
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
  one-shot guard) — all via an injected fake killer.
- `dazai-child`: spawn/pid/kill/Drop.
- `dazai-seccomp`: allowlist contents and dangerous-syscall omission.
- `dazai` integration: spawn daemon+client, connection-drop / `SIGUSR1` /
  `SIGUSR2` / ping-timeout dry-run exits, **armed drop really SIGKILLs**, armed
  reconnect cancels, second client refused.

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
