# Show HN draft

## Title (pick one — lead option first)

1. `Show HN: Dazai – burn-after-reading secrets for AI agents`
2. `Show HN: Dazai – secrets your AI agent can read exactly once`
3. `Show HN: A dead-man's switch for AI agents and their secrets`

## Post body

> I kept being bothered by two things about running AI agents locally: my MCP
> configs are full of plaintext API keys that any agent can read, and those
> keys keep working long after the agent — or my session — is gone.
>
> dazai is my answer: a small Rust daemon that holds secrets in locked,
> non-swappable RAM (`mlock` + `madvise(MADV_DONTDUMP)` +
> `prctl(PR_SET_DUMPABLE, 0)`, seccomp-confined on Linux) and serves them to
> agents over MCP. Secrets are destroyed after N reads, or the instant your
> session dies — wiped with `explicit_bzero` and the process `SIGKILL`ed.
>
> The one-liner demo is a self-immolating MCP server:
>
>     motokano --calls 1 --tool 'name=get_key,kind=static,value=s3cr3t' --arm
>
> Point any MCP client at it, call `get_key` once, get the secret — then the
> server wipes the value out of locked memory and kills itself. Call again:
> the process is gone.
>
> The same daemon doubles as a dead-man's switch: agents register their PID
> over MCP, and when my shell/SSH session dies every registered process gets
> SIGKILLed. Walk away: agents die, secrets burn.
>
> To be honest about what it is NOT: it can't wipe GPU VRAM, it can't un-read
> a secret an agent already put in its context window, and it can't defend
> against root on a live box. The threat model in the README spells out every
> limitation — the goal is to make secrets ephemeral and session-bound, not to
> claim immunity from an attacker who already owns your machine.
>
> It runs only on your own machine, on your own secrets and configured tools.
> Rust workspace, 66 tests, seccomp validated on both x86_64 and aarch64. MIT.

## Prepared first comment

> A few design notes that didn't fit the post:
>
> **Why MCP?** The daemon originally supervised one child via `--exec`. MCP
> inverts that: any agent registers its own PID over a standard protocol, so
> dazai and the agents launch independently and protection is stack-wide. The
> MCP server adds zero new mechanism — it's a thin adapter over the daemon's
> UNIX-socket control protocol.
>
> **Memory story:** all `unsafe` is confined to one crate (`goodnight`), which
> owns the mmap+mlock'd buffer; every other crate is `#![deny(unsafe_code)]`.
> Data is written into the locked mapping via borrow-checked slices and never
> copied to GC/heap; the buffer type is move-only. The wipe is
> `explicit_bzero`/`memset_s` — non-elidable by contract.
>
> **Origin:** it started as a Python proof-of-concept (still in the repo as
> the reference implementation). CPython copies `bytes` freely, which is
> exactly the residue problem the Rust rewrite eliminates.
>
> **The known gap:** once an agent has *read* a secret, that disclosure is
> permanent — context windows, logs, VRAM. The roadmap answer is a signing
> proxy: dazai holds the key and signs requests on the agent's behalf, so the
> agent never sees the credential at all. Burn-after-reading shrinks the
> window; non-disclosure closes it.

## Posting notes

- Post Tue–Thu, 8–10am US Eastern.
- Be in the comments for the first 3–4 hours; answer every technical
  question; concede limitations immediately (the threat-model honesty IS the
  brand).
- Do not post until everything in [checklist.md](checklist.md) above the
  "Show HN" line is done — dead install links kill a launch.
