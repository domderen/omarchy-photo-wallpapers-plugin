use omarchy_photo_wallpapers::{platform, storage::*};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn private_directory_rejects_a_symlink_in_a_parent_before_creating_children() {
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("unrelated");
    fs::create_dir(&outside).unwrap();
    let link = root.path().join("parent");
    symlink(&outside, &link).unwrap();
    assert!(private_dir(&link.join("child")).is_err());
    assert!(!outside.join("child").exists());
}

#[test]
fn private_directory_rejects_a_writable_parent_before_creating_children() {
    let root = tempfile::tempdir().unwrap();
    let shared = root.path().join("writable");
    fs::create_dir(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(private_dir(&shared.join("private")).is_err());
    assert!(!shared.join("private").exists());
}

#[test]
fn private_directory_rejects_symlink_without_changing_target_permissions() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("unrelated");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    let link = root.path().join("private");
    symlink(&target, &link).unwrap();
    assert!(private_dir(&link).is_err());
    assert_eq!(
        fs::metadata(target).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn private_reads_reject_symlinks_and_non_regular_files() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("unrelated");
    fs::write(&target, b"synthetic-private-value").unwrap();
    let link = root.path().join("saved-session");
    symlink(&target, &link).unwrap();
    assert!(read_bounded(&link, 100).is_err());
    assert!(read_bounded(root.path(), 100).is_err());
    assert_eq!(
        read_bounded(&target, 100).unwrap(),
        b"synthetic-private-value"
    );
}

#[test]
fn database_rejects_symlink_without_initializing_the_target() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::under(root.path());
    private_dir(&paths.data).unwrap();
    let unrelated = root.path().join("unrelated.sqlite");
    rusqlite::Connection::open(&unrelated).unwrap();
    symlink(&unrelated, paths.database()).unwrap();
    assert!(Store::open(&paths).is_err());
    let count: i64 = rusqlite::Connection::open(&unrelated)
        .unwrap()
        .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn daemon_lock_rejects_symlink() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::under(root.path());
    private_dir(&paths.runtime).unwrap();
    let target = root.path().join("unrelated");
    fs::write(&target, b"unchanged").unwrap();
    symlink(&target, paths.runtime.join("engine.lock")).unwrap();
    assert!(platform::lock(&paths).is_err());
    assert_eq!(fs::read(target).unwrap(), b"unchanged");
}
