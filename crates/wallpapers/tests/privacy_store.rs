use omarchy_photo_wallpapers::{
    privacy::{
        policy::{Evidence, PrivacyChoice, Signals},
        store::{AssetScope, ContentKey, Decision, DecisionKey, PrivacyStore},
    },
    storage::{Paths, Store},
};
use serde_json::json;

fn fixture() -> (
    tempfile::TempDir,
    Store,
    PrivacyStore,
    ContentKey,
    DecisionKey,
) {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(root.path())).unwrap();
    let privacy = PrivacyStore::new(store.clone());
    let preview = privacy.save_preview(b"synthetic-full-frame").unwrap();
    let content = ContentKey {
        scope: AssetScope {
            account: "account-a".into(),
            library: "library-a".into(),
            asset: "asset-a".into(),
        },
        rendition: "wallpaper".into(),
        rendition_revision: "pixels-v1".into(),
        media_sha256: "a".repeat(64),
        preview_sha256: preview,
    };
    let identity = DecisionKey {
        modelset_sha256: "b".repeat(64),
        preprocessing_version: "full-frame-v1".into(),
        policy_version: "meeting-v1".into(),
    };
    (root, store, privacy, content, identity)
}
fn signals() -> Signals {
    Signals {
        nudity: Evidence::Clear,
        underwear: Evidence::Clear,
        bathing: Evidence::Clear,
        sleepwear: Evidence::Clear,
        swimwear: Evidence::Uncertain,
    }
}

#[test]
fn verified_decisions_survive_metadata_updates_and_restart() {
    let (_root, store, privacy, content, key) = fixture();
    privacy
        .save(&Decision::from_signals(
            content.clone(),
            key.clone(),
            signals(),
        ))
        .unwrap();
    store
        .set_meta(
            "catalog_metadata_revision",
            &json!("new-names-and-favorites"),
        )
        .unwrap();
    let reopened = PrivacyStore::new(Store::open(&store.paths).unwrap());
    assert!(reopened.lookup(&content, &key).unwrap().is_some());
    assert_eq!(
        store
            .connection()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        3
    );
}

#[test]
fn changed_pixels_model_or_policy_cannot_reuse_a_clear_decision() {
    let (_root, _store, privacy, content, key) = fixture();
    privacy
        .save(&Decision::from_signals(
            content.clone(),
            key.clone(),
            signals(),
        ))
        .unwrap();
    let mut changed = content.clone();
    changed.media_sha256 = "c".repeat(64);
    assert!(privacy.lookup(&changed, &key).unwrap().is_none());
    changed = content.clone();
    changed.rendition_revision = "pixels-v2".into();
    assert!(privacy.lookup(&changed, &key).unwrap().is_none());
    let mut identity = key.clone();
    identity.modelset_sha256 = "d".repeat(64);
    assert!(privacy.lookup(&content, &identity).unwrap().is_none());
    identity = key.clone();
    identity.preprocessing_version = "center-crop-v0".into();
    assert!(privacy.lookup(&content, &identity).unwrap().is_none());
    identity = key.clone();
    identity.policy_version = "meeting-v2".into();
    assert!(privacy.lookup(&content, &identity).unwrap().is_none());
}

#[test]
fn missing_tampered_or_linked_full_frame_proof_is_not_automatically_admitted() {
    let (_root, _store, privacy, content, key) = fixture();
    privacy
        .save(&Decision::from_signals(
            content.clone(),
            key.clone(),
            signals(),
        ))
        .unwrap();
    let path = privacy.preview_path(&content.preview_sha256).unwrap();
    std::fs::write(&path, b"changed").unwrap();
    assert!(privacy.lookup(&content, &key).unwrap().is_none());
    std::fs::remove_file(&path).unwrap();
    assert!(privacy.lookup(&content, &key).unwrap().is_none());
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), &path).unwrap();
    assert!(privacy.lookup(&content, &key).unwrap().is_none());
}

#[test]
fn allow_corrections_are_content_bound_and_personal_only_is_asset_bound() {
    let (_root, _store, privacy, content, _key) = fixture();
    privacy.set_choice(&content, PrivacyChoice::Allow).unwrap();
    assert_eq!(privacy.choice_for(&content).unwrap(), PrivacyChoice::Allow);
    let mut edited = content.clone();
    edited.media_sha256 = "c".repeat(64);
    assert_eq!(privacy.choice_for(&edited).unwrap(), PrivacyChoice::Auto);
    privacy
        .set_choice(&content, PrivacyChoice::PersonalOnly)
        .unwrap();
    assert_eq!(
        privacy.choice_for(&edited).unwrap(),
        PrivacyChoice::PersonalOnly
    );
    for scope in ["account", "library", "asset"] {
        let mut unrelated = content.clone();
        match scope {
            "account" => unrelated.scope.account = "other".into(),
            "library" => unrelated.scope.library = "other".into(),
            _ => unrelated.scope.asset = "other".into(),
        }
        assert_eq!(privacy.choice_for(&unrelated).unwrap(), PrivacyChoice::Auto);
    }
    privacy.set_choice(&content, PrivacyChoice::Auto).unwrap();
    assert_eq!(privacy.choice_for(&content).unwrap(), PrivacyChoice::Auto);
}

#[test]
fn corrupt_or_internally_inconsistent_records_never_become_clear() {
    let (_root, store, privacy, content, key) = fixture();
    privacy
        .save(&Decision::from_signals(
            content.clone(),
            key.clone(),
            signals(),
        ))
        .unwrap();
    store
        .connection()
        .unwrap()
        .execute("UPDATE privacy_decisions SET data='{}'", [])
        .unwrap();
    assert!(privacy.lookup(&content, &key).unwrap().is_none());
    let mut decision = Decision::from_signals(content.clone(), key, Signals::default());
    decision.state = omarchy_photo_wallpapers::privacy::policy::ScreeningState::Clear;
    assert!(privacy.save(&decision).is_err());
}

#[test]
fn schema_migration_preserves_existing_feedback_and_settings() {
    let (_root, store, _privacy, _content, _key) = fixture();
    let settings = store.settings().unwrap();
    assert_eq!(settings.interval_seconds, 3600);
    assert!(!settings.privacy.active());
    store
        .set_meta("legacy_feedback", &json!({"keep":"forever"}))
        .unwrap();
    Store::open(&store.paths).unwrap();
    assert_eq!(store.meta("legacy_feedback").unwrap()["keep"], "forever");
}

#[test]
fn corrupted_corrections_fail_closed_instead_of_losing_an_explicit_exclusion() {
    let (_root, store, privacy, content, _key) = fixture();
    privacy
        .set_choice(&content, PrivacyChoice::PersonalOnly)
        .unwrap();
    store
        .connection()
        .unwrap()
        .execute("UPDATE privacy_choices SET data='invalid'", [])
        .unwrap();
    assert_eq!(
        privacy.choice_for(&content).unwrap(),
        PrivacyChoice::PersonalOnly
    );
}

#[test]
fn version_two_database_upgrades_without_rewriting_feedback_or_render_cache() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::under(root.path());
    let store = Store::open(&paths).unwrap();
    store.connection().unwrap().execute_batch("DROP TABLE privacy_decisions; DROP TABLE privacy_choices; PRAGMA user_version=2; INSERT INTO feedback(asset_id,action) VALUES('synthetic','never'); INSERT INTO analysis(key,data) VALUES('existing','{}'); UPDATE meta SET value='{\"revision\":7,\"interval_seconds\":900,\"paused\":true,\"pinned\":\"synthetic\"}' WHERE key='settings';").unwrap();
    drop(store);
    let migrated = Store::open(&paths).unwrap();
    let settings = migrated.settings().unwrap();
    assert_eq!(settings.revision, 7);
    assert_eq!(settings.interval_seconds, 900);
    assert!(settings.paused);
    assert_eq!(settings.pinned.as_deref(), Some("synthetic"));
    assert!(!settings.privacy.active());
    let db = migrated.connection().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT action FROM feedback WHERE asset_id='synthetic'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "never"
    );
    assert_eq!(
        db.query_row("SELECT data FROM analysis WHERE key='existing'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "{}"
    );
    assert!(
        PrivacyStore::new(migrated)
            .lookup(
                &ContentKey {
                    scope: AssetScope {
                        account: "a".into(),
                        library: "l".into(),
                        asset: "id".into()
                    },
                    rendition: "wallpaper".into(),
                    rendition_revision: "1".into(),
                    media_sha256: "a".repeat(64),
                    preview_sha256: "b".repeat(64)
                },
                &DecisionKey {
                    modelset_sha256: "c".repeat(64),
                    preprocessing_version: "v1".into(),
                    policy_version: "v1".into()
                }
            )
            .unwrap()
            .is_none()
    );
}
