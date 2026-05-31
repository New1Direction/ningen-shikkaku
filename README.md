# dazai — a session-bound, memory-zeroizing dead-man's-switch

A small, portable **reference implementation** of a classic secure-secrets
pattern: a daemon holds secret material in page-locked RAM, keeps a liveness
channel open over a UNIX socket, and — on losing that liveness or receiving a
panic signal — overwrites the secret (`ctypes.memset`) and hard-kills itself
(`SIGKILL`). A shell `trap` ties the daemon's lifetime to your terminal
session, so logging out wipes the secret.

This is built for study and defensive use: it operates **only on its own
process and a synthetic secret**, with safety rails (dry-run by default, an
arming flag, a cancellable grace window). It does not touch any other process,
file, or machine.

## Why each piece exists

| Mechanism | Purpose |
|---|---|
| `mlock(2)` on the working buffers | Keep secret bytes out of swap (defends against secrets leaking to disk). CWE-591 mitigation. |
| `madvise(MADV_DONTDUMP)` (Linux) | Exclude the pages from core dumps. |
| UNIX-socket heartbeat | Liveness signal: a connected client means "still guarded". Its loss is a trigger. |
| `ctypes.memset` | The wipe primitive — overwrite the secret with zeros before exit. |
| `os.kill(getpid, SIGKILL)` | Immediate, uncatchable self-termination once wiped. |
| Shell `trap … EXIT` | Couples secret lifetime to the interactive session. |

## Components

- **`secmem.py`** — `SecureBuffer`: page-aligned `mmap` → `mlock` →
  (Linux) `madvise(DONTDUMP)` → `write` / `read` / `zeroize` → `free`.
  Degrades loudly (stays usable, unlocked) if `mlock` is refused.
- **`deadman.py`** — the daemon: heartbeat listener, signal handlers, and a
  unit-tested `PanicController` that encodes the arming / dry-run / grace policy.
- **`heartbeat.py`** — the client that holds the liveness connection open.
- **`shellrc.sh`** — source-able bash/zsh snippet; launches the client and sets
  the `EXIT` trap.

## Safety model

**Default is `--dry-run`** (i.e. no `--arm`). On any trigger the daemon *does*
run the real `memset` zeroization and logs `WOULD SIGKILL`, but does **not**
kill — so you can rehearse the whole path safely. Pass `--arm` for a real
self-destruct; graceful triggers then go through a cancellable `--grace` window
(a reconnect or a `CANCEL` line aborts). The lethal `SIGKILL` is injected as a
callable, so the test suite exercises the full decide/zeroize path with a fake
killer instead of dying.

## Triggers

| Event | Behavior |
|---|---|
| Heartbeat connection dropped (EOF) | graceful panic |
| `--ping-timeout` deadline missed | graceful panic |
| `SIGUSR1` | graceful panic (via self-pipe → main loop) |
| `SIGUSR2` | **hard** panic: minimal in-handler `memset` + `SIGKILL`, no grace |
| `SIGTERM` / `SIGINT` | clean shutdown (zeroize + exit 0) |

Graceful panic = dry-run wipe (default) or armed grace-window-then-kill.

## Usage

Rehearse safely first (dry-run):

```bash
python3 deadman.py --ping-timeout 15            # terminal A
python3 heartbeat.py --interval 5               # terminal B
# Ctrl-C terminal B  ->  terminal A wipes, logs WOULD SIGKILL, exits 0
```

Arm it for real (the daemon will actually SIGKILL itself):

```bash
python3 deadman.py --arm --grace 5 --ping-timeout 15 &
```

Couple it to your shell — add to `~/.bashrc` or `~/.zshrc`:

```bash
source ~/Documents/dazai/shellrc.sh
```

Now any interactive shell launches a heartbeat client; on logout the `EXIT`
trap kills it, the connection drops, and an armed daemon wipes and dies.

### Wire protocol (newline-delimited text over `SOCK_STREAM`)

```
client → HELLO <pid>   daemon → WELCOME
client → PING          daemon → PONG        (refreshes the ping deadline)
client → CANCEL        daemon → CANCELLED   (aborts a pending armed panic)
client → QUIT          daemon → BYE         (intentional clean stand-down)
connection closed      → heartbeat lost → panic
```

Single-client liveness: while one heartbeat is connected, a second concurrent
connection is refused with `BUSY` and closed, so it cannot displace the real
heartbeat or cancel a pending panic. Only a reconnect *after* the heartbeat was
actually lost cancels an armed grace window.

## Tests

```bash
python3 -m unittest discover -s tests -v
```

Covers `SecureBuffer` write/read/zeroize, the `PanicController` policy
(dry-run never kills; armed grace kills only after the deadline; reconnect
cancels; the fired-once guard), and an end-to-end daemon run driven over the
socket.

## Platform notes

- **macOS & Linux.** libc is resolved via `ctypes.util.find_library` with
  fallbacks; `mlock`/`munlock` work on both. `MADV_DONTDUMP` is Linux-only.
- `RLIMIT_MEMLOCK` caps how much an unprivileged process may lock; the daemon
  tries to raise the soft limit to the hard limit. If locking is still refused,
  it warns and continues unlocked.
- **Socket path limit.** `AF_UNIX` paths are bounded by `sun_path` (~104 bytes
  on macOS, 108 on Linux). The daemon validates `--socket` up front and exits
  with a clear message rather than an opaque bind error.
- **Signals are installed before any secret is written**, so a panic signal
  arriving during startup still wipes instead of hitting the default
  terminate-without-wipe disposition.
- **Shell integration is non-destructive.** `shellrc.sh` is idempotent, is
  `set -u`-safe, and does not clobber an existing exit handler: it chains onto a
  pre-existing bash `EXIT` trap and uses the additive `zshexit` hook in zsh. If
  the daemon socket or `python3` is missing it prints a visible "session
  UNGUARDED" notice instead of failing silently.

## Honest limitation

CPython copies `bytes` objects freely, so a secret may have transiently lived
in unlocked heap before reaching a `SecureBuffer`. This project demonstrates
the *mechanism* (page-locking + explicit zeroization + session-coupled wipe);
it is not a guarantee of zero plaintext residue inside a managed runtime. For
hard guarantees you want a language with end-to-end control of allocation.
