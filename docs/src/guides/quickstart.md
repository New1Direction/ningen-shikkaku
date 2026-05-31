# quickstart

## Install

You need a recent Rust toolchain. On Linux, install `libseccomp-dev` + `pkg-config` to build the seccomp-confined daemon.

```bash
git clone https://github.com/New1Direction/ningen-shikkaku
cd ningen-shikkaku/rs
cargo build --release                       # -> target/release/{dazai, motokano}
cargo build --release --features seccomp    # Linux: with the seccomp allowlist
```

## Rehearse safely (dry-run)

Dry-run is the default — it performs the wipe and *logs* what it would kill, but never sends a real kill. Use two terminals:

```bash
# terminal A — the daemon
dazai daemon --ping-timeout 15

# terminal B — the heartbeat client
dazai client --interval 5
```

Now close terminal B (or Ctrl-C the client). The daemon detects the dropped heartbeat, wipes its secret buffers, logs `WOULD SIGKILL …`, and exits cleanly. You have just watched the full trigger path with zero risk.

```admonish note title="Try the timeout path too"
Instead of closing the client, stop sending pings (kill just the client process) and watch `--ping-timeout 15` fire after 15 seconds. Reconnect a client *before* the grace window expires (when armed) to see a cancel.
```

## Arm it for real

```admonish danger title="--arm sends real SIGKILLs"
With `--arm`, triggers actually kill registered processes and the daemon itself. Rehearse in dry-run first.
```

```bash
dazai daemon --arm --grace 5 --ping-timeout 15 --exec /path/to/llm
```

## The one-shot, in one line

```bash
motokano --calls 1 --tool 'name=get_key,kind=static,value=s3cr3t' --arm
```

Point any MCP client at it, call `get_key` once → you get the value → it wipes and exits. See [motokano](../components/motokano.md).

## Next

- [configuration](../reference/config.md) — every flag
- [MCP agent integration](./mcp-integration.md) — protect an agent
- [running on Linux](./linux.md) — the full hardening set
