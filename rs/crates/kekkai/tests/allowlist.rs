//! Allowlist sanity (runs everywhere; the live filter is exercised only on
//! Linux with the `seccomp` feature, via the binary integration tests).

use kekkai::conceptual_allowlist;

#[test]
fn allowlist_contains_the_required_core_syscalls() {
    let list = conceptual_allowlist();
    for expected in [
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
    ] {
        assert!(list.contains(&expected), "allowlist missing {expected}");
    }
}

#[test]
fn allowlist_denies_dangerous_syscalls_by_omission() {
    let list = conceptual_allowlist();
    // These must NOT be in the conceptual allowlist (the filter's default
    // action kills on anything not listed).
    for forbidden in ["execve", "open", "openat", "connect", "ptrace", "fork"] {
        assert!(
            !list.contains(&forbidden),
            "{forbidden} must not be allowlisted"
        );
    }
}

#[test]
fn apply_is_callable_and_succeeds_as_noop_off_linux() {
    // On macOS / without the feature this is the no-op stub and must succeed.
    // On Linux+feature it installs the real filter (exercised in integration).
    #[cfg(not(all(target_os = "linux", feature = "seccomp")))]
    {
        kekkai::apply().expect("no-op apply must succeed");
    }
}
