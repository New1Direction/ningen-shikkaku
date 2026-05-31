# configuration

Every flag, by command.

## `dazai daemon`

| Flag | Default | Meaning |
|---|---|---|
| `--arm` | off (dry-run) | enable real self-destruct (wipe + `SIGKILL`); without it, wipes and logs `WOULD` but never kills |
| `--grace N` | `5` | armed graceful-panic grace window, seconds; a reconnect/cancel during it aborts |
| `--ping-timeout N` | `0` (off) | panic if no `PING` arrives within `N` seconds |
| `--socket PATH` | `${XDG_RUNTIME_DIR:-/tmp}/dazai-$UID.sock` | UNIX socket path (created `0600`) |
| `--exec PATH` | none | spawn this as a supervised child ([sienna](../components/sienna.md)); killed first on trigger |
| `--size BYTES` | `4096` | synthetic working-buffer size |

## `dazai client`

| Flag | Default | Meaning |
|---|---|---|
| `--interval N` | `0` | send `PING` every `N` seconds; `0` = just hold the connection open |
| `--socket PATH` | daemon default | socket to connect to |

## `dazai mcp`

| Flag | Default | Meaning |
|---|---|---|
| `--socket PATH` | daemon default | daemon socket to relay to |
| `--transport stdio` | `stdio` | MCP transport (stdio is the standard) |

## `motokano`

| Flag | Default | Meaning |
|---|---|---|
| `--calls N` | `1` | exit after `N` tool calls complete |
| `--session` | off | also exit when the client disconnects (stdin EOF) |
| `--dazai-socket PATH` | none | register with a daemon and die if it dies |
| `--tool '<spec>'` | — | declare a tool (repeatable); see below |
| `--arm` | off (dry-run) | enable the real wipe-and-exit |
| `--grace N` | — | delay before the final wipe/exit after the trigger |

### `--tool` spec

```text
name=<tool name>,kind=static,value=<the secret>     # locked, wipeable
name=<tool name>,kind=exec,cmd=<command + args>     # runs no-shell; stdout NOT locked
```

```admonish note title="Defaults compose"
`--calls` and `--session` together = exit on **either**. Adding `--dazai-socket` adds a third, independent death condition; whichever fires first wins.
```
