# honest limitations

ningen-shikkaku is deliberately honest about where it stops. None of these are bugs — they are the edges of what the mechanism can guarantee, stated up front so you don't over-trust it.

```admonish danger title="It does NOT protect against"
- **GPU VRAM.** If an attached agent copies a secret into GPU memory, killing the host process does not wipe VRAM. ningen-shikkaku controls host RAM and the processes it supervises — not an accelerator's memory.
- **`exec`-tool stdout.** `motokano`'s `kind=exec` tools return a command's stdout, which is OS-buffered and **not** in a locked buffer. Use `kind=static` for the wipe guarantee.
- **Managed-runtime residue (Python reference).** CPython copies `bytes` freely, so a secret may transiently live in unlocked heap before/after the locked buffer. The Rust layer eliminates this for its own buffers; the Python tier is a reference, not a guarantee.
- **A privileged or same-UID attacker on the live box** (root, `/proc/<pid>/mem`, a debugger) reading memory before the wipe. `mlock` stops swap, not reads; seccomp + `PR_SET_DUMPABLE` raise the bar, but an adversary who already has privileged access to your running machine is out of scope.
- **Cold-boot / DMA / physical** attacks on RAM.
- **A hard crash** — kernel OOM, an external `kill -9`, power loss. The wipe path can't run, so nothing is zeroed. The mechanism is best-effort on these paths and never claims otherwise.
```

## Platform gaps

On non-Linux hosts, `madvise(MADV_DONTDUMP)`, `prctl(PR_SET_DUMPABLE, 0)`, and seccomp are **absent**. `mlock` and the non-elidable wipe still apply. The daemon prints a loud platform-guarantee notice at startup so this is never silent. See [running on Linux](../guides/linux.md).

## What's solid

For balance — the guarantees that *do* hold, and are tested:

- secrets are not paged to swap (`mlock`);
- secrets are wiped with a non-elidable overwrite, on trigger and on `Drop`;
- on Linux, secrets are excluded from core dumps and the daemon is not `ptrace`-able;
- on Linux, the daemon runs inside a `KillProcess` seccomp allowlist, validated on both architectures under the live filter;
- the kill sequence fires exactly once and in a fixed order (kill holders → wipe → self-kill).

```admonish quote
ningen-shikkaku is not, and cannot be, a guarantee against an attacker who already owns your running machine. It is a sharp tool for making secrets ephemeral and session-bound, with every limitation stated up front.
```
