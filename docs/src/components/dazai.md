# dazai — the daemon

`dazai` is the CLI binary and the watchdog that everything else orbits. It holds the secret buffers, owns the heartbeat socket, and runs the [kill sequence](../kill-sequence.md).

## Subcommands

| Command | Role |
|---|---|
| `dazai daemon` | the watchdog: holds `mlock`'d secrets + a UNIX-socket heartbeat, wipes and self-destructs on session loss, seccomp-confined on Linux |
| `dazai client` | the heartbeat client — ties the daemon's life to a shell / SSH session |
| `dazai mcp` | the [MCP adapter](./rei.md) — exposes the daemon as tools any agent can call |

## Startup order

The daemon performs privileged setup in a fixed order, then confines itself before serving:

1. raise `RLIMIT_MEMLOCK`
2. `prctl(PR_SET_DUMPABLE, 0)` (Linux)
3. allocate + `mlock` the secret buffers ([goodnight](./goodnight.md))
4. spawn the LLM child if `--exec` ([sienna](./sienna.md))
5. bind the `0600` UNIX socket + write the `0600` pidfile
6. apply seccomp ([kekkai](./kekkai.md), Linux + `seccomp` feature)
7. enter the accept/event loop ([kikka](./kikka.md))

```admonish info title="Why the pidfile"
The daemon writes `<socket>.pid` (mode `0600`) before applying seccomp — creating a file needs `openat`, which the filter denies. The [MCP adapter](./rei.md) reads it to find the daemon for `dazai_panic` / `dazai_hard_panic`.
```

## Defaults

- **socket:** `${XDG_RUNTIME_DIR:-/tmp}/dazai-$UID.sock`, mode `0600`
- **pidfile:** `<socket>.pid`, mode `0600`
- **mode:** dry-run unless `--arm`
- **grace:** `5` seconds (armed graceful panic)

See [configuration](../reference/config.md) for the full flag list.

## Memory safety

`dazai` itself is `#![deny(unsafe_code)]`. Every `unsafe` operation it needs — locking memory, wiping, sending signals — lives behind the safe API of [goodnight](./goodnight.md), the one crate permitted `unsafe`.
