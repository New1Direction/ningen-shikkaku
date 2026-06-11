# dazai 0→1 launch — design

**Date:** 2026-06-11
**Status:** Approved (conversational review)

## Goal

Take the project from "release-ready engineering nobody has seen" to "tool people
adopt." The leverage is repositioning and distribution, not new mechanism. The
target outcome the user chose: **maximum leverage and popularity**.

## Decisions made

| Decision | Choice | Rationale |
|---|---|---|
| Direction | Agent-security positioning | Hottest underserved niche; working code already exists (MCP layer, motokano); 0→1 cost is mostly copy + packaging |
| Lead story | **Burn-after-reading secrets for AI agents** | Practically adoptable day-to-day; kill-switch is the dramatic supporting act |
| Hero name | **dazai** | Short, typeable, already the CLI binary; free on crates.io (verified 2026-06-11; `motokano` also free). Repo stays `ningen-shikkaku`; motokano stays as a binary, presented as "dazai's self-immolating one-shot server" |
| Lore | Below the fold | Brand asset, not the opener |

## Positioning

One line fronting every public surface:

> **dazai — burn-after-reading secrets for AI agents.** Your keys live in
> locked, non-swappable RAM, are served to agents over MCP, and are destroyed
> after N reads — or the instant your session dies.

Second act: the session kill-switch (registered agent PIDs are SIGKILLed when
the operator's session dies). The honest threat model stays prominent — it is
the project's credibility signature.

## Workstreams

### A. Distribution (highest leverage)

- Release automation (cargo-dist or equivalent current tooling): GitHub
  Releases with prebuilt binaries (macOS arm64/x86_64, Linux x86_64/arm64),
  Homebrew tap config, curl install script. Ready-to-tag; actual publish is a
  human action.
- `claude mcp add` one-liners for `dazai mcp` and `motokano` in the README.
- MCP registry listings prepared: official directory, Smithery, mcp.so.
- crates.io publish deferred: requires publishing all internal path-dep crates;
  bare names (`rei`, `goodnight`, …) are likely squatted. If/when wanted,
  internal crates get `dazai-` prefixes. Binary distribution does not block on
  this.

### B. Public surfaces

- README: new first screen (lead line, demo GIF, install one-liner,
  `claude mcp add`), kill-switch second, threat model kept, lore at the bottom.
- Docs landing page (`docs/src/index.md`) re-led with the same story.

### C. Demo

- Scripted demo: agent calls `get_key` → receives secret → calls again →
  server is gone. VHS tape (or asciinema script) committed; GIF recorded if
  tooling is available locally, otherwise instructions committed alongside.

### D. Launch week (materials prepared, human executes)

- Show HN draft: title ≈ "Show HN: Dazai – burn-after-reading secrets for AI
  agents", post body, first-comment draft.
- Registry submissions, r/rust + lobste.rs notes, launch checklist.

## Deliberately deferred (launch #2 material)

- **Signing proxy** — the agent never sees the key; converts the documented
  context/VRAM-leak limitation into the next headline feature.
- **Session-native liveness** — systemd-logind / launchd / PAM integration; no
  heartbeat client needed.
- **E-stop for agent fleets** as its own story.

## Verification

- Clean build + full test suite green before and after surface changes.
- Demo claims verified against a real run of motokano.
- Install instructions match what the release tooling actually produces.
- Link/badge check on all rewritten surfaces.

## Non-goals

- No new mechanism, no protocol changes, no renames of code or crates.
- No actual publishing (tags, releases, registry submissions, HN post) — all
  outward-facing actions are prepared as ready-to-execute materials for the
  user.
