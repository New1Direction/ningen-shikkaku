# socket protocol

The daemon listens on a `0600` UNIX stream socket. A connection is one of two kinds, decided by its **first verb**.

## Heartbeat connection

The first verb is `HELLO`. This connection becomes the single liveness channel:

```text
HELLO            -> (accepted as the heartbeat; a second concurrent HELLO -> BUSY)
PING             -> PONG        (optional keepalive; required if --ping-timeout > 0)
<disconnect>     -> liveness lost -> panic
```

```admonish note title="One heartbeat at a time"
Only one heartbeat client is honored. A second concurrent `HELLO` is refused with `BUSY` — the liveness signal can't be ambiguous. Only a reconnect *after a real loss* cancels an armed grace window.
```

## Control connection

A connection whose first verb is **not** `HELLO` is a control connection: request/response, any number of them, never touching the heartbeat lock.

```text
REGISTER pid=<N>    -> OK | BUSY (at 32) | ERROR invalid pid
UNREGISTER pid=<N>  -> OK
ARM                 -> OK (now armed) | ALREADY_ARMED
STATUS              -> STATUS alive=1 armed=<0|1> grace=<n> registered=<n>
```

## Validation and storage

- PIDs are validated with `kill(pid, 0)` — they must be `> 0` and refer to a live process; `0`, negatives, and out-of-range values are rejected before any signal is sent.
- Registered PIDs are stored in a plain `Vec<u32>`, capped at **32**. They are not secret, so they are not held in a `SecretBuffer`.

## On trigger

```admonish danger title="Order"
On any **armed** trigger the daemon `SIGKILL`s every registered PID **before** wiping its buffers and `SIGKILL`ing itself. In dry-run it only logs `WOULD` and never kills. See [the kill sequence](../kill-sequence.md).
```

The [MCP adapter](../components/rei.md) is a client of this protocol: each `dazai_*` tool maps to one of these verbs, and panics are delivered as `SIGUSR1`/`SIGUSR2` to the PID read from the daemon's `<socket>.pid` file.
