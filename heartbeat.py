#!/usr/bin/env python3
"""dazai heartbeat client -- holds the liveness connection for a session.

It connects to the daemon's UNIX socket, says HELLO, and then keeps the
connection open. Optionally it sends a periodic PING so a daemon configured
with ``--ping-timeout`` stays satisfied. When this process dies -- because the
shell that launched it exited and the rc `trap` killed it -- the socket closes,
the daemon sees the heartbeat drop, and (if armed) self-destructs.

Run it in the background from your shell rc; see shellrc.sh.
"""

from __future__ import annotations

import argparse
import os
import signal
import socket
import sys
import time


def _default_socket_path() -> str:
    base = os.environ.get("XDG_RUNTIME_DIR") or "/tmp"
    return os.path.join(base, f"deadman-{os.getuid()}.sock")


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="dazai heartbeat client")
    p.add_argument("--socket", default=_default_socket_path(),
                   help="daemon UNIX socket path (default: %(default)s)")
    p.add_argument("--interval", type=float, default=0.0, metavar="SECONDS",
                   help="send PING every N seconds; 0 = just hold the "
                        "connection open (default: %(default)s)")
    p.add_argument("-q", "--quiet", action="store_true", help="suppress stdout")
    opts = p.parse_args(argv)

    def say(msg: str) -> None:
        if not opts.quiet:
            print(f"[heartbeat] {msg}", flush=True)

    try:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(opts.socket)
    except OSError as exc:
        print(f"[heartbeat] cannot connect to {opts.socket}: {exc}", file=sys.stderr)
        return 1

    # Closing the socket cleanly on SIGTERM lets the daemon distinguish a
    # killed heartbeat (connection drop -> panic) without relying on the OS
    # to reap the fd. We just exit; the kernel closes the fd either way.
    def _bye(signum, frame):
        try:
            sock.close()
        finally:
            os._exit(0)

    signal.signal(signal.SIGTERM, _bye)
    signal.signal(signal.SIGINT, _bye)

    sock.sendall(b"HELLO %d\n" % os.getpid())
    try:
        reply = sock.recv(64)
    except OSError:
        reply = b""
    say(f"connected to {opts.socket}: {reply.decode(errors='replace').strip()}")

    try:
        if opts.interval > 0:
            while True:
                time.sleep(opts.interval)
                sock.sendall(b"PING\n")
                if not sock.recv(64):  # daemon went away
                    say("daemon closed the connection")
                    return 0
        else:
            # Block until the connection closes from the other side.
            while sock.recv(64):
                pass
            say("daemon closed the connection")
    except (OSError, KeyboardInterrupt):
        pass
    finally:
        try:
            sock.close()
        except OSError:
            pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
