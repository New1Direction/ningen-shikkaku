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

**Secrets that live only as long as you do** — a session-bound, memory-zeroizing dead-man's-switch that pins secrets in locked, non-swappable RAM and, the instant your session dies, wipes them and `SIGKILL`s the processes holding them.

## Install

```bash
git clone https://github.com/New1Direction/ningen-shikkaku
cd ningen-shikkaku/rs
cargo build --release          # → target/release/{dazai, motokano}
```

## See it in one line

```bash
motokano --calls 1 --tool 'name=get_key,kind=static,value=s3cr3t' --arm
```

Point any MCP client at it and call `get_key` **once** → you receive `s3cr3t` → it wipes the value out of locked memory and `SIGKILL`s itself. Call again → the process is gone.

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
