# goodnight — secure memory

`goodnight` is the secure-memory layer and **the only crate in the workspace permitted to use `unsafe`**. Every other crate is `#![deny(unsafe_code)]` and reaches the platform through `goodnight`'s safe API.

## `SecretBuffer`

The core type. A `SecretBuffer` owns an anonymous `mmap` mapping and guarantees, for as long as it lives:

- **no swap** — the pages are `mlock`ed into RAM;
- **no core dump** — `madvise(MADV_DONTDUMP)` on Linux;
- **no silent copies** — the type is move-only (no `Clone`, no `Copy`); data is written *into* the locked mapping through borrow-checked slices, never duplicated onto a GC or general heap;
- **a wipe on drop** — `Drop` overwrites the mapping before unmapping it.

```admonish note title="API"
`new(len)` · `write(bytes)` · `as_slice()` / `as_mut_slice()` · `wipe()` · `len()` / `is_locked()` · `Drop`
```

## The wipe

The wipe is the whole point, so it is the part the optimizer is forbidden to remove:

| Platform | Mechanism |
|---|---|
| Linux | `explicit_bzero` |
| macOS | `memset_s` |
| fallback | volatile write loop |

A plain `memset` can be optimized away as a "dead store" when the compiler sees the buffer is about to be freed. These three are contractually non-elidable.

## Process-level helpers

`goodnight` also wraps the privileged syscalls the daemon needs, each behind a safe function:

- `try_raise_memlock_rlimit()` — raise `RLIMIT_MEMLOCK` toward `RLIM_INFINITY`
- `set_process_undumpable()` — `prctl(PR_SET_DUMPABLE, 0)` (disables core dumps + `ptrace`)
- `current_uid()`, `pid_exists()`, `send_signal()`, `sigkill_pid()`, `raise_sigkill()`

```admonish warning title="pid hygiene"
The pid helpers reject `pid == 0` and `pid > i32::MAX` before issuing any `kill`, so a malformed or wildcard PID can never be turned into a broadcast signal.
```

## Why one unsafe crate

Confining every `unsafe` block to one small, audited crate means the memory-safety surface of the entire system is a few hundred reviewable lines — and the rest of the workspace is statically guaranteed safe by the compiler.
