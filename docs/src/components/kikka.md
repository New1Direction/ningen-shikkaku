# kikka — the watchdog

`kikka` is the liveness engine and the panic policy. It owns the socket listener, the signal thread, and the event loop — and it decides *when* to die and *in what order*. It is `#![deny(unsafe_code)]`.

## `Watchdog`

The `Watchdog` accepts connections on the daemon's UNIX socket and turns them into liveness events:

- the **first** verb on a connection decides its kind — `HELLO` makes it the single **heartbeat** client; anything else makes it a **control** connection ([protocol](../reference/protocol.md));
- heartbeat liveness is tracked with a generation tag, so a stale reader thread from an old connection can never cancel a panic armed by a newer one;
- with `--ping-timeout`, each connection carries a per-line ping deadline;
- control connections are request/response, capped, and never touch the single-client heartbeat lock.

Registered PIDs are kept in a plain `Vec<u32>` (capped at **32**) — they are not secret, so they are *not* stored in a `SecretBuffer`.

## `PanicController`

The policy object. It is constructed with injected closures — `wipe`, `kill`, `kill_registered`, `dry_done`, `clock`, `log` — so the *mechanism* (how to wipe, how to kill) is decoupled from the *policy* (the order, the grace window, the single-fire guarantee). That separation is also what makes the whole [kill sequence](../kill-sequence.md) unit-testable without ever locking real memory or killing a real process.

```admonish note title="The fixed sequence"
`kill_registered` → `wipe` → `kill self`. Registered holders die before the buffers are wiped; the buffers are wiped before the daemon kills itself.
```

## Single-fire

```admonish info title="compare_exchange"
The controller holds an `Arc<AtomicBool>` armed flag and fires through a `compare_exchange`. Concurrent triggers — a dropped socket *and* a signal *and* a timeout at once — collapse to exactly one run of the sequence.
```

## Grace and cancel

When armed, a graceful trigger starts the `--grace` window. Only a real heartbeat **reconnect after a loss** cancels it; an ordinary message on an already-live connection does not. A hard panic (`SIGUSR2`) ignores the window.
