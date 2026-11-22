//! Keep this test in its own binary with no child launches. On Unix a
//! concurrent fork can inherit the locked file descriptor until exec,
//! briefly keeping the lock alive after the original handle is dropped.

use metro_connector::desktop::store::try_lock_instance;

#[test]
fn instance_lock_is_exclusive() {
    let root = tempfile::tempdir().unwrap();
    let first = try_lock_instance(root.path())
        .unwrap()
        .expect("the first instance should acquire the lock");
    assert!(try_lock_instance(root.path()).unwrap().is_none());

    drop(first);
    let _second = try_lock_instance(root.path())
        .unwrap()
        .expect("a new primary should acquire the lock after the first drops it");
    assert!(try_lock_instance(root.path()).unwrap().is_none());
}
