# motokano — one-shot server

`motokano` is a standalone, **self-immolating** MCP server: it serves a configurable number of tool calls, then wipes its in-memory secret state and exits. It is the sharpest expression of the whole project — a secret server whose normal, expected end state is *gone*. It adds no new mechanism (static values live in [`goodnight::SecretBuffer`](./goodnight.md)s; the optional daemon link reuses the [control protocol](../reference/protocol.md)) and is `#![deny(unsafe_code)]`.

## Tools

| Kind | Behavior | Wipe guarantee |
|---|---|---|
| `static` | serves a pre-loaded value held in a locked `SecretBuffer` | **yes** — wiped on exit |
| `exec` | runs an operator-configured command (no shell) and returns its stdout | **no** — stdout is OS-buffered, not locked |

```admonish warning title="exec stdout is not locked"
`kind=exec` is for dynamic values you don't control ahead of time. Its output is **not** in a locked buffer and is **not** covered by the wipe. When you need the wipe guarantee, use `kind=static`.
```

## Three death conditions

Whichever fires first ends the process:

| Flag | Condition |
|---|---|
| `--calls N` (default `1`) | exit after `N` tool calls complete |
| `--session` | exit when the client disconnects (stdin EOF) |
| `--dazai-socket PATH` | register with a [dazai daemon](./dazai.md) and self-destruct if it dies |

`--calls` and `--session` together mean *exit on either*.

## The exit is the product

```admonish danger title="No secret served after the exit fires"
The call counter is a single-fire `compare_exchange`, and a `closed` flag is set **synchronously** the instant the final call is accounted for. Any tool call arriving during the brief response-flush + grace window after exit fires is rejected — the secret cannot be served once the server has committed to dying. All paths funnel through one wipe routine guarded so it runs exactly once, ending in the buffer wipe and process exit.
```

## Demo

```bash
motokano --calls 1 \
  --tool 'name=get_key,kind=static,value=s3cr3t' \
  --arm
```

Point an MCP client at it, call `get_key` **once** → you receive `s3cr3t` → the server wipes the value out of locked memory and `SIGKILL`s itself. Call again → the process is gone.
