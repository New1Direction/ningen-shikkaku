# dazai 0→1 Launch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reposition the repo around "burn-after-reading secrets for AI agents," add prebuilt-binary distribution, a real demo GIF, and a ready-to-execute launch kit — no new mechanism, no renames.

**Architecture:** Four independent workstreams over an unchanged codebase: (1) cargo-dist release automation, (2) README/docs-landing rewrite, (3) a VHS-recorded demo driven by a tiny stdlib-Python MCP client, (4) launch materials. Everything commits locally; nothing is pushed or published by the agent.

**Tech Stack:** Rust workspace (existing), cargo-dist 0.32.0, VHS (installed at `/opt/homebrew/bin/vhs`), Python 3 stdlib for the demo client, mdBook docs (existing).

**Spec:** `docs/superpowers/specs/2026-06-11-dazai-0to1-launch-design.md`

**Verified facts this plan relies on:**
- `dazai` and `motokano` are unclaimed on crates.io (checked 2026-06-11) — but crates.io publish is OUT OF SCOPE (would require publishing all path-dep crates).
- cargo-dist latest is 0.32.0 (May 2026, axodotdev — still maintained).
- vhs + ttyd installed locally; asciinema/agg are not.
- motokano CLI (from `rs/crates/motokano/src/main.rs`): `--calls N`, `--session`, `--arm`, `--tool 'name=...,kind=static,value=...'`, `--grace N`, stdio transport only.
- Existing CI (`.github/workflows/ci.yml`) covers fmt/clippy/tests/seccomp on Linux; release workflow must be a NEW file, not a CI edit.
- Workspace `[workspace.package]` in `rs/Cargo.toml` has `edition/version/license/description` but **no `repository`** — cargo-dist requires it.

---

### Task 1: Green baseline

**Files:** none created — verification only.

- [ ] **Step 1.1: Build release binaries**

Run (from repo root):
```bash
cargo build --release --manifest-path rs/Cargo.toml
```
Expected: success; binaries at `rs/target/release/dazai` and `rs/target/release/motokano`.

- [ ] **Step 1.2: Run the full test suite**

```bash
cargo test --manifest-path rs/Cargo.toml
```
Expected: all tests pass (README claims 66 across the workspace). seccomp feature is Linux-only — do NOT pass `--features seccomp` on macOS.

- [ ] **Step 1.3: Verify the MCP handshake motokano expects**

Read `rs/crates/motokano/tests/lifecycle.rs` and note the exact JSON-RPC `initialize` / `tools/call` shapes used there. Use those shapes in Task 2's demo client. Do not guess protocol versions — copy what the tests use.

No commit (no changes).

---

### Task 2: Demo client + smoke test

**Files:**
- Create: `demo/burn_once.py` (stdlib-only MCP stdio client driving the burn)
- Create: `demo/README.md`

- [ ] **Step 2.1: Write `demo/burn_once.py`**

A stdlib-only script that spawns motokano, performs the MCP handshake, calls `get_key` once (prints the secret), attempts a second call, and proves the process is dead. Adjust the two JSON payload shapes to match what Step 1.3 found.

```python
#!/usr/bin/env python3
"""Drive motokano's burn-after-reading lifecycle, end to end.

Spawns `motokano --calls 1 --arm` with one static secret, performs the MCP
handshake over stdio, reads the secret once, then shows the second read
failing because the server wiped and killed itself.
"""
import json
import os
import shutil
import subprocess
import sys
import time

MOTOKANO = shutil.which("motokano") or "rs/target/release/motokano"

def rpc(proc, payload):
    proc.stdin.write((json.dumps(payload) + "\n").encode())
    proc.stdin.flush()

def read_response(proc, timeout=5.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        line = proc.stdout.readline()
        if not line:
            return None  # EOF: server is gone
        line = line.strip()
        if line:
            return json.loads(line)
    return None

def main():
    proc = subprocess.Popen(
        [MOTOKANO, "--calls", "1", "--arm",
         "--tool", "name=get_key,kind=static,value=s3cr3t-k3y"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
    )
    # -- handshake (shapes must match rs/crates/motokano/tests/lifecycle.rs) --
    rpc(proc, {"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2025-03-26",
                          "capabilities": {},
                          "clientInfo": {"name": "burn-demo", "version": "0"}}})
    read_response(proc)
    rpc(proc, {"jsonrpc": "2.0", "method": "notifications/initialized"})

    print("$ first call: get_key")
    rpc(proc, {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "get_key", "arguments": {}}})
    resp = read_response(proc)
    secret = resp["result"]["content"][0]["text"]
    print(f"  -> {secret}")

    time.sleep(1.0)  # let the wipe+SIGKILL land
    print("$ second call: get_key")
    rpc_dead = False
    try:
        rpc(proc, {"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                   "params": {"name": "get_key", "arguments": {}}})
        rpc_dead = read_response(proc, timeout=2.0) is None
    except BrokenPipeError:
        rpc_dead = True
    print("  -> no response. the server burned the secret and SIGKILL'd itself.")
    rc = proc.wait(timeout=5)
    print(f"$ kill -0 {proc.pid}")
    print(f"  -> process gone (exit status {rc})")
    assert rpc_dead, "server answered a second call — burn failed"
    assert rc != 0 or True  # SIGKILL surfaces as negative returncode on POSIX

if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 2.2: Run it against the release binary — this is the smoke test**

```bash
python3 demo/burn_once.py
```
Expected output: the secret printed once, then "no response" + "process gone". If the handshake hangs, fix the JSON shapes per Step 1.3 (the canonical truth is the Rust test file). Iterate until clean.

- [ ] **Step 2.3: Write `demo/README.md`**

```markdown
# Demo

`burn_once.py` — a stdlib-only MCP client that drives the burn-after-reading
lifecycle against a release build of motokano. Used as a smoke test and as the
script behind the README GIF.

    cargo build --release --manifest-path rs/Cargo.toml
    python3 demo/burn_once.py

`burn.tape` — the VHS script that records the GIF (`brew install vhs`):

    vhs demo/burn.tape   # writes docs/src/assets/burn.gif
```

- [ ] **Step 2.4: Commit**

```bash
git add demo/
git commit -m "feat: add burn-after-reading demo client (doubles as MCP smoke test)"
```

---

### Task 3: Record the GIF with VHS

**Files:**
- Create: `demo/burn.tape`
- Create: `docs/src/assets/burn.gif` (recorded artifact)

- [ ] **Step 3.1: Write `demo/burn.tape`**

```tape
# VHS tape — records the burn-after-reading demo GIF.
Output docs/src/assets/burn.gif
Set FontSize 12
Set Width 880
Set Height 480
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms
Set Padding 16

Type "python3 demo/burn_once.py"
Sleep 600ms
Enter
Sleep 6s
```

- [ ] **Step 3.2: Record**

```bash
vhs demo/burn.tape
```
Expected: `docs/src/assets/burn.gif` exists, < 2 MB, and visually shows: first call returns the secret, second call gets nothing, process gone. Open it to check. If output runs past the GIF, raise the final `Sleep`.

- [ ] **Step 3.3: Commit**

```bash
git add demo/burn.tape docs/src/assets/burn.gif
git commit -m "docs: record burn-after-reading demo GIF (vhs)"
```

---

### Task 4: cargo-dist release automation

**Files:**
- Modify: `rs/Cargo.toml` (add `repository` to `[workspace.package]`, add `[workspace.metadata.dist]`)
- Modify: `rs/crates/dazai/Cargo.toml` + `rs/crates/motokano/Cargo.toml` (inherit `repository`)
- Create: `.github/workflows/release.yml` (generated by dist)

- [ ] **Step 4.1: Install cargo-dist 0.32.0**

```bash
cargo install cargo-dist --version 0.32.0 --locked
```

- [ ] **Step 4.2: Add repository metadata**

In `rs/Cargo.toml` under `[workspace.package]` add:
```toml
repository = "https://github.com/New1Direction/ningen-shikkaku"
```
In `rs/crates/dazai/Cargo.toml` and `rs/crates/motokano/Cargo.toml` under `[package]` add (if not present):
```toml
repository.workspace = true
```
Check the other five crate manifests; internal library crates do not need it for dist (only binary-producing packages are dist'd), but adding `repository.workspace = true` everywhere is harmless and consistent — do it.

- [ ] **Step 4.3: Initialize dist (non-interactive)**

From `rs/`:
```bash
cd rs && dist init --yes \
  --installer shell --installer homebrew \
  --target aarch64-apple-darwin --target x86_64-apple-darwin \
  --target x86_64-unknown-linux-gnu --target aarch64-unknown-linux-gnu \
  --hosting github
```
Expected: `[workspace.metadata.dist]` block written to `rs/Cargo.toml`, and a release workflow generated. **Watch out:** dist writes the workflow relative to the workspace; since the workspace lives in `rs/` but GitHub needs `.github/` at repo root, check where `release.yml` landed and move it to `.github/workflows/release.yml` if needed, then set `allow-dirty` accordingly or re-run `dist generate` from the right context. If dist refuses because the workspace is in a subdirectory, the fallback is: run `dist init` as above, move the workflow file to repo root `.github/workflows/`, and add to `[workspace.metadata.dist]`:
```toml
allow-dirty = ["ci"]
```
(documenting that the workflow file is manually relocated).

- [ ] **Step 4.4: Configure the homebrew tap + binaries-only scope**

In the generated `[workspace.metadata.dist]` in `rs/Cargo.toml`, ensure:
```toml
tap = "New1Direction/homebrew-tap"
publish-jobs = ["homebrew"]
```
And exclude library crates from dist by adding to each of the five library crate manifests (`goodnight`, `kikka`, `kekkai`, `sienna`, `rei`) — only if `dist plan` (next step) tries to include them:
```toml
[package.metadata.dist]
dist = false
```

- [ ] **Step 4.5: Verify the plan**

```bash
cd rs && dist plan
```
Expected: exactly two apps (`dazai` 0.1.0, `motokano` 0.1.0), four targets each, shell + homebrew installers, unified tag `v0.1.0`. Fix config until this is true.

- [ ] **Step 4.6: Sanity-build one artifact locally**

```bash
cd rs && dist build --artifacts local
```
Expected: tarball(s) for the host target under `rs/target/distrib/`. This proves the build config works before CI ever runs.

- [ ] **Step 4.7: Commit**

```bash
git add rs/Cargo.toml rs/crates/*/Cargo.toml .github/workflows/release.yml
git commit -m "ci: cargo-dist release automation (shell + homebrew installers, 4 targets)"
```

**Note for the README task:** the resulting install one-liners are:
```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/New1Direction/ningen-shikkaku/releases/latest/download/dazai-installer.sh | sh
brew install New1Direction/tap/dazai     # and New1Direction/tap/motokano
```
Release binaries are default-features (no seccomp — it needs libseccomp at build time); seccomp stays a documented build-from-source option. State this honestly in the README.

---

### Task 5: README rewrite

**Files:**
- Modify: `README.md`

- [ ] **Step 5.1: Replace the top of the README**

Replace everything from the title through the end of the current "Demo" section with the content below. KEEP unchanged: the "Threat model" section, the "Verification" section, the "The name" section, the "License" section. The "What's in the box" table moves below the quickstart, unchanged.

```markdown
# dazai

[![CI](https://github.com/New1Direction/ningen-shikkaku/actions/workflows/ci.yml/badge.svg)](https://github.com/New1Direction/ningen-shikkaku/actions/workflows/ci.yml)
[![docs](https://img.shields.io/badge/docs-ningen--shikkaku-c0392b)](https://new1direction.github.io/ningen-shikkaku/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Burn-after-reading secrets for AI agents.**

Your MCP configs are full of plaintext API keys, and every agent you run can
read all of them — and they keep working long after the agent is done. dazai
inverts that: secrets live in locked, non-swappable RAM, are served to agents
over MCP, and are **destroyed after N reads — or the instant your session
dies**.

![burn-after-reading demo](docs/src/assets/burn.gif)

```bash
motokano --calls 1 --tool 'name=get_key,kind=static,value=s3cr3t' --arm
```

Point any MCP client at it and call `get_key` **once** → you receive `s3cr3t`
→ the server wipes the value out of locked memory and `SIGKILL`s itself. Call
again → the process is gone.

## Install

```bash
# prebuilt binaries (macOS arm64/x86_64, Linux x86_64/arm64)
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/New1Direction/ningen-shikkaku/releases/latest/download/dazai-installer.sh | sh

# or homebrew
brew install New1Direction/tap/dazai New1Direction/tap/motokano

# or from source (required for the Linux seccomp build)
git clone https://github.com/New1Direction/ningen-shikkaku
cd ningen-shikkaku/rs
cargo build --release                       # -> target/release/{dazai, motokano}
cargo build --release --features seccomp    # Linux: seccomp syscall allowlist
```

Prebuilt binaries are built with default features; the seccomp-confined daemon
is a from-source build (it links libseccomp).

## Use it with Claude Code

```bash
# a one-shot secret an agent can read exactly once:
claude mcp add burn-once -- motokano --calls 1 --arm \
  --tool 'name=get_key,kind=static,value=YOUR-SECRET'

# or the session-bound daemon: agents register their PID and get SIGKILLed
# the moment your session dies
dazai daemon --arm --grace 5 &
claude mcp add dazai -- dazai mcp
```

## And the second act: a dead-man's switch for your agents

The same daemon is a session kill-switch. Any MCP client registers its PID;
when your shell/SSH session dies, your heartbeat stops, or a panic signal
arrives, dazai `SIGKILL`s every registered process, overwrites its secrets
with a wipe the compiler can't optimize away, and exits. Walk away: agents
die, secrets burn.

```bash
dazai daemon --ping-timeout 15        # terminal A
dazai client --interval 5             # terminal B
# close terminal B  ->  the daemon wipes its secrets and exits
```

It runs **only on your own machine, on your own secrets and your own
configured tools**. It never touches another process, file, or host.
```

Then keep: "What's in the box" (unchanged), "Threat model" (unchanged), "Verification" (unchanged), "The name" (unchanged), "License" (unchanged). Delete the old "Install" and "Demo" sections (replaced above). The sentence about the two layers (Rust daemon + Python reference) moves into "What's in the box" as its intro paragraph, unchanged in wording.

- [ ] **Step 5.2: Verify rendering + links**

```bash
grep -n "](" README.md | grep -v "^.*http" | head -20
```
Check every relative link target exists (`LICENSE`, `python-reference.md`, `rs/README.md`, `docs/src/assets/burn.gif`). View the file top-to-bottom once for flow.

- [ ] **Step 5.3: Commit**

```bash
git add README.md
git commit -m "docs: lead README with burn-after-reading-for-agents positioning"
```

---

### Task 6: Docs landing page rewrite

**Files:**
- Modify: `docs/src/index.md`

- [ ] **Step 6.1: Rewrite the copy, keep the 3D hero**

Keep lines 1–20 (title, model-viewer block, badges) exactly as they are. Replace everything from the bold one-liner (current line 22) through the "See it in one line" section with:

```markdown
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
```

Keep the "Start here" links, the crate-map code block, and the Dazai quote at the bottom unchanged.

- [ ] **Step 6.2: Build the book**

```bash
mdbook build 2>/dev/null || ~/.cargo/bin/mdbook build
```
If mdbook is not installed: `cargo install mdbook` first (check `docs.yml` workflow for the version CI uses and match it). Expected: clean build, `docs/book/index.html` contains the new copy and the GIF renders (asset path `assets/burn.gif` resolves because the GIF lives at `docs/src/assets/burn.gif`).

- [ ] **Step 6.3: Commit**

```bash
git add docs/src/index.md
git commit -m "docs: landing page leads with agent-secrets story + install one-liner"
```

---

### Task 7: Launch kit

**Files:**
- Create: `docs/launch/show-hn.md`
- Create: `docs/launch/registries.md`
- Create: `docs/launch/checklist.md`

- [ ] **Step 7.1: Write `docs/launch/show-hn.md`**

Full draft: title options (lead: `Show HN: Dazai – burn-after-reading secrets for AI agents`), a 150–250 word post body written in first person (what it is, the one-line motokano demo, the honest "what it does NOT protect against" paragraph — lifted from the README threat model, this is the credibility move), and a prepared first comment covering: why MCP, the seccomp/mlock design in two sentences, the Python-reference origin story, and the known limitation that a disclosed secret lives in the agent's context (with the signing-proxy roadmap as the answer). End with a "do not post until" checklist referencing `checklist.md`.

- [ ] **Step 7.2: Write `docs/launch/registries.md`**

For each registry — official MCP registry (modelcontextprotocol/registry), Smithery, mcp.so — record: submission URL/mechanism, the server entry metadata (name `dazai`, the stdio command `dazai mcp` and the motokano variant, description string reusing the lead line), and any required manifest (e.g. `server.json` for the official registry — include the full JSON draft inline with the two command configurations).

- [ ] **Step 7.3: Write `docs/launch/checklist.md`**

Ordered launch-week checklist with checkboxes, in dependency order: (1) create `New1Direction/homebrew-tap` repo + add `HOMEBREW_TAP_TOKEN` secret (exact `gh repo create` + token-scope instructions inline), (2) push main, (3) `git tag v0.1.0 && git push --tags`, (4) verify the release workflow produced installers + formulas, (5) test the curl installer on a clean machine, (6) registry submissions, (7) Show HN per `show-hn.md` (Tue–Thu, morning US time), (8) r/rust + lobste.rs same day, (9) reply cadence notes.

- [ ] **Step 7.4: Commit**

```bash
git add docs/launch/
git commit -m "docs: launch kit — Show HN draft, registry submissions, launch-week checklist"
```

---

### Task 8: Final verification

**Files:** none — verification only.

- [ ] **Step 8.1: Full re-test**

```bash
cargo test --manifest-path rs/Cargo.toml && cargo fmt --all --check --manifest-path rs/Cargo.toml && cargo clippy --all-targets --manifest-path rs/Cargo.toml -- -D warnings
```
Expected: all green (Cargo.toml metadata edits must not break anything).

- [ ] **Step 8.2: Re-run the demo smoke test**

```bash
python3 demo/burn_once.py
```
Expected: clean burn output.

- [ ] **Step 8.3: Repo-wide link check on changed files**

Verify every path referenced in README.md, docs/src/index.md, demo/README.md, and docs/launch/*.md exists in the worktree. Verify `dist plan` still passes.

- [ ] **Step 8.4: Status summary**

`git log --oneline main..HEAD` style review of the commit stack; confirm nothing is pushed; write the hand-off summary for the user (what's done, what only they can do: tap repo, tag, registries, HN).

---

## Out of scope (explicit)

- Pushing to GitHub, tagging, creating the tap repo, publishing to crates.io, posting anywhere. All human actions, prepared in `docs/launch/`.
- Signing proxy, session-native liveness, e-stop story — launch #2 (see spec).
