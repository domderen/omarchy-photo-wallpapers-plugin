use omarchy_photo_wallpapers::{model::*, storage::*};
use serde_json::json;

fn state() -> (tempfile::TempDir, Store) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(temp.path())).unwrap();
    (temp, store)
}

#[test]
fn fresh_settings_are_independent_and_revision_safe() {
    let (_temp, store) = state();
    let initial = store.settings().unwrap();
    assert_eq!(initial.interval_seconds, 3600);
    assert!(initial.sources.is_empty());
    assert!(!store.paths.config.join("credentials.json").exists());
    let updated = store
        .update_settings(
            0,
            json!({"interval_seconds":900,"sources":[{"id":"all","kind":"library"}]}),
        )
        .unwrap();
    assert_eq!(updated.revision, 1);
    assert!(
        store
            .update_settings(0, json!({"interval_seconds":60}))
            .is_err()
    );
    assert_eq!(store.settings().unwrap().interval_seconds, 900);
    for invalid in [
        json!({"interval_seconds":59}),
        json!({"interval_seconds":true}),
        json!({"paused":"false"}),
    ] {
        assert!(store.update_settings(1, invalid).is_err());
    }
    assert_eq!(store.settings().unwrap().revision, 1);
}

#[test]
fn commands_are_durable_and_deduplicated() {
    let (temp, store) = state();
    let one = store.enqueue("next").unwrap();
    assert_eq!(one, store.enqueue("next").unwrap());
    let command = store.claim_command().unwrap().unwrap();
    assert_eq!(command.id, one);
    assert_eq!(store.enqueue("next").unwrap(), one);
    drop(store);
    let store = Store::open(&Paths::under(temp.path())).unwrap();
    store.recover_commands().unwrap();
    assert_eq!(store.claim_command().unwrap().unwrap().id, one);
    store.finish_command(&one, json!({"ok":true})).unwrap();
    assert!(store.claim_command().unwrap().is_none());
    assert_ne!(store.enqueue("next").unwrap(), one);
}

fn asset(id: &str) -> Asset {
    Asset {
        id: id.into(),
        cloud_guid: id.into(),
        revision: "r1".into(),
        width: 4000,
        height: 3000,
        ..Default::default()
    }
}

#[test]
fn base_publication_is_atomic_and_excludes_hidden_deleted() {
    let (_temp, store) = state();
    let mut hidden = asset("hidden");
    hidden.hidden = true;
    let mut deleted = asset("deleted");
    deleted.deleted = true;
    store
        .activate_catalog(
            &[asset("visible"), hidden, deleted],
            &[Source::library()],
            &[],
            "g1",
        )
        .unwrap();
    assert_eq!(
        store
            .eligible(&[SourceRef::library()], false)
            .unwrap()
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        vec!["visible"]
    );
    let invalid = asset("");
    assert!(store.activate_catalog(&[invalid], &[], &[], "g2").is_err());
    assert_eq!(store.meta("catalog_generation").unwrap(), json!("g1"));
    assert_eq!(store.assets().unwrap().len(), 1);
    store.feedback("visible", "never", None).unwrap();
    assert!(
        store
            .eligible(&[SourceRef::library()], false)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn legacy_migration_preserves_preferences_without_guessing_cloud_identity() {
    let (_temp, store) = state();
    store.import_legacy(json!({"revision":7,"interval_seconds":900,"sources":[{"kind":"person","id":"local-person"}]}),&["local-photo".into()],json!({"current":{"asset_id":"local-photo"},"history":[]})).unwrap();
    assert_eq!(store.settings().unwrap().interval_seconds, 900);
    assert_eq!(store.settings().unwrap().sources[0].id, "local-person");
    assert!(store.assets().unwrap().is_empty());
    assert_eq!(
        store.meta("migration_pending_ids").unwrap(),
        json!(["local-photo"])
    );
    store
        .import_legacy(json!({"interval_seconds":60}), &[], json!({}))
        .unwrap();
    assert_eq!(store.settings().unwrap().interval_seconds, 900);
}

#[test]
fn secure_directory_access_does_not_change_attributes() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("private");
    private_dir(&path).unwrap();
    let before = std::fs::metadata(&path).unwrap();
    private_dir(&path).unwrap();
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(
        (before.ctime(), before.ctime_nsec()),
        (after.ctime(), after.ctime_nsec())
    );
    assert_eq!(after.permissions().mode() & 0o777, 0o700);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    private_dir(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn repeated_state_updates_keep_the_wal_open_instead_of_recreating_it() {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::under(root.path());
    let store = Store::open(&paths).unwrap();
    store.set_meta("probe", &json!(1)).unwrap();
    let wal = std::path::PathBuf::from(format!("{}-wal", paths.database().display()));
    let inode = std::fs::metadata(&wal).unwrap().ino();
    store.set_meta("probe", &json!(2)).unwrap();
    assert_eq!(std::fs::metadata(wal).unwrap().ino(), inode);
}

#[test]
fn a_catalog_from_an_obsolete_apple_session_cannot_be_published() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(root.path())).unwrap();
    store
        .set_meta("sync_authorization_epoch", &json!(2))
        .unwrap();
    assert!(
        store
            .activate_catalog_checked(&[], &[], &[], "stale", Some((1, "old-account".into())))
            .is_err()
    );
    assert!(store.meta("catalog_generation").unwrap().is_null());
    store
        .activate_catalog_checked(
            &[],
            &[],
            &[],
            "current",
            Some((2, "current-account".into())),
        )
        .unwrap();
    assert_eq!(
        store.meta("base-account-id").unwrap(),
        json!("current-account")
    );
}
#[test]
fn bounded_private_reads_reject_oversized_files_and_nonregular_inputs() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("oversized-session");
    std::fs::write(&path, b"12345678901").unwrap();
    assert!(omarchy_photo_wallpapers::storage::read_bounded(&path, 10).is_err());
    assert!(omarchy_photo_wallpapers::storage::read_bounded(root.path(), 10).is_err());
    assert_eq!(
        omarchy_photo_wallpapers::storage::read_bounded(&path, 11).unwrap(),
        b"12345678901"
    );
}
