#![deny(unsafe_code)]
//! seccomp syscall allowlist for the dazai watchdog.
//!
//! On Linux with the `seccomp` feature, [`apply`] installs a default-`KillProcess`
//! seccomp-bpf filter that permits only the syscalls the watchdog needs and
//! denies everything else — notably `execve`, `open`/`openat`, `connect`,
//! `ptrace`, and any other escape vector. It is meant to be applied *after* the
//! socket is bound and the secret buffers are allocated, just before the accept
//! loop, so the steady-state process is tightly confined.
//!
//! On macOS, or when the `seccomp` feature is off, [`apply`] is a no-op that
//! prints a one-line notice — there is no equivalent macOS facility, so this is
//! a guarantee Phase 2 provides on Linux only.
//!
//! ## Allowlist scope
//! The conceptual allowlist (see [`conceptual_allowlist`]) is the *intent*:
//! read/write/close/mmap/mlock/munlock/madvise/prctl/socket/bind/accept/kill/
//! exit_group/futex/sigprocmask/rt_sigaction. The *effective* filter additionally
//! permits the syscalls the Rust runtime, its threads, the allocator, and
//! signal-hook unavoidably issue (e.g. `clone3`, `munmap`, `mprotect`,
//! `rt_sigreturn`, `pipe2`, `clock_*`, `getrandom`). Without those the process
//! would `SIGSYS` on its own machinery. The dangerous syscalls remain denied.

/// The conceptual allowlist from the design spec (the security *intent*).
///
/// Useful for documentation and for asserting the filter has not drifted.
pub fn conceptual_allowlist() -> &'static [&'static str] {
    &[
        "read",
        "write",
        "close",
        "mmap",
        "mlock",
        "munlock",
        "madvise",
        "prctl",
        "socket",
        "bind",
        "accept",
        "kill",
        "exit_group",
        "futex",
        "sigprocmask",
        "rt_sigaction",
    ]
}

#[cfg(all(target_os = "linux", feature = "seccomp"))]
mod imp {
    use anyhow::{Context, Result};
    use libseccomp::{ScmpAction, ScmpFilterContext, ScmpSyscall};

    /// Syscalls additionally required to merely *run* the multi-threaded Rust
    /// watchdog (allocator, thread spawn/teardown, signal-hook self-pipe,
    /// monotonic clocks). Denying these would SIGSYS the process on startup.
    const RUNTIME_REQUIRED: &[&str] = &[
        "munmap",
        "mprotect",
        "brk",
        "clone",
        "clone3",
        "set_robust_list",
        "rseq",
        "rt_sigreturn",
        "rt_sigprocmask",
        "restart_syscall",
        "sched_yield",
        "nanosleep",
        "clock_nanosleep",
        "clock_gettime",
        "gettid",
        "getrandom",
        "pipe2",
        "poll",
        "ppoll",
        "epoll_create1",
        "epoll_ctl",
        "epoll_wait",
        "accept4",
        "recvfrom",
        "sendto",
        "getpid",
        "exit",
        "tgkill",
        "munlockall",
        // Reaping the LLM child: std's Child::kill()/wait()/Drop -> waitpid,
        // which is the wait4 (or waitid) syscall on Linux.
        "wait4",
        "waitid",
        "waitpid",
        // Clean-shutdown socket teardown: Path::exists() -> statx/newfstatat,
        // std::fs::remove_file -> unlink/unlinkat.
        "statx",
        "newfstatat",
        "fstatat",
        "unlink",
        "unlinkat",
        "faccessat",
        "faccessat2",
        // Verified by stracing the running daemon (all issued AFTER apply()):
        //   socketpair        - signal-hook's wakeup self-pipe
        //   sigaltstack       - per-thread stack-overflow guard on thread spawn
        //   sched_getaffinity - pthread setup / runtime sizing
        //   setsockopt        - UnixStream::set_read_timeout (ping deadline)
        //   fcntl             - UnixStream::try_clone (F_DUPFD_CLOEXEC)
        //   fstat             - stderr tty probe on the first post-apply log
        // (execve and open/openat are deliberately NOT here: they occur only
        //  before apply() — own exec, glibc startup, the pre-seccomp child
        //  spawn — so the steady-state filter still denies file-open and exec.)
        "socketpair",
        "sigaltstack",
        "sched_getaffinity",
        "setsockopt",
        "fcntl",
        "fstat",
        // x86_64 TLS syscall (glibc); benign (sets a segment base, no file/exec/
        // net surface). Added defensively for native x86_64 — the local
        // Rosetta-emulated run could NOT validate seccomp (qemu/Rosetta does not
        // support a guest installing a seccomp filter), so native x86_64 CI is
        // the validator. Best-effort, so it is skipped on aarch64.
        "arch_prctl",
    ];

    /// Install the seccomp filter (default action: kill the process).
    ///
    /// Note: `kill` is allowed unconditionally (no argument restriction). The
    /// daemon only needs it to signal its single child, but pinning it to that
    /// pid requires an argument comparator; see the README "remaining
    /// limitations" — it is a defense-in-depth gap, not a confinement break.
    pub fn apply() -> Result<()> {
        let mut filter = ScmpFilterContext::new_filter(ScmpAction::KillProcess)
            .context("creating seccomp filter context")?;

        // Core allowlist: the security-relevant intent. These MUST resolve.
        for name in super::conceptual_allowlist() {
            let syscall = ScmpSyscall::from_name(name)
                .with_context(|| format!("resolving core syscall {name}"))?;
            filter
                .add_rule(ScmpAction::Allow, syscall)
                .with_context(|| format!("allowing core syscall {name}"))?;
        }

        // Runtime-required extras are arch/libc-dependent. Add them best-effort
        // so a name absent on this architecture (e.g. `unlink`/`waitpid` on
        // aarch64, which only have the *at / wait4 forms) is skipped with a
        // note rather than failing the whole filter.
        let mut runtime_added = 0usize;
        for name in RUNTIME_REQUIRED {
            match ScmpSyscall::from_name(name) {
                Ok(syscall) => match filter.add_rule(ScmpAction::Allow, syscall) {
                    Ok(()) => runtime_added += 1,
                    Err(e) => eprintln!("[kekkai] note: could not allow {name}: {e}"),
                },
                Err(_) => { /* not present on this arch; skip */ }
            }
        }

        filter.load().context("loading seccomp filter")?;
        eprintln!(
            "[kekkai] seccomp active: {} core + {} runtime syscalls allowed; default = KillProcess",
            super::conceptual_allowlist().len(),
            runtime_added
        );
        Ok(())
    }
}

#[cfg(not(all(target_os = "linux", feature = "seccomp")))]
mod imp {
    use anyhow::Result;

    /// No-op: seccomp is unavailable on this platform / build configuration.
    pub fn apply() -> Result<()> {
        eprintln!(
            "[kekkai] WARNING: seccomp NOT active (requires Linux + the `seccomp` feature). \
             The watchdog process is NOT syscall-confined."
        );
        Ok(())
    }
}

/// Apply the seccomp confinement appropriate for this platform/build.
pub use imp::apply;
