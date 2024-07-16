#[cfg(all(loom, test))]
mod test_model {
    use crate::{prepare, Guard, Shared};
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::Arc;

    struct A(Arc<AtomicBool>);
    impl Drop for A {
        fn drop(&mut self) {
            self.0.swap(true, Relaxed);
        }
    }

    #[test]
    fn ebr() {
        loom::model(|| {
            prepare();

            let flag = Arc::new(AtomicBool::new(false));
            let data = Shared::new(A(flag.clone()));
            let data_clone = data.clone();

            let thread = loom::thread::spawn(move || {
                let guard = Guard::new();
                let ptr = data_clone.get_guarded_ptr(&guard);
                drop(data_clone);
                assert!(!ptr.as_ref().unwrap().0.load(Relaxed));
            });

            let guard = Guard::new();
            let ptr = data.get_guarded_ptr(&guard);
            drop(data);

            assert!(!ptr.as_ref().unwrap().0.load(Relaxed));
            drop(guard);

            assert!(thread.join().is_ok());
        });
    }
}
