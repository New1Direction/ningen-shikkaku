# ningen-shikkaku

[![CI](https://github.com/New1Direction/ningen-shikkaku/actions/workflows/ci.yml/badge.svg)](https://github.com/New1Direction/ningen-shikkaku/actions/workflows/ci.yml)
[![docs](https://img.shields.io/badge/docs-ningen--shikkaku-c0392b)](https://new1direction.github.io/ningen-shikkaku/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Burn-after-reading secrets for AI agents.**

`ningen-shikkaku` ships two small Rust binaries — **`dazai`** (the session-bound daemon) and **`motokano`** (a self-immolating one-shot MCP server).

Your MCP configs are full of plaintext API keys, and every agent you run can
read all of them — and they keep working long after the agent is done. dazai
inverts that: secrets live in locked, non-swappable RAM, are served to agents
over MCP, and are **destroyed after N reads — or the instant your session
dies**.

![burn-after-reading demo](docs/src/assets/burn.gif)

```bash
motokano --calls 1 --tool 'name=get_key,kind=static,value=s3cr3t' --arm
```

Point any MCP client at it and call `get_key` **once** → you receive `s3cr3t`
→ the server wipes the value out of locked memory and `SIGKILL`s itself. Call
again → the process is gone.

## Install

```bash
# prebuilt binaries (macOS arm64/x86_64, Linux x86_64/arm64)
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/New1Direction/ningen-shikkaku/releases/latest/download/dazai-installer.sh | sh

# or homebrew
brew install New1Direction/tap/dazai New1Direction/tap/motokano

# or from source (required for the Linux seccomp build)
git clone https://github.com/New1Direction/ningen-shikkaku
cd ningen-shikkaku/rs
cargo build --release                       # -> target/release/{dazai, motokano}
cargo build --release --features seccomp    # Linux: seccomp syscall allowlist
```

Prebuilt binaries are built with default features; the seccomp-confined daemon
is a from-source build (it links libseccomp — install `libseccomp-dev` +
`pkg-config` first).

## Use it with Claude Code

```bash
# a one-shot secret an agent can read exactly once:
claude mcp add burn-once -- motokano --calls 1 --arm \
  --tool 'name=get_key,kind=static,value=YOUR-SECRET'

# or the session-bound daemon: agents register their PID and get SIGKILLed
# the moment your session dies
dazai daemon --arm --grace 5 &
claude mcp add dazai -- dazai mcp
```

Any MCP client works the same way — the transport is plain stdio.

## And the second act: a dead-man's switch for your agents

The same daemon is a session kill-switch. Any MCP client registers its PID;
when your shell/SSH session dies, your heartbeat stops, or a panic signal
arrives, dazai `SIGKILL`s every registered process, overwrites its secrets
with a wipe the compiler can't optimize away, and exits. Walk away: agents
die, secrets burn.

```bash
dazai daemon --ping-timeout 15        # terminal A  (dry-run by default; --arm makes it real)
dazai client --interval 5             # terminal B
# close terminal B  ->  the daemon wipes its secrets and exits
```

It runs **only on your own machine, on your own secrets and your own configured
tools**. It never touches another process, file, or host.

## What's in the box

It ships in two layers:

- a **hardened Rust daemon + tooling** — the real thing: `mlock` (no swap),
  `madvise(MADV_DONTDUMP)` + `prctl(PR_SET_DUMPABLE, 0)` (no core dumps / no
  ptrace), a **seccomp** syscall allowlist (no `execve`/`open`/`connect`/…), and
  a non-elidable `explicit_bzero` / `memset_s` wipe; and
- a **Python reference implementation** — the original proof-of-concept that
  established the mechanism (see [`python-reference.md`](python-reference.md)).

| Component | What it does |
|---|---|
| `dazai daemon` | The watchdog: holds `mlock`'d secrets + a UNIX-socket heartbeat; wipes and self-destructs on session loss. seccomp-confined on Linux. |
| `dazai client` | The heartbeat client — ties the daemon's life to a shell / SSH session. |
| `dazai mcp` | An MCP server exposing the daemon as tools, so any agent can register its PID for session-bound protection (it gets `SIGKILL`ed if your session dies). |
| `motokano` | A standalone **self-immolating** MCP server: serve N tool calls, then wipe secret state and exit. |
| Python reference | `secmem.py` / `deadman.py` / `heartbeat.py` / `shellrc.sh` — the portable proof-of-concept. |

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
