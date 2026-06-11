#!/usr/bin/env python3
"""Drive motokano's burn-after-reading lifecycle, end to end.

Spawns `motokano --calls 1 --arm` with one static secret, performs the MCP
handshake over stdio, reads the secret once, then shows the second read
failing because the server wiped and killed itself.

Stdlib only. Doubles as a smoke test for the release binary.
"""
import json
import shutil
import subprocess
import sys
import time

MOTOKANO = shutil.which("motokano") or "rs/target/release/motokano"
SECRET = "s3cr3t-k3y"
HANDSHAKE_TIMEOUT = 5.0
DEAD_WAIT = 5.0


def rpc(proc, payload):
    proc.stdin.write((json.dumps(payload) + "\n").encode())
    proc.stdin.flush()


def read_response(proc, timeout=HANDSHAKE_TIMEOUT):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        line = proc.stdout.readline()
        if not line:
            return None  # EOF: the server is gone
        line = line.strip()
        if line:
            return json.loads(line)
    return None


def main():
    proc = subprocess.Popen(
        [MOTOKANO, "--calls", "1", "--arm",
         "--tool", f"name=get_key,kind=static,value={SECRET}"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
    )
    # handshake — shapes mirror rs/crates/motokano/tests/lifecycle.rs
    rpc(proc, {"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2025-03-26",
                          "capabilities": {},
                          "clientInfo": {"name": "burn-demo", "version": "0"}}})
    read_response(proc)
    rpc(proc, {"jsonrpc": "2.0", "method": "notifications/initialized"})

    print("$ agent calls get_key  (1st time)")
    rpc(proc, {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "get_key", "arguments": {}}})
    resp = read_response(proc)
    secret = resp["result"]["content"][0]["text"]
    print(f"  -> {secret}")

    time.sleep(1.0)  # let the wipe + SIGKILL land
    print("$ agent calls get_key  (2nd time)")
    server_dead = False
    try:
        rpc(proc, {"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                   "params": {"name": "get_key", "arguments": {}}})
        server_dead = read_response(proc, timeout=2.0) is None
    except BrokenPipeError:
        server_dead = True
    print("  -> no response: the value was wiped from locked memory")

    rc = proc.wait(timeout=DEAD_WAIT)
    print(f"$ kill -0 {proc.pid}")
    print(f"  -> no such process (SIGKILLed itself, status {rc})")

    if not server_dead:
        print("FAIL: server answered a second call — burn failed", file=sys.stderr)
        return 1
    if rc != -9:
        print(f"FAIL: expected death by SIGKILL (-9), got {rc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
