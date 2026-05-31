"""Page-locked, zeroizable memory buffers built on mmap + ctypes(mlock).

This is the memory-hygiene layer of the ``dazai`` dead-man's-switch reference
implementation. A :class:`SecureBuffer` wraps a page-aligned anonymous mmap
region, attempts to ``mlock(2)`` it so the kernel will not page it out to swap,
and exposes ``write`` / ``read`` / ``zeroize`` operations that act directly on
the underlying bytes through ctypes.

Portability notes
------------------
* ``mlock`` / ``munlock`` exist on both macOS and Linux.
* ``madvise(MADV_DONTDUMP)`` (exclude pages from core dumps) is Linux-only;
  on macOS it is skipped.
* ``RLIMIT_MEMLOCK`` caps how much memory an unprivileged process may lock. We
  try to raise the soft limit to the hard limit before locking.
* If locking fails (rlimit too low, EPERM, ...) we log loudly and continue in a
  *degraded* (swappable) mode rather than aborting. This is a reference impl.

Honest caveat
-------------
CPython copies ``bytes`` objects freely, so a secret may have transiently lived
in unlocked heap memory before it ever reached a SecureBuffer. This module
demonstrates the *mechanism* (page-locking + explicit zeroization); it is not a
guarantee of zero plaintext residue inside a managed runtime. Real key handling
wants a language where you control allocation end-to-end.
"""

from __future__ import annotations

import ctypes
import ctypes.util
import logging
import mmap
import os
import platform

log = logging.getLogger("dazai.secmem")

_SYSTEM = platform.system()  # 'Darwin', 'Linux', ...

# MADV_DONTDUMP is a Linux-only advice value; absent on macOS.
_MADV_DONTDUMP = 16 if _SYSTEM == "Linux" else None


def _load_libc() -> ctypes.CDLL:
    """Load the C library robustly across macOS and Linux.

    ``find_library`` can return ``None`` on recent macOS (libraries live in the
    dyld shared cache), so we fall back through known names and finally to
    ``CDLL(None)`` which resolves already-loaded symbols (RTLD_DEFAULT).
    """
    candidates = []
    found = ctypes.util.find_library("c")
    if found:
        candidates.append(found)
    if _SYSTEM == "Darwin":
        candidates += ["libSystem.B.dylib", "libSystem.dylib"]
    else:
        candidates += ["libc.so.6", "libc.so"]
    candidates.append(None)  # main program / RTLD_DEFAULT

    last_err: Exception | None = None
    for name in candidates:
        try:
            return ctypes.CDLL(name, use_errno=True)
        except OSError as exc:  # pragma: no cover - platform dependent
            last_err = exc
    raise OSError(f"could not load libc (tried {candidates}): {last_err}")


_libc = _load_libc()

_libc.mlock.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
_libc.mlock.restype = ctypes.c_int
_libc.munlock.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
_libc.munlock.restype = ctypes.c_int
try:
    _libc.madvise.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int]
    _libc.madvise.restype = ctypes.c_int
    _HAVE_MADVISE = True
except AttributeError:  # pragma: no cover
    _HAVE_MADVISE = False


def _try_raise_memlock_rlimit() -> None:
    """Best-effort: raise the soft RLIMIT_MEMLOCK to the hard limit.

    Guarded because ``RLIMIT_MEMLOCK`` is not defined in ``resource`` on every
    platform, and ``setrlimit`` may be refused.
    """
    try:
        import resource
    except ImportError:  # pragma: no cover - non-unix
        return
    if not hasattr(resource, "RLIMIT_MEMLOCK"):
        return
    try:
        soft, hard = resource.getrlimit(resource.RLIMIT_MEMLOCK)
        if soft != hard:
            resource.setrlimit(resource.RLIMIT_MEMLOCK, (hard, hard))
    except (ValueError, OSError) as exc:  # pragma: no cover - policy dependent
        log.debug("could not raise RLIMIT_MEMLOCK: %s", exc)


# Registry of live buffers so a panic path can zeroize every working buffer at
# once without threading them through every call site. In CPython, signal
# handlers run between bytecode ops in the main thread (never truly concurrent
# with main-thread list mutation), so iterating this list from a handler is
# safe here. Documented because it would NOT be safe under free-threading.
LIVE_BUFFERS: "list[SecureBuffer]" = []


def zeroize_all() -> int:
    """Zeroize every live SecureBuffer. Returns the count wiped.

    Swallows per-buffer errors: in a panic we want best-effort wiping of as
    many buffers as possible, not an early abort.
    """
    wiped = 0
    for buf in list(LIVE_BUFFERS):
        try:
            buf.zeroize()
            wiped += 1
        except Exception:  # pragma: no cover - panic path, never raise
            pass
    return wiped


class SecureBuffer:
    """A page-aligned, mlock'd, explicitly-zeroizable byte buffer.

    Usage::

        buf = SecureBuffer(64, name="key")
        buf.write(b"super secret")
        ... use buf ...
        buf.zeroize()      # overwrite contents with zeros
        buf.free()         # munlock + release the mapping

    ``locked`` reports whether ``mlock`` actually succeeded. The buffer is
    fully usable either way; ``locked is False`` just means the pages may be
    swappable (degraded mode).
    """

    def __init__(self, size: int, name: str = "buf"):
        if size <= 0:
            raise ValueError("size must be positive")
        self.size = size
        self.name = name
        self.locked = False
        self._closed = False
        self._used = 0

        # Anonymous mmap is page-aligned, which is what mlock wants. Passing
        # fileno -1 selects MAP_ANONYMOUS with sensible default flags/prot on
        # both macOS and Linux without us hard-coding platform constants.
        self._mm = mmap.mmap(-1, size)
        # A ctypes view that shares the mmap's storage; gives us a stable
        # address to hand to mlock/memset/memmove. Held so the export stays
        # alive; dropped before close() in free().
        self._view = (ctypes.c_char * size).from_buffer(self._mm)
        self.address = ctypes.addressof(self._view)

        _try_raise_memlock_rlimit()
        self.locked = self._mlock()
        self._dontdump()

        LIVE_BUFFERS.append(self)

    # -- locking ---------------------------------------------------------

    def _mlock(self) -> bool:
        ctypes.set_errno(0)
        rc = _libc.mlock(self.address, self.size)
        if rc != 0:
            err = ctypes.get_errno()
            log.warning(
                "mlock failed for buffer %r (%d bytes): %s -- continuing in "
                "DEGRADED (swappable) mode",
                self.name, self.size, os.strerror(err) if err else "unknown",
            )
            return False
        log.debug("mlock'd buffer %r (%d bytes) at 0x%x", self.name, self.size, self.address)
        return True

    def _dontdump(self) -> None:
        """Linux: ask the kernel to exclude these pages from core dumps."""
        if _MADV_DONTDUMP is None or not _HAVE_MADVISE:
            return
        ctypes.set_errno(0)
        rc = _libc.madvise(self.address, self.size, _MADV_DONTDUMP)
        if rc != 0:  # pragma: no cover - advisory, non-fatal
            err = ctypes.get_errno()
            log.debug("madvise(DONTDUMP) failed for %r: %s", self.name,
                      os.strerror(err) if err else "unknown")

    # -- contents --------------------------------------------------------

    def write(self, data: bytes) -> None:
        """Copy ``data`` into the buffer (and zero any trailing slack)."""
        if self._closed:
            raise ValueError("buffer is closed")
        if len(data) > self.size:
            raise ValueError(f"data ({len(data)}B) exceeds buffer ({self.size}B)")
        ctypes.memmove(self.address, data, len(data))
        if len(data) < self.size:
            ctypes.memset(self.address + len(data), 0, self.size - len(data))
        self._used = len(data)

    def read(self, n: int | None = None) -> bytes:
        """Return the first ``n`` bytes (default: whole buffer) as ``bytes``.

        Note this copies into a Python bytes object -- handy for tests/demo,
        but in real use you would operate on the locked memory in place.
        """
        if self._closed:
            raise ValueError("buffer is closed")
        # Clamp on BOTH ends: a negative count would make ctypes.string_at
        # switch to NUL-terminated (strlen) mode and read past the mapping.
        count = self.size if n is None else max(0, min(n, self.size))
        return ctypes.string_at(self.address, count)

    def zeroize(self) -> None:
        """Overwrite the entire buffer with zero bytes via ``ctypes.memset``.

        This is the core wipe primitive the panic path relies on. Safe to call
        repeatedly; a no-op once the mapping is released.
        """
        if self._closed:
            return
        ctypes.memset(self.address, 0, self.size)
        self._used = 0

    # -- lifecycle -------------------------------------------------------

    def free(self) -> None:
        """Zeroize, munlock, and release the underlying mapping. Idempotent.

        Ordering matters for signal-safety. A SIGUSR2 hard-panic can interrupt
        this method between bytecode ops and run zeroize_all() -> zeroize() on
        the main thread. We therefore do the final wipe while the mapping is
        still valid, then flip ``_closed`` and de-register from LIVE_BUFFERS
        *before* unmapping. Once those run, an interrupting zeroize() sees a
        closed/absent buffer and never memsets the freed address (which would
        SIGSEGV the very path that is supposed to be the most robust).
        """
        if self._closed:
            return
        self.zeroize()  # final wipe -- mapping still mapped, guard not yet set
        if self.locked:
            _libc.munlock(self.address, self.size)
            self.locked = False
        # Neutralize BEFORE unmapping: after these two statements, zeroize()
        # and zeroize_all() are guaranteed no-ops for this buffer.
        self._closed = True
        try:
            LIVE_BUFFERS.remove(self)
        except ValueError:
            pass
        # The ctypes view exports a pointer into the mmap; it must be released
        # before mmap.close() or CPython raises BufferError.
        self._view = None
        try:
            self._mm.close()
        except BufferError:  # pragma: no cover - outstanding export
            pass

    def __len__(self) -> int:
        return self.size

    def __repr__(self) -> str:
        state = "closed" if self._closed else ("locked" if self.locked else "degraded")
        return f"<SecureBuffer {self.name!r} {self.size}B {state}>"
