# Launch-week checklist

In dependency order. Everything above the Show HN line must be green before
posting anywhere.

## Release plumbing

- [x] **Create the Homebrew tap repo** (must exist before the first tag):
  done 2026-06-11 → https://github.com/New1Direction/homebrew-tap

- [x] **Add the tap token secret**: done 2026-06-11 — fine-grained PAT
  (`contents: write` on the tap only) stored as `HOMEBREW_TAP_TOKEN` on
  ningen-shikkaku. Earlier leaked tokens revoked.

- [ ] **Push main** (all launch commits): `git push origin main`
- [ ] **Verify CI is green** on the pushed commit.
- [ ] **Tag and release**:

  ```bash
  git tag v0.1.0 && git push origin v0.1.0
  ```

- [ ] **Verify the release workflow**: GitHub Release `v0.1.0` exists with
  `dazai-installer.sh`, `motokano-installer.sh`, 4 tarballs per app +
  checksums; `dazai.rb` + `motokano.rb` landed in the tap.
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

- [ ] Registry submissions per [registries.md](registries.md) (official,
  Smithery, mcp.so, awesome-mcp-servers PR).

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
