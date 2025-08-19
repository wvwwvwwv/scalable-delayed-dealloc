use sdd::{AtomicShared, Guard, Owned, Shared, Tag};
use std::sync::atomic::AtomicIsize;
use std::sync::atomic::Ordering::{Acquire, Relaxed};
use std::thread::{self, yield_now};

struct R(&'static AtomicIsize);
impl Drop for R {
    fn drop(&mut self) {
        self.0.fetch_add(1, Relaxed);
    }
}

#[test]
fn single_threaded() {
    static DROP_CNT: AtomicIsize = AtomicIsize::new(0);

    let r = Shared::new(R(&DROP_CNT));

    let guard = Guard::new();

    // `p` can outlive `r` as long as `guard` survives.
    let p = r.get_guarded_ptr(&guard);

    assert_eq!(DROP_CNT.load(Relaxed), 0);
    drop(r);

    assert_eq!(DROP_CNT.load(Relaxed), 0);

    // It is possible to read the memory owned by `r`.
    assert!(p.as_ref().is_some());

    // Dropping guard immediately invalidates `p`.
    drop(guard);

    while DROP_CNT.load(Relaxed) != 1 {
        Guard::new().accelerate();
        yield_now();
    }
    assert_eq!(DROP_CNT.load(Relaxed), 1);
}

#[test]
fn multi_threaded() {
    static DROP_CNT: AtomicIsize = AtomicIsize::new(0);

    let r1 = Owned::new(R(&DROP_CNT));
    let r2 = AtomicShared::new(R(&DROP_CNT));

    thread::scope(|s| {
        s.spawn(|| {
            let guard = Guard::new();
            let p = r1.get_guarded_ptr(&guard);
            drop(r1);

            // `p` can outlive `r1`.
            assert!(p.as_ref().unwrap().0.load(Relaxed) <= 1);
        });
        s.spawn(|| {
            let guard = Guard::new();

            // `p` can be constructed through `AtomicShared` or `AtomicOwned`.
            let p = r2.load(Acquire, &guard);
            assert!(p.as_ref().unwrap().0.load(Relaxed) <= 1);

            let r3 = r2.get_shared(Acquire, &guard).unwrap();

            // `r3` can outlive `guard`.
            assert!(r3.0.load(Relaxed) <= 1);

            // `AtomicOwned` and `AtomicShared` provide atomic compare-and-swap methods.
            let r4 = r2
                .compare_exchange(p, (None, Tag::None), Acquire, Relaxed, &guard)
                .ok()
                .unwrap()
                .0
                .unwrap();
            assert!(r4.0.load(Relaxed) <= 1);
        });
    });

    while DROP_CNT.load(Relaxed) != 2 {
        Guard::new().accelerate();
        yield_now();
    }
    assert_eq!(DROP_CNT.load(Relaxed), 2);
}
