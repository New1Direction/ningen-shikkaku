# Demo

`burn_once.py` — a stdlib-only MCP client that drives the burn-after-reading
lifecycle against a release build of motokano. Used as a smoke test and as the
script behind the README GIF.

```bash
cargo build --release --manifest-path rs/Cargo.toml
PATH="$PWD/rs/target/release:$PATH" python3 demo/burn_once.py
```

Expected: the secret is served exactly once; the second call gets no response;
the process is gone (death by SIGKILL — exit status `-9`).

`burn.tape` — the VHS script that records the GIF (`brew install vhs`):

```bash
vhs demo/burn.tape   # writes docs/src/assets/burn.gif
```
