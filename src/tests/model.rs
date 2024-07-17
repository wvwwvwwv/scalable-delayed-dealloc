#[cfg(all(loom_test, test))]
mod test_model {
    use crate::collector::{Collector, CollectorRoot};
    use crate::{suspend, Guard, Owned};
    use loom::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::Arc;

    struct A(Arc<AtomicBool>);
    impl Drop for A {
        fn drop(&mut self) {
            assert!(!self.0.swap(true, Relaxed));
        }
    }

    #[test]
    fn ebr() {
        loom::model(|| {
            let root = Arc::new(CollectorRoot::default());
            unsafe {
                Collector::set_root(&root);
            }

            let flag = Arc::new(AtomicBool::new(false));
            let data = Owned::new(A(flag.clone()));

            let guard = Guard::new();
            let ptr = data.get_guarded_ptr(&guard);

            let thread = loom::thread::spawn(move || {
                unsafe {
                    Collector::set_root(&root);
                }
                drop(root);

                let guard = Guard::new();
                let ptr = data.get_guarded_ptr(&guard);
                drop(data);

                assert!(!ptr.as_ref().unwrap().0.load(Relaxed));
                drop(guard);

                assert!(suspend());
            });

            assert!(!ptr.as_ref().unwrap().0.load(Relaxed));
            assert!(!flag.load(Relaxed));
            guard.accelerate();
            drop(guard);

            assert!(thread.join().is_ok());
        });
    }
}
