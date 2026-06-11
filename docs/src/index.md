# 人間失格

<!-- interactive 3D hero — model-viewer renders the glb in-browser (orbit/drag to inspect). -->
<script type="module" src="https://ajax.googleapis.com/ajax/libs/model-viewer/4.0.0/model-viewer.min.js"></script>
<model-viewer
  src="assets/hero.glb"
  alt="ningen-shikkaku"
  camera-controls
  auto-rotate
  auto-rotate-delay="0"
  rotation-per-second="18deg"
  interaction-prompt="none"
  shadow-intensity="0.6"
  exposure="0.9"
  style="width:100%; aspect-ratio:16/10; background:#080808; --poster-color:#080808;">
</model-viewer>

[![CI](https://github.com/New1Direction/ningen-shikkaku/actions/workflows/ci.yml/badge.svg)](https://github.com/New1Direction/ningen-shikkaku/actions/workflows/ci.yml)
[![docs](https://img.shields.io/badge/docs-ningen--shikkaku-c0392b)](https://new1direction.github.io/ningen-shikkaku/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/New1Direction/ningen-shikkaku/blob/main/LICENSE)

**Burn-after-reading secrets for AI agents** — keys live in locked,
non-swappable RAM, are served to agents over MCP, and are destroyed after N
reads, or the instant your session dies. Walk away: agents die, secrets burn.

## Install

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/New1Direction/ningen-shikkaku/releases/latest/download/dazai-installer.sh | sh
# or: brew install New1Direction/tap/dazai New1Direction/tap/motokano
```

## See it burn

![burn-after-reading demo](assets/burn.gif)

```bash
motokano --calls 1 --tool 'name=get_key,kind=static,value=s3cr3t' --arm
```

One read → the secret. Second read → the server has already wiped the value
out of locked memory and `SIGKILL`ed itself.

## Wire it into an agent

```bash
claude mcp add burn-once -- motokano --calls 1 --arm \
  --tool 'name=get_key,kind=static,value=YOUR-SECRET'
```

## Start here

- [**Quickstart**](./guides/quickstart.md) — install, rehearse in dry-run, then arm
- [**Threat model**](./threat-model.md) — exactly what it protects against, and what it doesn't
- [**GitHub**](https://github.com/New1Direction/ningen-shikkaku) — source, issues, CI

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

> *Mine has been a life of much shame.*
> *No longer human. No longer here.*
>
> — Osamu Dazai, 1948
