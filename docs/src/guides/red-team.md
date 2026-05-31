# red team usage

ningen-shikkaku is operator-side tooling for an **authorized engagement**: it makes *your own* engagement secrets — the credentials, tokens, and keys issued to you for the work — ephemeral and bound to your session, on *your own* operator host.

```admonish danger title="Scope"
Everything here runs on **your** machine, on **your** secrets, against processes **you** launched. ningen-shikkaku never reaches another host, and nothing on this page is about acting on a target. It is about not leaving your own credentials lying in memory after the session that authorized them ends.
```

## The problem it solves for an operator

During an engagement your operator box accumulates secrets in long-running processes: a C2 operator console, a tool runner, a notebook holding a borrowed credential. If that box is lost, imaged, or crash-dumped, those secrets are recoverable from memory or swap. ningen-shikkaku ties their lifetime to your session and destroys them deterministically when it ends.

## Pattern: session-bound credential

```bash
# hold the engagement key in locked, non-dumpable RAM, armed, with a short grace
dazai daemon --arm --grace 3 --ping-timeout 20

# from your operator shell, hold the heartbeat
dazai client --interval 5
```

Close the shell, lose the SSH session, or trip the timeout → the key is wiped and the daemon dies within the grace window. No swap copy, no core dump, nothing to seize.

## Pattern: protect the whole tool stack

Register every engagement process so they all die together when the session ends — see [MCP agent integration](./mcp-integration.md). A registered PID is `SIGKILL`ed by the daemon on trigger, so a forgotten tool process can't outlive the engagement.

## Pattern: one-shot credential handoff

When a tool needs a secret exactly once, serve it with [motokano](../components/motokano.md) and let it self-destruct:

```bash
motokano --calls 1 --session \
  --tool 'name=token,kind=static,value=<engagement-token>' --arm
```

The value lives in a locked buffer, is served once, then wiped — and the server also dies if the [daemon](../components/dazai.md) it's linked to dies.

```admonish warning title="Read the limits"
This protects host RAM and the processes ningen-shikkaku supervises. It does **not** cover GPU VRAM, `exec`-tool stdout, or a privileged attacker already on the live box. Know exactly where it stops — see [honest limitations](../reference/limitations.md).
```
