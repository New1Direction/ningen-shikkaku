# daZai

[![CI](https://github.com/New1Direction/ningen-shikkaku/actions/workflows/ci.yml/badge.svg)](https://github.com/New1Direction/ningen-shikkaku/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Secrets that live only as long as you do.**

daZai is a session-bound, memory-zeroizing dead-man's-switch. It pins secret
material into locked, non-swappable RAM, holds a liveness channel tied to your
shell or SSH session, and the moment that liveness is lost — you log out, the
connection drops, a panic signal arrives, or a daemon it's watching dies — it
overwrites the secrets with a wipe the compiler can't optimize away and
`SIGKILL`s the processes holding them.

It runs **only on your own machine, on your own secrets and your own configured
tools**. It never touches another process, file, or host.

It ships in two layers:

- a **hardened Rust daemon + tooling** — the real thing: `mlock` (no swap),
  `madvise(MADV_DONTDUMP)` + `prctl(PR_SET_DUMPABLE, 0)` (no core dumps / no
  ptrace), a **seccomp** syscall allowlist (no `execve`/`open`/`connect`/…), and
  a non-elidable `explicit_bzero` / `memset_s` wipe; and
- a **Python reference implementation** — the original proof-of-concept that
  established the mechanism (see [`python-reference.md`](python-reference.md)).

## What's in the box

| Component | What it does |
|---|---|
| `dazai daemon` | The watchdog: holds `mlock`'d secrets + a UNIX-socket heartbeat; wipes and self-destructs on session loss. seccomp-confined on Linux. |
| `dazai client` | The heartbeat client — ties the daemon's life to a shell / SSH session. |
| `dazai mcp` | An MCP server exposing the daemon as tools, so any agent can register its PID for session-bound protection (it gets `SIGKILL`ed if your session dies). |
| `motokano` | A standalone **self-immolating** MCP server: serve N tool calls, then wipe secret state and exit. |
| Python reference | `secmem.py` / `deadman.py` / `heartbeat.py` / `shellrc.sh` — the portable proof-of-concept. |

## Install

Needs a recent Rust toolchain. On Linux, install `libseccomp-dev` + `pkg-config`
to build the seccomp-confined daemon.

```bash
git clone https://github.com/New1Direction/ningen-shikkaku
cd ningen-shikkaku/rs
cargo build --release                       # -> target/release/{dazai, motokano}
cargo build --release --features seccomp    # Linux: with the seccomp allowlist
```

## Demo

A self-destructing one-shot secret server, in one line:

```bash
motokano --calls 1 \
  --tool 'name=get_key,kind=static,value=s3cr3t' \
  --arm
```

Point any MCP client at it and call `get_key` **once** → you receive `s3cr3t` →
the server wipes the value out of locked memory and `SIGKILL`s itself. Call
again → the process is gone.

Or the session-coupled daemon (dry-run is the safe default; `--arm` makes it
real):

```bash
dazai daemon --ping-timeout 15        # terminal A
dazai client --interval 5             # terminal B
# close terminal B  ->  the daemon wipes its secrets and exits
```

## Threat model

daZai shrinks the *window* and the *surface* in which plaintext secrets are
reachable, and makes session-end deterministically destroy them.

**It protects against:**

| Risk | How |
|---|---|
| Secrets paged to **swap** | `mlock` locks the buffers into RAM |
| Secrets captured in **core dumps** | `madvise(MADV_DONTDUMP)` + `prctl(PR_SET_DUMPABLE, 0)` (Linux) |
| **Session loss** leaving secrets resident | heartbeat drop / logout → wipe + `SIGKILL` |
| Secrets lingering in **process memory** after use | `explicit_bzero` / `memset_s` — a wipe the compiler may not elide |
| A confined process **escaping** | seccomp allowlist denies `execve` / `open` / `connect` / `ptrace` / … (Linux) |
| **ptrace** snooping the daemon | `prctl(PR_SET_DUMPABLE, 0)` (Linux) |

**It does NOT protect against** (and the project is deliberately honest about
this):

- **GPU VRAM.** If an attached LLM/agent copies a secret into GPU memory,
  killing the host process does not wipe VRAM. daZai controls host RAM and the
  processes it supervises — not an accelerator's memory.
- **`exec`-tool stdout.** `motokano`'s `kind=exec` tools return a command's
  stdout, which is OS-buffered and **not** held in a locked buffer. Use
  `kind=static` (a pre-loaded, locked, wipeable value) when you need the wipe
  guarantee.
- **Managed-runtime residue (Python reference).** CPython copies `bytes` freely,
  so a secret may transiently live in unlocked heap before/after the locked
  buffer. The **Rust** implementation eliminates this for its own buffers (data
  is written into the locked mapping and never copied to a GC heap); the Python
  tier is a *reference*, not a hard guarantee.
- **A privileged or same-UID attacker on the live box** (root, `/proc/<pid>/mem`,
  a debugger) reading memory before the wipe. `mlock` stops swap, not memory
  reads; seccomp + `PR_SET_DUMPABLE` raise the bar on Linux, but an adversary
  who already has privileged access to your running machine is out of scope.
- **Cold-boot / DMA / physical** attacks on RAM.
- **A hard crash** (kernel OOM, an external `kill -9`, power loss): the wipe
  path can't run, so nothing is zeroed. The mechanism is best-effort on these
  paths and never claims otherwise.

In short: daZai is not, and cannot be, a guarantee against an attacker who
already owns your running machine. It is a sharp tool for making secrets
ephemeral and session-bound, with every limitation stated up front.

## Verification

- **66 tests** across the Rust workspace (secure memory, the panic policy, the
  wire protocol, the MCP layer, the one-shot lifecycle) plus the **29-test**
  Python reference.
- **Linux seccomp is validated on both architectures under the real
  `KillProcess` filter** — `aarch64` (locally) and `x86_64`
  ([CI](https://github.com/New1Direction/ningen-shikkaku/actions)) — where the daemon
  installs the live filter and the full integration suite runs against it with
  no `SIGSYS`.
- Every push runs the whole suite (default **and** `--features seccomp`),
  `clippy -D warnings`, and `rustfmt --check` on x86_64 Linux as a permanent
  regression gate.

Deep technical docs (per-crate design, the seccomp allowlist, the
adversarial-review history) live in [`rs/README.md`](rs/README.md); the Python
proof-of-concept is in [`python-reference.md`](python-reference.md).

## The name

Named for the novelist **Osamu Dazai**, whose work circles themes of
disappearance and self-erasure — fitting for software whose defining act is to
wipe itself out the moment it is no longer being watched over. The repository
takes its name from his novel *No Longer Human* (人間失格, *Ningen Shikkaku*).
It's flavor, not a manifesto; the software just self-destructs on cue.

## License

[MIT](LICENSE) © 2026 New1Direction.
