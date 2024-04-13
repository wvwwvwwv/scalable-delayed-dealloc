# Scalable Concurrent Containers

[![Cargo](https://img.shields.io/crates/v/smm)](https://crates.io/crates/smm)
![Crates.io](https://img.shields.io/crates/l/smm)
![GitHub Workflow Status](https://img.shields.io/github/actions/workflow/status/wvwwvwwv/scalable-memory-manager/smm.yml?branch=main)

Epoch-based reclamation and various types of auxiliary data structures to make use of it safely. Its epoch-based reclamation algorithm is similar to that implemented in [crossbeam_epoch](https://docs.rs/crossbeam-epoch/), however users may find it easier to use as the lifetime of an instance is safely managed. For instance, `ebr::AtomicOwned` and `ebr::Owned` automatically retire the contained instance and `ebr::AtomicShared` and `ebr::Shared` hold a reference-counted instance which is retired when the last strong reference is dropped.

## Memory Overhead

Retired instances are stored in intrusive queues in thread-local storage, and therefore additional 16-byte space for `Option<NonNull<dyn Collectible>>` is allocated per instance.

## Examples

The `ebr` module can be used without an `unsafe` block.

```rust
use smm::{suspend, AtomicOwned, AtomicShared, Guard, Ptr, Shared, Tag};

use std::sync::atomic::Ordering::Relaxed;

// `atomic_shared` holds a strong reference to `17`.
let atomic_shared: AtomicShared<usize> = AtomicShared::new(17);

// `atomic_owned` owns `19`.
let atomic_owned: AtomicOwned<usize> = AtomicOwned::new(19);

// `guard` prevents the garbage collector from dropping reachable instances.
let guard = Guard::new();

// `ptr` cannot outlive `guard`.
let mut ptr: Ptr<usize> = atomic_shared.load(Relaxed, &guard);
assert_eq!(*ptr.as_ref().unwrap(), 17);

// `atomic_shared` can be tagged.
atomic_shared.update_tag_if(Tag::First, |p| p.tag() == Tag::None, Relaxed, Relaxed);

// `ptr` is not tagged, so CAS fails.
assert!(atomic_shared.compare_exchange(
    ptr,
    (Some(Shared::new(18)), Tag::First),
    Relaxed,
    Relaxed,
    &guard).is_err());

// `ptr` can be tagged.
ptr.set_tag(Tag::First);

// The return value of CAS is a handle to the instance that `atomic_shared` previously owned.
let prev: Shared<usize> = atomic_shared.compare_exchange(
    ptr,
    (Some(Shared::new(18)), Tag::Second),
    Relaxed,
    Relaxed,
    &guard).unwrap().0.unwrap();
assert_eq!(*prev, 17);

// `17` will be garbage-collected later.
drop(prev);

// `ebr::AtomicShared` can be converted into `ebr::Shared`.
let shared: Shared<usize> = atomic_shared.into_shared(Relaxed).unwrap();
assert_eq!(*shared, 18);

// `18` and `19` will be garbage-collected later.
drop(shared);
drop(atomic_owned);

// `17` is still valid as `guard` keeps the garbage collector from dropping it.
assert_eq!(*ptr.as_ref().unwrap(), 17);

// Execution of a closure can be deferred until all the current readers are gone.
guard.defer_execute(|| println!("deferred"));
drop(guard);

// If the thread is expected to lie dormant for a while, call `suspend()` to allow other threads
// to reclaim its own retired instances.
suspend();
```

## Performance

- The average time taken to enter and exit a protected region: 2.1 nanoseconds on Apple M1.

## [Changelog](https://github.com/wvwwvwwv/scalable-concurrent-containers/blob/main/CHANGELOG.md)
