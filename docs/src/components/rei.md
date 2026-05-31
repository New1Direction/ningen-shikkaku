# rei — MCP adapter

`rei` exposes the daemon as a set of [MCP](https://modelcontextprotocol.io) tools, so any MCP client — an agent, a tool runner, a swarm — can opt into session-bound protection. It adds **zero new mechanism**: it is a thin adapter (built on the `rmcp` SDK) that relays tool calls to the daemon socket and reads the pidfile to signal it. Run it with `dazai mcp`. It is `#![deny(unsafe_code)]`.

## Why MCP over `--exec`

`--exec` supervises one child the daemon launches. MCP inverts that: agents opt in by **registering their own PID** over a standard protocol. Protection becomes:

- **loosely coupled** — the daemon and the agent launch independently;
- **stack-wide** — any number of agents/tools register (capped at 32);
- **still hard-killing** — a registered PID is `SIGKILL`ed by the daemon on trigger exactly as an `--exec` child would be.

```admonish info title="The contract"
An agent does not need to know how ningen-shikkaku launches. ningen-shikkaku does not need to know anything about the agent. The only handshake is: *register my PID; kill me if the session dies.*
```

## Tools

| Tool | Effect | Returns |
|---|---|---|
| `dazai_status()` | `STATUS` round-trip (a dead daemon is a valid answer) | `{alive, armed, grace_seconds, registered_pids}` |
| `dazai_register(pid)` | register a PID for session-bound protection | `{ok, message}` |
| `dazai_unregister(pid)` | drop a registration | `{ok, message}` |
| `dazai_arm()` | arm the daemon at runtime | `{armed, message}` |
| `dazai_panic()` | `SIGUSR1` to the daemon (graceful) | `{triggered}` |
| `dazai_hard_panic()` | `SIGUSR2` to the daemon (bypass grace) | `{triggered}` |

Full schemas in the [MCP tools reference](../reference/mcp-tools.md); the wire side is in the [socket protocol](../reference/protocol.md).

## Transport

`dazai mcp` speaks MCP over **stdio**, the standard transport — point any MCP client at the command. It relays each call to the daemon's UNIX socket and, for panics, signals the PID it reads from the daemon's pidfile.

```admonish warning title="Stale-pidfile safety"
A panic is gated on the daemon actually being alive (`STATUS` first). If the daemon has exited and its PID was recycled by the OS, `rei` will not blindly signal the recycled PID.
```
