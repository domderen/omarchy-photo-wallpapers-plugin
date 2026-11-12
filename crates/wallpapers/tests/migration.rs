use omarchy_photo_wallpapers::{
    enrichment::{Binding, Enrichment, import},
    migration::*,
    model::{Asset, Source},
    storage::{Paths, Store, atomic_json},
};
use rusqlite::{Connection, params};
use serde_json::json;
fn legacy(store: &Store) {
    atomic_json(&store.paths.config.join("settings.json"),&json!({"sources":[{"kind":"person","id":"p1"},{"kind":"album","id":"local-album"}],"interval_seconds":900,"paused":true,"pinned":"local-1","enhancements":{"exposure":true,"white_balance":false,"contrast":false},"match_colors":false,"cache_bytes":10000000,"fallback":"fit","revision":7})).unwrap();
    let db = Connection::open(store.paths.data.join("feedback.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TABLE feedback(asset_id TEXT,action TEXT,crop TEXT,PRIMARY KEY(asset_id,action));",
    )
    .unwrap();
    for (id, action, crop) in [
        ("local-1", "never", "null"),
        ("local-2", "prefer", "null"),
        ("local-2", "crop", "{\"rect\":[0,0,400,300]}"),
    ] {
        db.execute(
            "INSERT INTO feedback VALUES(?1,?2,?3)",
            params![id, action, crop],
        )
        .unwrap();
    }
    atomic_json(&store.paths.data.join("engine.json"),&json!({"current":{"asset_id":"local-2","revision":"old-mac-revision","render_path":"/synthetic/unavailable-legacy-render.jpg"},"history":[],"history_index":-1})).unwrap();
    std::fs::create_dir_all(store.paths.data.join("catalogs")).unwrap();
    let generation = "a".repeat(64);
    atomic_json(
        &store.paths.data.join("catalogs/active.json"),
        &json!({"generation":generation}),
    )
    .unwrap();
    let db = Connection::open(
        store
            .paths
            .data
            .join(format!("catalogs/{generation}.sqlite")),
    )
    .unwrap();
    db.execute_batch("CREATE TABLE catalog_metadata(key TEXT PRIMARY KEY,value TEXT);")
        .unwrap();
    db.execute(
        "INSERT INTO catalog_metadata VALUES('namespace',?1)",
        [json!("mac-library").to_string()],
    )
    .unwrap();
}
fn base(store: &Store) {
    let assets: Vec<_> = (1..=2)
        .map(|n| Asset {
            id: format!("cloud-{n}"),
            cloud_master_guid: format!("master-{n}"),
            library_id: "icloud:123:private:PrimarySync:_defaultOwner".into(),
            revision: "cloud-revision".into(),
            width: 4000,
            height: 3000,
            ..Asset::default()
        })
        .collect();
    store
        .activate_catalog(&assets, &[Source::library()], &[], "cloud")
        .unwrap();
    store.set_meta("base-account-id", &json!("123")).unwrap();
}
fn overlay() -> Enrichment {
    serde_json::from_value(json!({"schema_version":1,"provider_namespace":"mac-library","cloud_id_type":"CPLMaster.recordName","created_at":"2026-10-22T12:00:00Z","people":[{"id":"p1","name":"First"}],"collections":[{"id":"local-album","kind":"album","name":"Local collection","available":true}],"assets":[{"local_id":"local-1","cloud_master_guid":"master-1","owner_hash":null,"person_ids":["p1"],"collection_ids":["local-album"]},{"local_id":"local-2","cloud_master_guid":"master-2","owner_hash":null,"person_ids":["p1"],"collection_ids":[]}]})).unwrap()
}
#[test]
fn legacy_preferences_are_preserved_and_ids_resolve_only_after_verified_enrichment() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(root.path())).unwrap();
    legacy(&store);
    assert!(discover_legacy(&store).unwrap().imported);
    assert_eq!(store.settings().unwrap().interval_seconds, 900);
    assert_eq!(store.settings().unwrap().pinned.as_deref(), Some("local-1"));
    assert!(store.meta("legacy_namespace").unwrap() == json!("mac-library"));
    assert_eq!(resolve_legacy(&store).unwrap().mapped_assets, 0);
    base(&store);
    let binding = Binding {
        namespace: "mac-library".into(),
        account_id: "123".into(),
        library_id: "icloud:123:private:PrimarySync:_defaultOwner".into(),
        owned_library_ids: Vec::new(),
    };
    import(&store, &overlay(), &binding).unwrap();
    let report = resolve_legacy(&store).unwrap();
    assert_eq!(report.mapped_assets, 2);
    let settings = store.settings().unwrap();
    assert_eq!(settings.interval_seconds, 900);
    assert!(settings.paused && settings.enhancements.exposure);
    assert_eq!(settings.pinned.as_deref(), Some("cloud-1"));
    assert!(
        settings
            .sources
            .iter()
            .all(|s| s.id.starts_with("mac:mac-library:"))
    );
    let eligible = store.eligible_ids(&settings.sources).unwrap();
    assert_eq!(eligible, vec!["cloud-2"]);
    let db = store.connection().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM feedback WHERE asset_id='cloud-2' AND action='prefer'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM feedback WHERE action='crop'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert!(!store.meta("legacy_pending_crops").unwrap().is_null());
    assert!(!discover_legacy(&store).unwrap().imported);
    let revision = settings.revision;
    resolve_legacy(&store).unwrap();
    assert_eq!(store.settings().unwrap().revision, revision);
}
#[test]
fn namespace_or_account_mismatch_keeps_explicit_sources_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(root.path())).unwrap();
    legacy(&store);
    discover_legacy(&store).unwrap();
    base(&store);
    let mut doc = overlay();
    doc.provider_namespace = "different-mac-library".into();
    let binding = Binding {
        namespace: doc.provider_namespace.clone(),
        account_id: "123".into(),
        library_id: "icloud:123:private:PrimarySync:_defaultOwner".into(),
        owned_library_ids: Vec::new(),
    };
    import(&store, &doc, &binding).unwrap();
    assert_eq!(resolve_legacy(&store).unwrap().mapped_assets, 0);
    assert_eq!(store.settings().unwrap().sources[0].id, "p1");
    assert!(
        store
            .eligible_ids(&store.settings().unwrap().sources)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn fresh_install_has_no_helper_or_legacy_dependency() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(root.path())).unwrap();
    assert!(!discover_legacy(&store).unwrap().imported);
    assert_eq!(store.settings().unwrap().interval_seconds, 3600);
    assert!(store.settings().unwrap().sources.is_empty());
}
#[test]
fn completed_preview_session_handoff_preserves_existing_settings_and_refuses_live_or_existing_accounts()
 {
    use omarchy_photo_wallpapers::{platform, storage::private_dir};
    use std::os::unix::fs::PermissionsExt;
    let source_root = tempfile::tempdir().unwrap();
    let target_root = tempfile::tempdir().unwrap();
    let mut source = Paths::under(source_root.path());
    let target = Paths::under(target_root.path());
    source.runtime = target.runtime.clone(); // isolated sign-in can share the production socket
    private_dir(&source.config).unwrap();
    let store = Store::open(&target).unwrap();
    legacy(&store);
    let session = json!({"username":"synthetic@example.invalid","client_id":"fixture","headers":{"session_token":"synthetic-token"},"cookies":null,"stage":"authenticated","account":{"dsInfo":{"dsid":"123"},"webservices":{"ckdatabasews":{"url":"https://p01-ckdatabasews.icloud.com"}}}});
    atomic_json(&source.config.join("apple-session.json"), &session).unwrap();
    let lock = platform::lock(&source).unwrap();
    assert!(adopt_session(&source, &target).is_err());
    assert!(!target.config.join("apple-session.json").exists());
    drop(lock);
    assert!(adopt_session(&source, &target).unwrap()["adopted"] == true);
    assert_eq!(
        std::fs::metadata(target.config.join("apple-session.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(target.config.join("settings.json")).unwrap()
        )
        .unwrap()["interval_seconds"],
        900
    );
    assert!(adopt_session(&source, &target).is_err());
    assert_eq!(store.settings().unwrap().interval_seconds, 3600); // handoff does not rewrite canonical state
    assert!(source.config.join("apple-session.json").exists());
}
#[test]
fn legacy_current_missing_from_history_is_inserted_without_replacing_newer_native_current() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(root.path())).unwrap();
    legacy(&store);
    let path = store.paths.data.join("legacy.jpg");
    image::RgbImage::new(80, 50).save(&path).unwrap();
    let old = json!({"asset_id":"local-2","revision":"old-mac-revision","render_path":path,"applied_at":"2026-10-20T10:00:00Z"});
    atomic_json(
        &store.paths.data.join("engine.json"),
        &json!({"current":old,"history":[]}),
    )
    .unwrap();
    discover_legacy(&store).unwrap();
    base(&store);
    import(
        &store,
        &overlay(),
        &Binding {
            namespace: "mac-library".into(),
            account_id: "123".into(),
            library_id: "icloud:123:private:PrimarySync:_defaultOwner".into(),
            owned_library_ids: Vec::new(),
        },
    )
    .unwrap();
    resolve_legacy(&store).unwrap();
    let history = store.meta("history-v2").unwrap();
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0]["revision"], "old-mac-revision");
    assert_eq!(history[0]["provenance"], "legacy-mac-render");
    store
        .set_meta("current-v2", &json!({"asset_id":"new-native-current"}))
        .unwrap();
    store
        .set_meta("catalog_generation", &json!("updated-cloud-catalog"))
        .unwrap();
    resolve_legacy(&store).unwrap();
    assert_eq!(
        store.meta("current-v2").unwrap()["asset_id"],
        "new-native-current"
    );
}
#[test]
fn verified_catalog_handoff_preserves_destination_preferences_and_feedback() {
    let source_root = tempfile::tempdir().unwrap();
    let target_root = tempfile::tempdir().unwrap();
    let source = Store::open(&Paths::under(source_root.path())).unwrap();
    let target = Store::open(&Paths::under(target_root.path())).unwrap();
    legacy(&target);
    discover_legacy(&target).unwrap();
    base(&source);
    let session = json!({"username":"synthetic@example.invalid","client_id":"fixture","headers":{"session_token":"synthetic"},"cookies":null,"stage":"authenticated","account":{"dsInfo":{"dsid":"123"},"webservices":{"ckdatabasews":{"url":"https://p01-ckdatabasews.icloud.com"}}}});
    atomic_json(&source.paths.config.join("apple-session.json"), &session).unwrap();
    adopt_session(&source.paths, &target.paths).unwrap();
    let lock = omarchy_photo_wallpapers::platform::lock(&source.paths).unwrap();
    assert!(adopt_catalog(&source.paths, &target.paths).is_err());
    drop(lock);
    let report = adopt_catalog(&source.paths, &target.paths).unwrap();
    assert_eq!(report["assets"], 2);
    assert_eq!(target.settings().unwrap().interval_seconds, 900);
    assert_eq!(target.settings().unwrap().sources[0].id, "p1");
    assert_eq!(
        target
            .meta("legacy_pending_feedback")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(target.assets().unwrap().len(), 2);
    assert_eq!(source.assets().unwrap().len(), 2);
    assert!(adopt_catalog(&source.paths, &target.paths).is_err());
}
