#!/usr/bin/env python3
"""dazai -- a session-bound, memory-zeroizing dead-man's-switch daemon.

The daemon:

  1. Allocates synthetic "working buffers" in page-locked RAM (see secmem).
  2. Listens on a UNIX domain socket. A connected client is the *heartbeat*:
     while it is connected the daemon considers itself guarded/live.
  3. On a trigger -- the heartbeat connection dropping, a missed ping deadline,
     or a panic signal -- it zeroizes every buffer (ctypes.memset) and, when
     ARMED, hard-kills itself with SIGKILL.

Triggers
--------
* heartbeat connection dropped (EOF)  -> graceful panic
* --ping-timeout deadline missed       -> graceful panic
* SIGUSR1                              -> graceful panic (self-pipe -> main loop)
* SIGUSR2                              -> HARD panic: minimal in-handler
                                          ctypes.memset + os.kill(getpid, SIGKILL)
* SIGTERM / SIGINT                     -> clean shutdown (zeroize + exit 0)

Safety
------
Default mode is DRY-RUN: a trigger still runs the real memset zeroization and
logs ``WOULD SIGKILL``, but does NOT kill -- so you can exercise the whole path
safely. ``--arm`` enables a real self-destruct, and graceful triggers then route
through a cancellable ``--grace`` window (reconnect or a CANCEL line aborts).
The lethal SIGKILL is injected as a callable so tests drive the full
decide/zeroize path with a fake killer instead of actually dying.

Wire protocol (newline-delimited text over SOCK_STREAM)
-------------------------------------------------------
  client -> HELLO <pid>   daemon -> WELCOME
  client -> PING          daemon -> PONG     (refreshes ping deadline)
  client -> CANCEL        daemon -> CANCELLED (aborts a pending armed panic)
  client -> QUIT          daemon -> BYE       (intentional clean stand-down)
  connection closed       -> heartbeat lost -> panic
"""

from __future__ import annotations

import argparse
import errno
import logging
import os
import select
import signal
import socket
import sys
import time
from typing import Callable, List, Optional

import secmem

log = logging.getLogger("dazai.deadman")

# Largest line we will buffer from a client before treating it as garbage, so a
# peer that never sends a newline cannot grow daemon memory without bound.
_MAX_LINE = 8192

# Read by the minimal SIGUSR2 hard-panic handler. A module global (rather than
# instance state) so the handler -- which must stay tiny and async-signal-safe
# -- needs no object lookups. Set once in main().
ARMED = False

# Self-pipe: signal handlers write a 1-byte code; the select() loop drains it.
# This keeps the (potentially non-reentrant) panic logic out of handler context.
_pipe_r = -1
_pipe_w = -1


# --------------------------------------------------------------------------
# Panic decision logic -- pure-ish and unit-testable (no sockets, no signals).
# --------------------------------------------------------------------------
class PanicController:
    """Decides what a trigger does given the arming/dry-run/grace policy.

    Collaborators are injected so this class is fully testable with fakes:

    * ``zeroize()``  -- wipe the working buffers (real: secmem.zeroize_all).
    * ``kill()``     -- lethal action (real: os.kill(getpid, SIGKILL)). Only
                        ever called when armed.
    * ``dry_done()`` -- called after a DRY-RUN wipe so the host can wind down
                        (real: stop the daemon loop and exit 0).
    * ``clock()``    -- monotonic seconds (real: time.monotonic).
    * ``emit(msg)``  -- log sink.
    """

    def __init__(
        self,
        *,
        armed: bool,
        grace: float,
        zeroize: Callable[[], int],
        kill: Callable[[], None],
        dry_done: Callable[[], None],
        clock: Callable[[], float],
        emit: Callable[[str], None],
    ):
        self.armed = armed
        self.grace = max(0.0, grace)
        self._zeroize = zeroize
        self._kill = kill
        self._dry_done = dry_done
        self._clock = clock
        self._emit = emit
        self.deadline: Optional[float] = None  # set while an armed panic is pending
        self.fired = False

    def request(self, reason: str) -> None:
        """A trigger fired. Wipe now (dry-run / no grace) or schedule it."""
        if self.fired:
            return
        if not self.armed:
            self._emit(f"DRY-RUN trigger ({reason}): zeroizing buffers; WOULD SIGKILL")
            n = self._zeroize()
            self._emit(f"DRY-RUN: zeroized {n} buffer(s); standing down (no kill)")
            self.fired = True
            self._dry_done()
            return
        if self.grace > 0:
            self.deadline = self._clock() + self.grace
            self._emit(
                f"ARMED trigger ({reason}): SIGKILL in {self.grace:g}s "
                f"unless a client reconnects or sends CANCEL"
            )
        else:
            self.execute(reason)

    def cancel(self, reason: str) -> bool:
        """Abort a pending armed panic. Returns True if one was pending."""
        if self.deadline is not None:
            self.deadline = None
            self._emit(f"panic CANCELLED ({reason})")
            return True
        return False

    def tick(self, now: float) -> None:
        """Drive the grace timer; call once per loop iteration."""
        if self.deadline is not None and now >= self.deadline:
            self.execute("grace window expired")

    def execute(self, reason: str) -> None:
        """The real self-destruct: zeroize then kill. Only reached when armed."""
        if self.fired:
            return
        self.fired = True
        self.deadline = None
        self._emit(f"PANIC ({reason}): zeroizing buffers and SIGKILL")
        n = self._zeroize()
        self._emit(f"PANIC: zeroized {n} buffer(s); sending SIGKILL")
        self._kill()


# --------------------------------------------------------------------------
# Signal handlers
# --------------------------------------------------------------------------
def _on_graceful(signum, frame):  # SIGUSR1
    # Async-signal-safe: just nudge the self-pipe; the loop does the work.
    try:
        os.write(_pipe_w, b"G")
    except OSError:
        pass


def _on_terminate(signum, frame):  # SIGTERM / SIGINT
    try:
        os.write(_pipe_w, b"T")
    except OSError:
        pass


def _on_hard_panic(signum, frame):  # SIGUSR2
    # The literal minimal panic path from the spec, kept deliberately tiny.
    # No logging module (not reentrant); raw os.write only.
    secmem.zeroize_all()
    if ARMED:
        os.kill(os.getpid(), signal.SIGKILL)
    else:
        os.write(2, b"[dazai] HARD PANIC (dry-run): zeroized; WOULD SIGKILL\n")
        os._exit(0)


# --------------------------------------------------------------------------
# Daemon: wires the socket + signals to a PanicController.
# --------------------------------------------------------------------------
class Daemon:
    def __init__(self, opts: argparse.Namespace,
                 killer: Optional[Callable[[], None]] = None):
        self.opts = opts
        self.buffers: List[secmem.SecureBuffer] = []
        self.server: Optional[socket.socket] = None
        self.client: Optional[socket.socket] = None
        self.last_ping = 0.0
        self._rbuf = b""
        self._stop = False
        self.controller = PanicController(
            armed=opts.arm,
            grace=opts.grace,
            zeroize=secmem.zeroize_all,
            kill=killer or self._kill,
            dry_done=self._dry_done,
            clock=time.monotonic,
            emit=log.warning,
        )

    # -- collaborators handed to the controller -------------------------
    def _kill(self) -> None:
        # Test seam: DAZAI_FAKE_KILL lets the armed self-destruct run
        # end-to-end in a subprocess without truly SIGKILLing -- the test
        # detects the sentinel exit code instead of a signal death.
        if os.environ.get("DAZAI_FAKE_KILL"):
            log.warning("FAKE-KILL (test mode): WOULD os.kill(SIGKILL) now")
            os._exit(42)
        os.kill(os.getpid(), signal.SIGKILL)

    def _dry_done(self) -> None:
        self._stop = True  # leave the loop -> clean cleanup -> exit 0

    # -- setup ----------------------------------------------------------
    def _make_buffers(self) -> None:
        # A small key buffer + a larger working buffer: "buffers", plural, all
        # mlock'd, all wiped together on panic. Append each as soon as it is
        # constructed so a failure on the second never orphans the first --
        # it stays reachable for _cleanup via self.buffers and LIVE_BUFFERS.
        self.buffers = []
        key = secmem.SecureBuffer(32, name="key")
        self.buffers.append(key)
        key.write(os.urandom(32))
        work = secmem.SecureBuffer(self.opts.size, name="work")
        self.buffers.append(work)
        work.write(
            b"SYNTHETIC SECRET -- if this is readable after a wipe, zeroize failed.\n"
        )
        locked = sum(1 for b in self.buffers if b.locked)
        log.warning("allocated %d working buffer(s), %d mlock'd",
                    len(self.buffers), locked)

    def _make_socket(self) -> None:
        path = self.opts.socket
        # AF_UNIX paths are bounded by sockaddr_un.sun_path: ~104 bytes on
        # macOS, 108 on Linux. Validate up front with a clear, portable message
        # rather than letting bind() raise an opaque "AF_UNIX path too long".
        if len(os.fsencode(path)) > 103:
            raise SystemExit(
                f"--socket path too long ({len(os.fsencode(path))} bytes); the "
                f"AF_UNIX limit is ~104 bytes (macOS). Use a shorter --socket "
                f"path or point XDG_RUNTIME_DIR at a short directory."
            )
        # Remove a stale socket from a previous run.
        try:
            if os.path.exists(path):
                os.unlink(path)
        except OSError as exc:
            log.error("could not remove stale socket %s: %s", path, exc)
        srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        old_umask = os.umask(0o177)  # ensure 0600 on the socket node
        try:
            srv.bind(path)
        except OSError as exc:
            srv.close()
            raise SystemExit(f"could not bind UNIX socket {path}: {exc}")
        finally:
            os.umask(old_umask)
        srv.listen(8)
        srv.setblocking(False)
        self.server = srv
        log.warning("listening on %s (mode 0600)", path)

    def _install_signals(self) -> None:
        global _pipe_r, _pipe_w
        _pipe_r, _pipe_w = os.pipe()
        os.set_blocking(_pipe_r, False)
        os.set_blocking(_pipe_w, False)
        signal.signal(signal.SIGUSR1, _on_graceful)
        signal.signal(signal.SIGUSR2, _on_hard_panic)
        signal.signal(signal.SIGTERM, _on_terminate)
        signal.signal(signal.SIGINT, _on_terminate)

    # -- main loop ------------------------------------------------------
    def run(self) -> int:
        # Install signal handlers (and the self-pipe) FIRST, before any secret
        # is written. A panic signal arriving mid-setup then finds a handler
        # that can zeroize, instead of hitting the default terminate-without-
        # wipe disposition. The setup steps are inside the try so the finally
        # always wipes, even if buffer/socket setup raises.
        self._install_signals()
        try:
            self._make_buffers()
            self._make_socket()
            self._banner()
            self._loop()
        finally:
            self._cleanup()
        return 0

    def _loop(self) -> None:
        assert self.server is not None
        while not self._stop:
            rlist = [self.server, _pipe_r]
            if self.client is not None:
                rlist.append(self.client)
            timeout = self._next_timeout()
            try:
                ready, _, _ = select.select(rlist, [], [], timeout)
            except InterruptedError:  # a signal arrived mid-select; loop again
                continue

            if _pipe_r in ready:
                self._drain_pipe()
            # Process existing-client I/O (including EOF) BEFORE accepting, so a
            # dropped heartbeat clears self.client and a fast reconnect in the
            # same iteration is accepted as a reconnect rather than refused.
            if self.client is not None and self.client in ready:
                self._on_client_readable()
            if self.server in ready:
                self._accept()

            now = time.monotonic()
            self.controller.tick(now)
            self._check_ping_timeout(now)

    def _next_timeout(self) -> Optional[float]:
        now = time.monotonic()
        candidates = []
        if self.controller.deadline is not None:
            candidates.append(self.controller.deadline - now)
        if self.client is not None and self.opts.ping_timeout > 0:
            candidates.append(self.last_ping + self.opts.ping_timeout - now)
        if not candidates:
            return None
        return max(0.0, min(candidates))

    # -- event handlers -------------------------------------------------
    def _drain_pipe(self) -> None:
        try:
            data = os.read(_pipe_r, 256)
        except BlockingIOError:
            return
        for code in data:
            if code == ord("G"):
                self.controller.request("SIGUSR1 graceful panic signal")
            elif code == ord("T"):
                log.warning("received SIGTERM/SIGINT: clean shutdown (zeroize, no kill)")
                self._stop = True

    def _accept(self) -> None:
        try:
            conn, _ = self.server.accept()
        except BlockingIOError:
            return
        except OSError as exc:
            # On fd exhaustion the pending connection stays in the backlog and
            # the listener stays readable, so back off briefly to avoid a 100%
            # CPU busy-loop. Transient errors just fall through to a retry.
            if exc.errno in (errno.EMFILE, errno.ENFILE):
                log.error("accept failed (%s); backing off", os.strerror(exc.errno))
                time.sleep(0.1)
            return
        conn.setblocking(False)
        if self.client is not None:
            # Already guarded by a live heartbeat. Refuse the extra connection
            # so a second client cannot silently displace the real one or
            # cancel a pending panic. Liveness stays bound to the first client.
            log.warning("refused extra client; heartbeat already held")
            try:
                conn.sendall(b"BUSY\n")
                conn.close()
            except OSError:
                pass
            return
        # No current client: a (re)connect. If an armed panic is pending from
        # the heartbeat we just lost, reconnecting within the grace window
        # cancels it.
        self.controller.cancel("client reconnected")
        self.client = conn
        self._rbuf = b""
        self.last_ping = time.monotonic()
        log.warning("heartbeat client connected")

    def _on_client_readable(self) -> None:
        try:
            chunk = self.client.recv(4096)
        except BlockingIOError:
            return
        except OSError:
            chunk = b""
        if not chunk:
            self._close_client()
            self.controller.request("heartbeat connection dropped")
            return
        self._rbuf += chunk
        # Bound the line buffer: a peer that never sends a newline must not be
        # able to grow daemon memory without limit.
        if b"\n" not in self._rbuf and len(self._rbuf) > _MAX_LINE:
            log.warning("client line exceeded %d bytes with no newline; dropping buffer",
                        _MAX_LINE)
            self._reply(b"ERR line-too-long")
            self._rbuf = b""
            return
        while b"\n" in self._rbuf:
            line, self._rbuf = self._rbuf.split(b"\n", 1)
            self._handle_line(line.strip())

    def _handle_line(self, line: bytes) -> None:
        if not line:
            return
        verb = line.split(b" ", 1)[0].upper()
        if verb == b"HELLO":
            self._reply(b"WELCOME")
        elif verb == b"PING":
            self.last_ping = time.monotonic()
            self._reply(b"PONG")
        elif verb == b"CANCEL":
            self._reply(b"CANCELLED" if self.controller.cancel("client CANCEL") else b"NOTHING-PENDING")
        elif verb == b"QUIT":
            self._reply(b"BYE")
            log.warning("client QUIT: intentional clean stand-down")
            self._stop = True
        else:
            self._reply(b"ERR unknown-verb")

    def _reply(self, msg: bytes) -> None:
        if self.client is None:
            return
        try:
            self.client.sendall(msg + b"\n")
        except OSError:
            pass

    def _check_ping_timeout(self, now: float) -> None:
        if self.client is None or self.opts.ping_timeout <= 0:
            return
        if now - self.last_ping > self.opts.ping_timeout:
            self._close_client()
            self.controller.request("ping deadline missed")

    def _close_client(self) -> None:
        if self.client is not None:
            try:
                self.client.close()
            except OSError:
                pass
            self.client = None
            self._rbuf = b""

    # -- teardown -------------------------------------------------------
    def _cleanup(self) -> None:
        # Always wipe on the way out, even for a clean shutdown.
        for buf in list(self.buffers):
            try:
                buf.free()
            except Exception:
                pass
        # Fallback: free any buffer that never made it into self.buffers (e.g.
        # a failure midway through _make_buffers), so nothing is left mlocked
        # or un-wiped.
        for buf in list(secmem.LIVE_BUFFERS):
            try:
                buf.free()
            except Exception:
                pass
        self._close_client()
        if self.server is not None:
            try:
                self.server.close()
            except OSError:
                pass
        try:
            if self.opts.socket and os.path.exists(self.opts.socket):
                os.unlink(self.opts.socket)
        except OSError:
            pass

    def _banner(self) -> None:
        mode = "ARMED -- real memset + SIGKILL on trigger" if self.opts.arm \
            else "DRY-RUN -- will zeroize and log WOULD-SIGKILL (pass --arm for real)"
        log.warning("=" * 64)
        log.warning("dazai dead-man's-switch  pid=%d  mode=%s", os.getpid(), mode)
        log.warning("socket : %s", self.opts.socket)
        log.warning("grace  : %gs (armed graceful panics)   ping-timeout: %gs",
                    self.opts.grace, self.opts.ping_timeout)
        log.warning("signals: SIGUSR1=graceful  SIGUSR2=HARD  SIGTERM/INT=clean-exit")
        log.warning("trigger by dropping the heartbeat connection, or:  kill -USR1 %d",
                    os.getpid())
        log.warning("=" * 64)


# --------------------------------------------------------------------------
def _default_socket_path() -> str:
    base = os.environ.get("XDG_RUNTIME_DIR") or "/tmp"
    return os.path.join(base, f"deadman-{os.getuid()}.sock")


def parse_args(argv: Optional[List[str]] = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="Session-bound, memory-zeroizing dead-man's-switch daemon "
                    "(reference implementation).",
    )
    p.add_argument("--socket", default=_default_socket_path(),
                   help="UNIX socket path for the heartbeat (default: %(default)s)")
    p.add_argument("--arm", action="store_true",
                   help="enable a REAL self-destruct (memset + SIGKILL). "
                        "Without this, runs in safe DRY-RUN mode.")
    p.add_argument("--grace", type=float, default=5.0, metavar="SECONDS",
                   help="armed graceful-panic grace window; a reconnect/CANCEL "
                        "within it aborts (default: %(default)ss)")
    p.add_argument("--ping-timeout", type=float, default=0.0, metavar="SECONDS",
                   help="if >0, panic when no PING arrives within this many "
                        "seconds (default: 0 = rely on connection liveness)")
    p.add_argument("--size", type=int, default=4096, metavar="BYTES",
                   help="synthetic working-buffer size (default: %(default)s)")
    p.add_argument("-v", "--verbose", action="store_true", help="debug logging")
    args = p.parse_args(argv)
    if args.size <= 0:
        p.error("--size must be a positive integer")
    if args.grace < 0:
        p.error("--grace must be >= 0")
    if args.ping_timeout < 0:
        p.error("--ping-timeout must be >= 0")
    return args


def main(argv: Optional[List[str]] = None) -> int:
    global ARMED
    opts = parse_args(argv)
    logging.basicConfig(
        level=logging.DEBUG if opts.verbose else logging.INFO,
        format="%(asctime)s [%(name)s] %(message)s",
        datefmt="%H:%M:%S",
        stream=sys.stderr,
    )
    ARMED = opts.arm
    return Daemon(opts).run()


if __name__ == "__main__":
    sys.exit(main())
