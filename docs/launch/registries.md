# MCP registry submissions

Two servers to list, one description language everywhere.

**Shared copy:**

- Name: `dazai` (daemon adapter) and `motokano` (one-shot server)
- One-liner: *Burn-after-reading secrets for AI agents — keys live in locked
  RAM, served over MCP, destroyed after N reads or the instant your session
  dies.*
- Transport: stdio. No network. Runs only on the operator's machine.

**Commands clients run:**

```bash
# one-shot secret (read exactly once):
motokano --calls 1 --arm --tool 'name=get_key,kind=static,value=YOUR-SECRET'

# session-bound daemon adapter (daemon must be running):
dazai mcp
```

## 1. Official MCP registry (registry.modelcontextprotocol.io)

Mechanism: `server.json` published with the `mcp-publisher` CLI; namespace is
verified via GitHub (`io.github.new1direction/...`). Docs:
https://github.com/modelcontextprotocol/registry

Draft `server.json` (place at repo root when publishing; binaries must be
installable first — i.e. after the v0.1.0 release exists):

```json
{
  "$schema": "https://static.modelcontextprotocol.io/schemas/2025-09-29/server.schema.json",
  "name": "io.github.new1direction/dazai",
  "description": "Burn-after-reading secrets for AI agents: keys in locked RAM, served over MCP, destroyed after N reads or on session death.",
  "repository": {
    "url": "https://github.com/New1Direction/ningen-shikkaku",
    "source": "github"
  },
  "version": "0.1.0",
  "packages": []
}
```

Note: the registry favors npm/pypi/docker/nuget package types; binary-only
distribution may need the `remotes`/package workarounds or a wrapper package.
Check the current schema when submitting — if binary distribution is awkward,
list it on the community registries first and revisit.

## 2. Smithery (smithery.ai)

Mechanism: sign in with GitHub, "Add server", point at the repo. Local/stdio
servers are listed with their launch command. Use the motokano one-shot
command as the canonical example (it demos without a running daemon).

## 3. mcp.so

Mechanism: community directory; submission is a GitHub issue/PR on
chatmcp/mcp-directory (or the "Submit" form on the site). Provide repo URL +
the shared copy above.

## 4. Bonus surfaces (same copy, five minutes each)

- `awesome-mcp-servers` lists (punkpeye/awesome-mcp-servers — PR with one line
  under Security).
- r/mcp weekly server thread.
