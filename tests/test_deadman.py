"""Tests for the PanicController logic and an end-to-end daemon run."""

import os
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import deadman  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DAEMON = os.path.join(ROOT, "deadman.py")


class FakeClock:
    def __init__(self, t=1000.0):
        self.t = t

    def __call__(self):
        return self.t

    def advance(self, dt):
        self.t += dt


def make_controller(armed, grace, clock):
    """Build a PanicController wired to recording fakes instead of real
    zeroize/kill, so we can assert behavior without dying."""
    calls = {"zeroize": 0, "kill": 0, "dry_done": 0, "log": []}

    def zeroize():
        calls["zeroize"] += 1
        return 3

    def kill():
        calls["kill"] += 1

    def dry_done():
        calls["dry_done"] += 1

    ctrl = deadman.PanicController(
        armed=armed, grace=grace,
        zeroize=zeroize, kill=kill, dry_done=dry_done,
        clock=clock, emit=lambda m: calls["log"].append(m),
    )
    return ctrl, calls


class PanicControllerTest(unittest.TestCase):
    def test_dry_run_wipes_but_never_kills(self):
        ctrl, calls = make_controller(armed=False, grace=5, clock=FakeClock())
        ctrl.request("connection dropped")
        self.assertEqual(calls["zeroize"], 1)
        self.assertEqual(calls["kill"], 0)        # crucial: no kill in dry-run
        self.assertEqual(calls["dry_done"], 1)
        self.assertTrue(ctrl.fired)

    def test_armed_no_grace_kills_immediately(self):
        ctrl, calls = make_controller(armed=True, grace=0, clock=FakeClock())
        ctrl.request("panic signal")
        self.assertEqual(calls["zeroize"], 1)
        self.assertEqual(calls["kill"], 1)

    def test_armed_grace_kills_only_after_deadline(self):
        clock = FakeClock()
        ctrl, calls = make_controller(armed=True, grace=5, clock=clock)
        ctrl.request("connection dropped")
        self.assertIsNotNone(ctrl.deadline)
        ctrl.tick(clock())                 # before deadline
        self.assertEqual(calls["kill"], 0)
        clock.advance(5.001)
        ctrl.tick(clock())                 # past deadline
        self.assertEqual(calls["zeroize"], 1)
        self.assertEqual(calls["kill"], 1)

    def test_reconnect_cancels_pending_panic(self):
        clock = FakeClock()
        ctrl, calls = make_controller(armed=True, grace=5, clock=clock)
        ctrl.request("connection dropped")
        self.assertTrue(ctrl.cancel("client reconnected"))
        clock.advance(10)
        ctrl.tick(clock())
        self.assertEqual(calls["kill"], 0)  # cancelled -> never fired
        self.assertFalse(ctrl.fired)

    def test_cancel_with_nothing_pending_returns_false(self):
        ctrl, _ = make_controller(armed=True, grace=5, clock=FakeClock())
        self.assertFalse(ctrl.cancel("noop"))

    def test_fired_guard_prevents_double_kill(self):
        ctrl, calls = make_controller(armed=True, grace=0, clock=FakeClock())
        ctrl.request("first")
        ctrl.request("second")
        ctrl.execute("third")
        self.assertEqual(calls["kill"], 1)
        self.assertEqual(calls["zeroize"], 1)  # guard protects zeroize too

    def test_tick_with_no_deadline_is_noop(self):
        ctrl, calls = make_controller(armed=True, grace=5, clock=FakeClock())
        ctrl.tick(10 ** 9)  # far future, but nothing scheduled
        self.assertEqual(calls["kill"], 0)
        self.assertEqual(calls["zeroize"], 0)


def _wait_for_socket(path, timeout=5.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if os.path.exists(path):
            return True
        time.sleep(0.05)
    return False


def _connect(path, timeout=5.0):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.connect(path)
            return s
        except OSError as exc:  # socket file exists but not accepting yet
            last = exc
            time.sleep(0.05)
    raise last


class DaemonEndToEndTest(unittest.TestCase):
    """Drive the real daemon (in DRY-RUN, so it exits 0 instead of SIGKILL)."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="dazai-test-")
        self.sock = os.path.join(self.tmp, "hb.sock")

    def tearDown(self):
        try:
            if os.path.exists(self.sock):
                os.unlink(self.sock)
            os.rmdir(self.tmp)
        except OSError:
            pass

    def _spawn(self, *extra, fake_kill=False):
        env = dict(os.environ)
        if fake_kill:
            env["DAZAI_FAKE_KILL"] = "1"  # armed kill -> exit 42 instead of dying
        return subprocess.Popen(
            [sys.executable, DAEMON, "--socket", self.sock, "--size", "256", *extra],
            stderr=subprocess.PIPE, stdout=subprocess.DEVNULL, env=env,
        )

    def test_protocol_then_connection_drop_triggers_dryrun_exit(self):
        proc = self._spawn()
        try:
            self.assertTrue(_wait_for_socket(self.sock), "daemon never bound socket")
            c = _connect(self.sock)
            c.sendall(b"HELLO 999\n")
            self.assertIn(b"WELCOME", c.recv(64))
            c.sendall(b"PING\n")
            self.assertIn(b"PONG", c.recv(64))
            c.close()  # drop the heartbeat -> dry-run wipe -> exit 0
            rc = proc.wait(timeout=10)
            self.assertEqual(rc, 0)
            err = proc.stderr.read().decode(errors="replace")
            self.assertIn("WOULD SIGKILL", err)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)
            proc.stderr.close()

    def test_sigusr1_triggers_dryrun_exit(self):
        proc = self._spawn()
        c = None
        try:
            self.assertTrue(_wait_for_socket(self.sock), "daemon never bound socket")
            # A completed handshake proves the loop is running (and, since
            # signals are installed before the socket binds, that the SIGUSR1
            # handler is live) -- no fragile fixed sleep needed.
            c = _connect(self.sock)
            c.sendall(b"HELLO 1\n")
            self.assertIn(b"WELCOME", c.recv(64))
            proc.send_signal(signal.SIGUSR1)
            rc = proc.wait(timeout=10)
            self.assertEqual(rc, 0)
            err = proc.stderr.read().decode(errors="replace")
            self.assertIn("WOULD SIGKILL", err)
        finally:
            if c is not None:
                c.close()
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)
            proc.stderr.close()

    def test_sigusr2_hard_panic_dryrun(self):
        proc = self._spawn()
        c = None
        try:
            self.assertTrue(_wait_for_socket(self.sock), "daemon never bound socket")
            c = _connect(self.sock)
            c.sendall(b"HELLO 1\n")
            self.assertIn(b"WELCOME", c.recv(64))
            proc.send_signal(signal.SIGUSR2)
            rc = proc.wait(timeout=10)
            self.assertEqual(rc, 0)
            err = proc.stderr.read().decode(errors="replace")
            self.assertIn("HARD PANIC (dry-run)", err)
        finally:
            if c is not None:
                c.close()
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)
            proc.stderr.close()

    def test_second_client_is_refused_and_first_survives(self):
        proc = self._spawn()
        c1 = c2 = None
        try:
            self.assertTrue(_wait_for_socket(self.sock), "daemon never bound socket")
            c1 = _connect(self.sock)
            c1.sendall(b"HELLO 1\n")
            self.assertIn(b"WELCOME", c1.recv(64))
            c2 = _connect(self.sock)
            c2.sendall(b"HELLO 2\n")
            self.assertIn(b"BUSY", c2.recv(64))   # extra client refused
            # The first heartbeat is unaffected and the daemon did not panic.
            c1.sendall(b"PING\n")
            self.assertIn(b"PONG", c1.recv(64))
            self.assertIsNone(proc.poll())
        finally:
            for s in (c1, c2):
                if s is not None:
                    s.close()
            if proc.poll() is None:
                proc.kill()
            proc.wait(timeout=5)
            proc.stderr.close()

    def test_partial_and_combined_lines(self):
        proc = self._spawn()
        c = None
        try:
            self.assertTrue(_wait_for_socket(self.sock), "daemon never bound socket")
            c = _connect(self.sock)
            c.sendall(b"HEL")        # verb split across two writes
            c.sendall(b"LO 1\n")
            self.assertIn(b"WELCOME", c.recv(64))
            c.sendall(b"PING\nPING\n")  # two verbs in one chunk
            data = b""
            deadline = time.monotonic() + 5
            while data.count(b"PONG") < 2 and time.monotonic() < deadline:
                data += c.recv(64)
            self.assertGreaterEqual(data.count(b"PONG"), 2)
        finally:
            if c is not None:
                c.close()
            if proc.poll() is None:
                proc.kill()
            proc.wait(timeout=5)
            proc.stderr.close()

    def test_ping_timeout_triggers_dryrun(self):
        proc = self._spawn("--ping-timeout", "1")
        c = None
        try:
            self.assertTrue(_wait_for_socket(self.sock), "daemon never bound socket")
            c = _connect(self.sock)
            c.sendall(b"HELLO 1\n")
            self.assertIn(b"WELCOME", c.recv(64))
            # Stay silent: no PING within 1s -> dry-run wipe -> exit 0.
            rc = proc.wait(timeout=10)
            self.assertEqual(rc, 0)
            err = proc.stderr.read().decode(errors="replace")
            self.assertIn("ping deadline missed", err)
        finally:
            if c is not None:
                c.close()
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)
            proc.stderr.close()

    def test_armed_grace_drop_reaches_kill(self):
        # Armed + fake killer: dropping the heartbeat must, after the grace
        # window, run the full execute() path through to the (faked) SIGKILL.
        proc = self._spawn("--arm", "--grace", "1", fake_kill=True)
        try:
            self.assertTrue(_wait_for_socket(self.sock), "daemon never bound socket")
            c = _connect(self.sock)
            c.sendall(b"HELLO 1\n")
            c.recv(64)
            c.close()  # drop -> 1s grace -> execute() -> fake kill (exit 42)
            rc = proc.wait(timeout=10)
            self.assertEqual(rc, 42)
            err = proc.stderr.read().decode(errors="replace")
            self.assertIn("sending SIGKILL", err)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)
            proc.stderr.close()

    def test_armed_reconnect_within_grace_cancels(self):
        proc = self._spawn("--arm", "--grace", "3", fake_kill=True)
        c1 = c2 = None
        try:
            self.assertTrue(_wait_for_socket(self.sock), "daemon never bound socket")
            c1 = _connect(self.sock)
            c1.sendall(b"HELLO 1\n")
            c1.recv(64)
            c1.close()              # start the 3s grace countdown
            time.sleep(0.6)
            c2 = _connect(self.sock)  # reconnect well within grace -> cancel
            c2.sendall(b"HELLO 2\n")
            c2.recv(64)
            time.sleep(3.2)         # outlive the original deadline
            self.assertIsNone(proc.poll(), "daemon should have survived the cancelled panic")
            proc.send_signal(signal.SIGTERM)
            rc = proc.wait(timeout=10)
            self.assertEqual(rc, 0)
            err = proc.stderr.read().decode(errors="replace")
            self.assertIn("CANCELLED", err)
        finally:
            for s in (c1, c2):
                if s is not None:
                    s.close()
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=5)
            proc.stderr.close()


if __name__ == "__main__":
    unittest.main()
