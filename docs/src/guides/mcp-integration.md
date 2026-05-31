# MCP agent integration

Any MCP client can opt into session-bound protection by registering its own PID with the daemon. When your session dies, the daemon `SIGKILL`s every registered PID, then wipes and kills itself. This is the [rei](../components/rei.md) adapter in practice.

## Wire it up

```bash
# terminal 1 — the daemon (writes <socket>.pid, 0600)
dazai daemon --arm --grace 5

# terminal 2 — the MCP server over stdio
dazai mcp
```

Point your MCP client at the `dazai mcp` command. The agent then calls the tools directly.

## The registration handshake

```text
agent → dazai_register(pid = <the agent's own PID>)
        ← { ok: true, message: "registered" }

agent → dazai_status()
        ← { alive: true, armed: true, grace_seconds: 5, registered_pids: 1 }
```

From here the agent does nothing special — it just works. If the operator's session dies, the daemon kills the registered PID as step one of the [kill sequence](../kill-sequence.md).

```admonish note title="Up to 32 registrants"
Any number of agents and tools can register, capped at 32. Each gets the identical hard-kill guarantee. Drop a registration with `dazai_unregister(pid)` when a tool exits cleanly on its own.
```

## End-to-end verification

```bash
# terminal 1 — armed daemon
dazai daemon --arm --grace 2

# terminal 2 — MCP server
dazai mcp

# terminal 3 — an MCP client connected to `dazai mcp`:
#   dazai_register(pid = <a process to protect>)
#   dazai_status()  -> { alive: true, armed: true, registered_pids: 1 }

# now kill terminal 1's shell (or drop the heartbeat). Verify:
#   - the daemon is gone (wiped + SIGKILL self)
#   - the registered process is dead (SIGKILL)
#   - dazai_status() from the MCP server now returns { alive: false }
```

```admonish info title="A dead daemon is a valid answer"
`dazai_status()` never errors on a dead daemon — it returns `{ alive: false }`. That is how a client confirms the destruction actually happened.
```

See the [MCP tools reference](../reference/mcp-tools.md) for every tool's schema.
