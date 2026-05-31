# threat model

ningen-shikkaku shrinks the *window* and the *surface* in which plaintext secrets are reachable, and makes session-end deterministically destroy them. It is precise about what that does and does not buy you.

## What it protects against

| Risk | How |
|---|---|
| Secrets paged to **swap** | `mlock` pins the buffers into RAM |
| Secrets captured in **core dumps** | `madvise(MADV_DONTDUMP)` + `prctl(PR_SET_DUMPABLE, 0)` (Linux) |
| **Session loss** leaving secrets resident | heartbeat drop / logout → wipe + `SIGKILL` |
| Secrets lingering in **process memory** after use | `explicit_bzero` / `memset_s` — a wipe the compiler may not elide |
| A confined process **escaping** | seccomp allowlist denies `execve` / `open` / `connect` / `ptrace` / … (Linux) |
| **ptrace** snooping the daemon | `prctl(PR_SET_DUMPABLE, 0)` (Linux) |

## What it does NOT protect against

```admonish danger title="Out of scope — by design, and stated up front"
- **GPU VRAM.** If an attached agent copies a secret into GPU memory, killing the host process does not wipe VRAM. ningen-shikkaku controls host RAM and the processes it supervises — not an accelerator's memory.
- **`exec`-tool stdout.** `motokano`'s `kind=exec` tools return a command's stdout, which is OS-buffered and **not** held in a locked buffer. Use `kind=static` when you need the wipe guarantee.
- **Managed-runtime residue (Python reference).** CPython copies `bytes` freely, so a secret may transiently live in unlocked heap. The **Rust** layer eliminates this for its own buffers; the Python tier is a reference, not a guarantee.
- **A privileged or same-UID attacker on the live box** (root, `/proc/<pid>/mem`, a debugger) reading memory before the wipe. `mlock` stops swap, not reads.
- **Cold-boot / DMA / physical** attacks on RAM.
- **A hard crash** (kernel OOM, external `kill -9`, power loss): the wipe path can't run, so nothing is zeroed.
```

## The honest summary

```admonish quote
ningen-shikkaku is not, and cannot be, a guarantee against an attacker who already owns your running machine. It is a sharp tool for making secrets ephemeral and session-bound, with every limitation stated up front.
```

## How the guarantees are checked

- **66 tests** across the Rust workspace (secure memory, the panic policy, the wire protocol, the MCP layer, the one-shot lifecycle), plus the **29-test** Python reference.
- **Linux seccomp is validated on both architectures under the real `KillProcess` filter** — `aarch64` locally and `x86_64` in CI — where the daemon installs the live filter and the full integration suite runs against it with no `SIGSYS`.
- Every push runs the whole suite (default **and** `--features seccomp`), `clippy -D warnings`, and `rustfmt --check` as a permanent regression gate.
