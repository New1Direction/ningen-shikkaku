# kekkai — the seccomp wall

`kekkai` (結界, a warding barrier) is the syscall confinement layer. On Linux, with the `seccomp` feature, it installs a seccomp-bpf allowlist whose default action is `KillProcess`: any syscall not on the list terminates the daemon. Elsewhere — non-Linux, or without the feature — it compiles to a no-op stub, so the rest of the workspace is platform-agnostic. It is `#![deny(unsafe_code)]` (the filter is built through the safe `libseccomp` crate).

## Default-deny

```admonish danger title="KillProcess, not errno"
The filter's default action is `KillProcess` — a denied syscall does not return `EPERM` for the program to ignore, it kills the whole thread group immediately. The daemon either runs inside the allowlist or it does not run at all.
```

## Two tiers of allowlist

| Tier | Behavior |
|---|---|
| **core** | the syscalls the steady-state loop cannot live without — resolved with `?`, so a name that fails to resolve is a hard error |
| **runtime** | best-effort extras (allocator, threading, signal plumbing) — names absent on an architecture are simply skipped, not fatal |

This split keeps the allowlist honest on both `aarch64` and `x86_64`: the must-haves are guaranteed, and arch-specific names (e.g. `arch_prctl` on x86_64) are added defensively without breaking the other arch.

## Applied last

The daemon applies the filter **after** every privileged setup syscall (locking memory, binding the socket, writing the pidfile) and **before** the event loop. That ordering is what lets the allowlist stay small: it only has to cover serving, not setup.

```admonish warning title="Validated under the live filter, not emulated"
seccomp is confirmed on **both** architectures running the real `KillProcess` filter — `aarch64` locally and `x86_64` in CI — where the daemon installs the filter and the full suite runs against it with no `SIGSYS`. Emulation (Rosetta / qemu-user) cannot install a guest seccomp filter and therefore cannot validate this; native runs are the only valid check, and CI is the permanent gate.
```
