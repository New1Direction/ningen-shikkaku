# sienna — child process wrapper

`sienna` wraps a child process the daemon supervises — typically an LLM runtime launched with `dazai daemon --exec /path/to/llm`. The principle is **parent-owns-the-kill-switch**: the daemon spawns the child, holds its PID, and is the only thing that decides when it dies. It is `#![deny(unsafe_code)]`.

## `ChildProcess`

- `spawn(path, args)` — launches the child via the standard library's `Command` (`fork` + `exec`), with no shell in between, so there is no argument string for an attacker to inject into;
- `none()` — the empty case, when the daemon runs without `--exec`;
- on any trigger, the daemon `SIGKILL`s the child **first**, as step one of the [kill sequence](../kill-sequence.md), before wiping its own buffers.

```admonish note title="No shell, no injection"
The child is executed directly from a path and an argument vector — never `sh -c "…"`. A configured command cannot be turned into shell metacharacter injection because there is no shell to interpret them.
```

## Why a parent-owned child at all

An LLM or tool runtime that handles secrets should not outlive the session that authorized it. By making the daemon the parent, the child's lifetime is bound to the daemon's: when the heartbeat drops, the child is killed in the same deterministic sequence that wipes the secrets it was using.

```admonish info title="MCP is the looser-coupled alternative"
`--exec` supervises **one** child the daemon launches itself. When you want any number of independently-launched agents to opt into the same protection, use the [MCP adapter](./rei.md) instead — agents register their own PID and get the identical hard-kill guarantee. The two mechanisms coexist.
```
