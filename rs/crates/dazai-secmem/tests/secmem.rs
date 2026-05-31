//! Ports the Phase 1 (Python) `test_secmem.py` suite to Rust, plus
//! Rust-specific lifecycle checks (Drop wipes, move-only, no escaping pointer).

use dazai_secmem::{secure_wipe, SecretBuffer};

#[test]
fn write_and_read_roundtrip() {
    let mut buf = SecretBuffer::new(64).unwrap();
    buf.write(b"super secret").unwrap();
    assert!(buf.as_slice().starts_with(b"super secret"));
    // trailing slack is zeroed
    assert_eq!(&buf.as_slice()[12..], &[0u8; 64 - 12][..]);
}

#[test]
fn wipe_clears_contents() {
    let mut buf = SecretBuffer::new(32).unwrap();
    buf.write(&[b'A'; 32]).unwrap();
    assert_eq!(buf.as_slice(), &[b'A'; 32][..]);
    buf.wipe();
    assert_eq!(buf.as_slice(), &[0u8; 32][..]);
}

#[test]
fn write_too_large_is_rejected() {
    let mut buf = SecretBuffer::new(8).unwrap();
    assert!(buf.write(b"123456789").is_err()); // 9 > 8
}

#[test]
fn zero_length_is_rejected() {
    assert!(SecretBuffer::new(0).is_err());
}

#[test]
fn double_wipe_is_safe() {
    let mut buf = SecretBuffer::new(48).unwrap();
    buf.write(&[0xFF; 48]).unwrap();
    buf.wipe();
    buf.wipe(); // must not panic / corrupt
    assert_eq!(buf.as_slice(), &[0u8; 48][..]);
}

#[test]
fn len_reflects_requested_length_not_page_size() {
    let buf = SecretBuffer::new(10).unwrap();
    assert_eq!(buf.len(), 10);
    assert_eq!(buf.as_slice().len(), 10);
    assert!(!buf.is_empty());
}

#[test]
fn mlock_succeeds_on_a_small_buffer() {
    // On the supported dev platforms (and CI with default rlimits) a 4 KiB
    // lock succeeds. If this ever fails it signals a real mlock regression.
    let buf = SecretBuffer::new(4096).unwrap();
    assert!(buf.is_locked(), "mlock should succeed for a single page");
}

#[test]
fn drop_runs_without_panic_in_a_loop() {
    // Exercises the wipe -> munlock -> munmap Drop path repeatedly; a
    // double-free or bad munmap length would abort the test process.
    for _ in 0..256 {
        let mut buf = SecretBuffer::new(1024).unwrap();
        buf.write(b"ephemeral").unwrap();
        // dropped here
    }
}

#[test]
fn standalone_secure_wipe_zeroes_a_slice() {
    let mut data = vec![0xAB_u8; 100];
    secure_wipe(&mut data);
    assert!(data.iter().all(|&b| b == 0));
}

#[test]
fn secure_wipe_on_empty_slice_is_noop() {
    let mut empty: [u8; 0] = [];
    secure_wipe(&mut empty); // must not panic
}

// Compile-time guarantees (documented, not asserted at runtime):
// - `SecretBuffer` is move-only: it derives neither Clone nor Copy, so a second
//   owner cannot be created. (A `let b2 = b1;` move invalidates `b1`.)
// - No public method returns a raw pointer; `as_slice`/`as_mut_slice` return
//   borrows tied to `&self`/`&mut self`, so no pointer outlives the buffer.
#[test]
fn move_only_value_semantics() {
    let mut a = SecretBuffer::new(16).unwrap();
    a.write(b"move me").unwrap();
    let b = a; // move (not copy); `a` is no longer usable after this line
    assert!(b.as_slice().starts_with(b"move me"));
}
