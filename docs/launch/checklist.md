# Launch-week checklist

In dependency order. Everything above the Show HN line must be green before
posting anywhere.

## Release plumbing

- [x] **Create the Homebrew tap repo** (must exist before the first tag):
  done 2026-06-11 → https://github.com/New1Direction/homebrew-tap

- [x] **Add the tap token secret**: done 2026-06-11 — fine-grained PAT
  (`contents: write` on the tap only) stored as `HOMEBREW_TAP_TOKEN` on
  ningen-shikkaku. Earlier leaked tokens revoked.

- [x] **Push main**: done 2026-06-11, CI green.
- [x] **Tag and release**: `v0.1.0` released 2026-06-11 — all 4 targets,
  installers + checksums live.
- [x] **Formulas in the tap**: pushed manually for v0.1.0 (the PAT couldn't
  write to the tap; publish job 403'd twice).
  **⚠ Before the next release: fix the `HOMEBREW_TAP_TOKEN` PAT** —
  Repository access must include `New1Direction/homebrew-tap` AND
  Permissions → Contents = Read and write — or the publish job will fail
  again and the formulas will go stale.
- [x] **Install test**: `brew install New1Direction/tap/{dazai,motokano}`
  verified 2026-06-11 — binaries 0.1.0 on PATH, burn demo passes against the
  brew-installed motokano.
- [ ] **Clean-machine install test** (at minimum: a fresh shell on another
  Mac/Linux box or container):

  ```bash
  curl --proto '=https' --tlsv1.2 -LsSf https://github.com/New1Direction/ningen-shikkaku/releases/latest/download/dazai-installer.sh | sh
  brew install New1Direction/tap/motokano
  motokano --calls 1 --tool 'name=t,kind=static,value=ok' --arm   # then drive it once
  ```

- [ ] **Verify the docs site deployed** with the new landing page + GIF.
- [ ] **README top-to-bottom read** on github.com (GIF renders, links work).

## Listings

- [x] **Official MCP registry**: `io.github.New1Direction/motokano` 0.1.0
  published 2026-06-11 (MCPB bundle `motokano-0.1.0.mcpb` attached to the
  release; `server.json` at repo root). Future releases: build the new .mcpb,
  update version + fileSha256 in server.json, `mcp-publisher login github &&
  mcp-publisher publish`.
- [x] **smithery.yaml** committed (local stdio config). Smithery's web form
  only takes hosted HTTP servers — dazai is local-only by design, so no
  hosted listing; aggregators mirror the official registry anyway.
- [x] **mcp.so**: submission filed 2026-06-11 →
  https://github.com/chatmcp/mcpso/issues/2760
- [x] **awesome-mcp-servers**: PR filed 2026-06-11 (agent fast-track) →
  https://github.com/punkpeye/awesome-mcp-servers/pull/7879

## ---------------- Show HN line ----------------

- [ ] **Show HN** per [show-hn.md](show-hn.md). Tue–Thu, 8–10am US Eastern.
  Stay in the comments 3–4 hours.
- [ ] **r/rust** same day, link post + a comment with the seccomp/mlock
  design notes (r/rust loves the unsafe-confined-to-one-crate story).
- [ ] **lobste.rs** same day (tags: security, rust).
- [ ] **r/mcp / r/ClaudeAI** the day after (the `claude mcp add` one-liner is
  the hook there).

## After

- [ ] Pin a "roadmap" issue: signing proxy (agent never sees the key),
  session-native liveness (logind/launchd), e-stop for agent fleets.
- [ ] Respond to every issue within 24h during launch week.
