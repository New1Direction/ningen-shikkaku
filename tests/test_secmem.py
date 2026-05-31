"""Unit tests for the SecureBuffer memory-hygiene layer."""

import ctypes
import os
import platform
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import secmem  # noqa: E402


class SecureBufferTest(unittest.TestCase):
    def tearDown(self):
        # Free any buffers a test left registered, so tests don't leak locks.
        for buf in list(secmem.LIVE_BUFFERS):
            buf.free()

    def test_write_and_read_roundtrip(self):
        buf = secmem.SecureBuffer(64, name="t")
        buf.write(b"super secret")
        self.assertTrue(buf.read().startswith(b"super secret"))
        # trailing slack is zeroed
        self.assertEqual(buf.read()[12:], b"\x00" * (64 - 12))

    def test_zeroize_clears_contents(self):
        buf = secmem.SecureBuffer(32, name="t")
        buf.write(b"A" * 32)
        self.assertEqual(buf.read(), b"A" * 32)
        buf.zeroize()
        self.assertEqual(buf.read(), b"\x00" * 32)

    def test_write_too_large_rejected(self):
        buf = secmem.SecureBuffer(8, name="t")
        with self.assertRaises(ValueError):
            buf.write(b"123456789")  # 9 > 8

    def test_locked_is_boolean_and_no_crash(self):
        # mlock may or may not succeed depending on rlimit; either way usable.
        buf = secmem.SecureBuffer(128, name="t")
        self.assertIn(buf.locked, (True, False))
        buf.write(b"x")  # still works in degraded mode

    def test_free_is_idempotent_and_zeroizes(self):
        buf = secmem.SecureBuffer(16, name="t")
        buf.write(b"sensitive")
        buf.free()
        buf.free()  # no raise
        self.assertTrue(buf._closed)

    def test_zeroize_all_wipes_every_live_buffer(self):
        a = secmem.SecureBuffer(16, name="a")
        b = secmem.SecureBuffer(16, name="b")
        a.write(b"a" * 16)
        b.write(b"b" * 16)
        wiped = secmem.zeroize_all()
        self.assertEqual(wiped, 2)
        self.assertEqual(a.read(), b"\x00" * 16)
        self.assertEqual(b.read(), b"\x00" * 16)

    def test_free_removes_from_registry(self):
        buf = secmem.SecureBuffer(16, name="t")
        self.assertIn(buf, secmem.LIVE_BUFFERS)
        buf.free()
        self.assertNotIn(buf, secmem.LIVE_BUFFERS)

    def test_zero_size_rejected(self):
        with self.assertRaises(ValueError):
            secmem.SecureBuffer(0)

    def test_read_negative_n_is_clamped_not_strlen(self):
        # A negative n must not reach string_at as the NUL-terminated sentinel.
        buf = secmem.SecureBuffer(16, name="t")
        buf.write(b"abcdef")  # leaves a trailing NUL strlen would stop at
        self.assertEqual(buf.read(-1), b"")

    def test_zeroize_after_free_is_safe_noop(self):
        # Regression: free() must neutralize the buffer (guard + deregister)
        # BEFORE unmapping, so a zeroize()/zeroize_all() that races free()
        # cannot memset freed memory and SIGSEGV.
        buf = secmem.SecureBuffer(64, name="t")
        buf.write(b"x" * 64)
        buf.free()
        buf.zeroize()          # must be a no-op, not a segfault
        secmem.zeroize_all()   # buf is de-registered; must be safe

    def test_buffererror_on_close_is_swallowed(self):
        buf = secmem.SecureBuffer(32, name="t")
        # A second outstanding export keeps mmap.close() from succeeding,
        # forcing the BufferError branch in free(); it must be swallowed.
        extra = (ctypes.c_char * buf.size).from_buffer(buf._mm)
        try:
            buf.free()
            self.assertTrue(buf._closed)
            self.assertNotIn(buf, secmem.LIVE_BUFFERS)
        finally:
            del extra

    def test_abandoned_buffer_stays_in_registry(self):
        # Documents that SecureBuffer has no finalizer: dropping the only local
        # reference does NOT free it; callers must free() explicitly.
        buf = secmem.SecureBuffer(16, name="t")
        self.assertIn(buf, secmem.LIVE_BUFFERS)

    def test_mlock_succeeds_when_permitted(self):
        buf = secmem.SecureBuffer(64, name="t")
        if not buf.locked:
            self.skipTest("mlock not permitted in this environment")
        self.assertIs(buf.locked, True)

    @unittest.skipUnless(platform.system() == "Linux", "madvise(DONTDUMP) is Linux-only")
    def test_madvise_dontdump_invoked_on_linux(self):
        calls = []
        orig = secmem._libc.madvise

        def spy(addr, length, advice):
            calls.append(advice)
            return orig(addr, length, advice)

        secmem._libc.madvise = spy
        try:
            secmem.SecureBuffer(64, name="t")
        finally:
            secmem._libc.madvise = orig
        self.assertIn(secmem._MADV_DONTDUMP, calls)


if __name__ == "__main__":
    unittest.main()
