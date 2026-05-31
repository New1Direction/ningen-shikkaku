//! Unit tests for CallCounter (the exit-condition logic), in isolation.

use motokano::counter::{CallCounter, CounterMode};
use std::sync::Arc;

#[test]
fn calls_1() {
    let c = CallCounter::new(CounterMode::Calls(1));
    assert!(c.decrement()); // fires on the 1st call
    assert!(!c.decrement()); // never again
    assert!(!c.decrement());
}

#[test]
fn calls_3() {
    let c = CallCounter::new(CounterMode::Calls(3));
    assert!(!c.decrement()); // call 1
    assert!(!c.decrement()); // call 2
    assert!(c.decrement()); // call 3 -> fire
    assert!(!c.decrement()); // after
}

#[test]
fn session() {
    let c = CallCounter::new(CounterMode::Session);
    assert!(!c.decrement()); // calls never fire in session mode
    assert!(!c.decrement());
    assert!(c.on_disconnect()); // disconnect fires
    assert!(!c.on_disconnect()); // only once
}

#[test]
fn either_calls() {
    let c = CallCounter::new(CounterMode::Either(2));
    assert!(!c.decrement()); // call 1
    assert!(c.decrement()); // call 2 -> fire via calls
    assert!(!c.on_disconnect()); // disconnect after a calls-fire returns false
}

#[test]
fn either_disconnect() {
    let c = CallCounter::new(CounterMode::Either(3));
    assert!(!c.decrement()); // call 1 (before N)
    assert!(c.on_disconnect()); // disconnect fires first
    assert!(!c.decrement()); // any later call returns false
    assert!(!c.decrement());
}

#[test]
fn concurrent_decrement() {
    let c = Arc::new(CallCounter::new(CounterMode::Calls(1)));
    let trues = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..32 {
        let c = Arc::clone(&c);
        let trues = Arc::clone(&trues);
        handles.push(std::thread::spawn(move || {
            if c.decrement() {
                trues.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(
        trues.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "exactly one concurrent decrement must return true"
    );
}
