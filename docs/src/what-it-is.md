# what it is

ningen-shikkaku is a **dead-man's-switch for secret material**. You give it a secret — an API key, a token, a credential — and a liveness signal tied to your session. While the signal is alive the secret lives in locked RAM. The moment the signal stops, the secret is destroyed and the processes holding it are killed.

That's the whole idea. Everything else is making the *destroy* step trustworthy: not paged to swap, not captured in a core dump, not left as a compiler-elided "wipe" that never ran, and not escapable by the confined process.

## Why it exists

A long-running process that touches secrets is a liability the entire time it runs. The secret sits in heap, maybe gets copied to swap, maybe lands in a core dump on the next crash, and stays resident long after the last use. The usual answer — "remember to zero it" — is a wish, not a guarantee.

ningen-shikkaku shrinks the *window* (the secret is destroyed at session end, deterministically) and the *surface* (locked pages, no dumps, a seccomp syscall allowlist) in which plaintext is reachable.

## Two layers

```admonish info title="It ships in two layers"
- a **hardened Rust daemon + tooling** — the real implementation, with guarantees the CPython runtime cannot offer (non-elidable wipe, `mlock`, `madvise(MADV_DONTDUMP)`, `prctl`, a seccomp allowlist);
- a **Python reference implementation** — the original proof-of-concept that established the mechanism. It is a *reference*, not a hard guarantee (see [honest limitations](./reference/limitations.md)).
```

## Scope, stated plainly

```admonish danger title="It runs only on your own machine"
ningen-shikkaku operates on **your own secrets, your own processes, and your own configured tools**. It never touches another process, file, or host. It is defensive, operator-side tooling — a way to make *your* secrets ephemeral and session-bound — not a means of acting on anyone else's system.
```

It is **not**, and cannot be, a guarantee against an attacker who already owns your running machine. It is a sharp tool for making secrets ephemeral, with every limitation stated up front. The next pages show exactly how it works and exactly where it stops.
