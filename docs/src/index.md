# 人間失格

**ningen-shikkaku** is a session-bound, self-immolating runtime for secrets and the agents that use them.

It holds secret material in locked, non-swappable RAM, watches a heartbeat tied to your shell session, and when that session ends — for any reason — it overwrites everything with a wipe the compiler cannot elide and `SIGKILL`s itself. Sub-millisecond. No swap copy, no core dump, no artifact left to seize.

It runs **only on your own machine, on your own secrets and your own configured tools**. It never reaches another process, file, or host.

```bash
# start the daemon (dry-run by default; --arm makes it real)
dazai daemon --ping-timeout 15

# expose it to agents over MCP
dazai mcp

# hold the heartbeat from your shell (drops when the session dies)
dazai client --interval 5
```

---

```
ningen-shikkaku/
  dazai        the daemon. the man.
  goodnight    secure memory layer
  kikka        the watchdog
  kekkai       the seccomp wall
  sienna       child process wrapper
  rei          MCP adapter
  motokano     one-shot server
```

---

> *Mine has been a life of much shame.*
> *No longer human. No longer here.*
>
> — Osamu Dazai, 1948
