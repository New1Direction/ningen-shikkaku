# running on Linux

Linux is where ningen-shikkaku is strongest: it is the only platform where the full hardening set — core-dump suppression, `ptrace` denial, and the seccomp allowlist — is active. On macOS those three are absent and the daemon says so loudly at startup; `mlock` and the non-elidable wipe still apply everywhere.

## Build dependencies

```bash
sudo apt-get install -y libseccomp-dev pkg-config
```

`libseccomp-dev` is needed only for the `seccomp` feature; the default build does not require it.

## Build and run with confinement

```bash
cd rs
cargo build --release --features seccomp
cargo run --release --features seccomp -- daemon --arm
```

With the feature on, the daemon installs the [kekkai](../components/kekkai.md) allowlist after setup and before the event loop. A syscall outside the list `KillProcess`-terminates the daemon.

## RLIMIT_MEMLOCK

`mlock` is bounded by `RLIMIT_MEMLOCK`. The daemon raises it toward `RLIM_INFINITY` at startup; if it can't (no `CAP_IPC_LOCK`, a tight cgroup limit) it warns and continues with best-effort locking.

```admonish tip title="Grant the capability instead of running as root"
Prefer `setcap 'cap_ipc_lock=+ep' ./target/release/dazai` over running the daemon as root — it gets the lock limit it needs and nothing else.
```

## What's active where

| Guarantee | Linux | macOS |
|---|---|---|
| `mlock` (no swap) | ✅ | ✅ |
| non-elidable wipe | ✅ `explicit_bzero` | ✅ `memset_s` |
| `madvise(MADV_DONTDUMP)` | ✅ | ❌ |
| `prctl(PR_SET_DUMPABLE, 0)` | ✅ | ❌ |
| seccomp allowlist | ✅ | ❌ (no-op stub) |

```admonish warning title="Confirm on the real arch"
seccomp is validated on `aarch64` and `x86_64` under the live `KillProcess` filter. Emulation (Rosetta / qemu-user) cannot install a guest seccomp filter, so it cannot validate the filter — test on native Linux for that architecture, which is exactly what CI does.
```
