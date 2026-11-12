use omarchy_photo_wallpapers::{
    enrichment::*,
    model::*,
    storage::{Paths, Store},
};
use serde_json::json;
fn asset(id: &str, master: &str) -> Asset {
    Asset {
        id: id.into(),
        cloud_guid: id.into(),
        cloud_master_guid: master.into(),
        library_id: "icloud:123:private:PrimarySync:_defaultOwner".into(),
        revision: "r".into(),
        width: 4000,
        height: 3000,
        ..Asset::default()
    }
}
fn document() -> Enrichment {
    serde_json::from_value(json!({"schema_version":1,"provider_namespace":"mac-library","cloud_id_type":"CPLMaster.recordName","created_at":"2026-10-22T12:00:00Z","people":[{"id":"p1","name":"First"}],"assets":[{"local_id":"local-1","cloud_master_guid":"master-1","owner_hash":null,"person_ids":["p1"],"keywords":[]}]})).unwrap()
}
fn binding() -> Binding {
    Binding {
        namespace: "mac-library".into(),
        account_id: "123".into(),
        library_id: "icloud:123:private:PrimarySync:_defaultOwner".into(),
        owned_library_ids: Vec::new(),
    }
}
#[test]
fn collection_lookup_keeps_only_known_available_membership_without_duplicates() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(temp.path())).unwrap();
    store
        .activate_catalog(&[asset("cloud-1", "master-1")], &[], &[], "a")
        .unwrap();
    let mut doc = document();
    doc.collections = vec![
        MetadataCollection {
            id: "good".into(),
            kind: "album".into(),
            name: "Good".into(),
            available: true,
        },
        MetadataCollection {
            id: "disabled".into(),
            kind: "album".into(),
            name: "Unavailable".into(),
            available: false,
        },
    ];
    doc.assets[0].collection_ids = vec![
        "good".into(),
        "unknown".into(),
        "disabled".into(),
        "good".into(),
    ];
    import(&store, &doc, &binding()).unwrap();
    for (id, count) in [("good", 1), ("disabled", 0)] {
        let source = store
            .sources()
            .unwrap()
            .into_iter()
            .find(|s| s.id == format!("mac:mac-library:collection:{id}"))
            .unwrap();
        assert_eq!(source.count, count);
    }
}

#[test]
fn oversized_collection_document_cannot_replace_a_good_overlay() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(temp.path())).unwrap();
    store
        .activate_catalog(&[asset("cloud-1", "master-1")], &[], &[], "a")
        .unwrap();
    import(&store, &document(), &binding()).unwrap();
    let before = store.sources().unwrap().len();
    let mut invalid = document();
    invalid.collections = (0..100001)
        .map(|n| MetadataCollection {
            id: format!("collection-{n}"),
            kind: "album".into(),
            name: "Synthetic".into(),
            available: true,
        })
        .collect();
    assert!(
        matches!(import(&store, &invalid, &binding()), Err(error) if error.to_string() == "enrichment_size_limit")
    );
    assert_eq!(store.sources().unwrap().len(), before);
}

#[test]
fn exact_master_match_keeps_local_identity_separate_and_stale_membership_usable() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(temp.path())).unwrap();
    store
        .activate_catalog(&[asset("cloud-1", "master-1")], &[], &[], "a")
        .unwrap();
    let report = import(&store, &document(), &binding()).unwrap();
    assert_eq!(report.matched, 1);
    let sources = store.sources().unwrap();
    let person = sources.iter().find(|s| s.kind == "person").unwrap();
    assert_eq!(person.count, 1);
    let selected = vec![SourceRef {
        kind: "person".into(),
        id: person.id.clone(),
    }];
    assert_eq!(
        store
            .eligible(&selected, false)
            .unwrap()
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        vec!["cloud-1"]
    );
    mark_stale(&store, "mac-library").unwrap();
    assert!(
        store
            .sources()
            .unwrap()
            .iter()
            .find(|s| s.kind == "person")
            .unwrap()
            .stale
    );
    assert_eq!(
        store
            .eligible(&selected, false)
            .unwrap()
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        vec!["cloud-1"]
    );
    store.activate_catalog(&[], &[], &[], "b").unwrap();
    assert!(store.eligible(&selected, false).unwrap().is_empty());
}
#[test]
fn account_library_and_ambiguous_mappings_cannot_attach_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(temp.path())).unwrap();
    store
        .activate_catalog(
            &[asset("a", "master-1"), asset("b", "master-1")],
            &[],
            &[],
            "a",
        )
        .unwrap();
    assert_eq!(import(&store, &document(), &binding()).unwrap().matched, 0);
    let mut wrong = binding();
    wrong.account_id = "999".into();
    assert!(import(&store, &document(), &wrong).is_err());
    let mut wrong_doc = document();
    wrong_doc.provider_namespace = "other-library".into();
    assert!(import(&store, &wrong_doc, &binding()).is_err());
    let mut doc = document();
    doc.assets[0].owner_hash = Some("unverified-shared-owner".into());
    assert_eq!(import(&store, &doc, &binding()).unwrap().matched, 0);
    let mut doc = document();
    doc.assets[0].cloud_master_guid = None;
    assert_eq!(import(&store, &doc, &binding()).unwrap().matched, 0);
}
#[test]
fn duplicate_local_master_mappings_are_rejected_and_last_good_overlay_survives() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(temp.path())).unwrap();
    store
        .activate_catalog(&[asset("a", "master-1")], &[], &[], "a")
        .unwrap();
    import(&store, &document(), &binding()).unwrap();
    let mut invalid = document();
    let mut other = invalid.assets[0].clone();
    other.local_id = "duplicate-filename-unrelated-local-id".into();
    invalid.assets.push(other);
    assert_eq!(import(&store, &invalid, &binding()).unwrap().matched, 0);
    let mut invalid = document();
    invalid.schema_version = 99;
    assert!(import(&store, &invalid, &binding()).is_err());
}

#[test]
fn freshness_only_refresh_does_not_republish_membership_or_invalidate_ready_work() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(temp.path())).unwrap();
    store
        .activate_catalog(&[asset("a", "master-1")], &[], &[], "a")
        .unwrap();
    let first = import(&store, &document(), &binding()).unwrap();
    assert!(first.changed);
    mark_stale(&store, "mac-library").unwrap();
    let mut next = document();
    next.created_at = "2026-10-22T12:15:00Z".into();
    let second = import(&store, &next, &binding()).unwrap();
    assert!(!second.changed);
    assert_eq!(second.matched, 1);
    assert!(
        !store
            .sources()
            .unwrap()
            .iter()
            .find(|s| s.kind == "person")
            .unwrap()
            .stale
    );
}
#[test]
fn a_helper_bound_to_the_preceding_account_cannot_republish_after_account_switch() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(temp.path())).unwrap();
    store
        .activate_catalog(&[asset("a", "master-1")], &[], &[], "first-account")
        .unwrap();
    store.set_meta("base-account-id", &json!("123")).unwrap();
    import(&store, &document(), &binding()).unwrap();
    store.set_meta("base-account-id", &json!("999")).unwrap();
    assert!(import(&store, &document(), &binding()).is_err());
}
#[test]
fn owned_shared_library_metadata_requires_unique_matches_and_excludes_other_owners() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(root.path())).unwrap();
    let personal = asset("personal", "master-1");
    let mut owned = asset("owned-shared", "master-2");
    owned.library_id = "icloud:123:private:SharedSync-owned:_defaultOwner".into();
    let mut external = asset("external-shared", "master-3");
    external.library_id = "icloud:123:shared:SharedSync-external:another-owner".into();
    store
        .activate_catalog(
            &[personal.clone(), owned.clone(), external.clone()],
            &[],
            &[],
            "complete",
        )
        .unwrap();
    store.set_meta("base-account-id", &json!("123")).unwrap();
    let mut doc = document();
    for n in [2, 3] {
        let mut item = doc.assets[0].clone();
        item.local_id = format!("local-{n}");
        item.cloud_master_guid = Some(format!("master-{n}"));
        doc.assets.push(item);
    }
    let binding = Binding::for_account(&store, "123", "mac-library").unwrap();
    let report = import(&store, &doc, &binding).unwrap();
    assert_eq!(
        (report.matched, report.unmatched, report.ambiguous),
        (2, 1, 0)
    );
    let mut duplicate = owned.clone();
    duplicate.id = "ambiguous-copy".into();
    duplicate.cloud_master_guid = "master-1".into();
    store
        .activate_catalog(&[personal, owned, external, duplicate], &[], &[], "next")
        .unwrap();
    let report = import(&store, &doc, &binding).unwrap();
    assert_eq!(
        (report.matched, report.unmatched, report.ambiguous),
        (1, 1, 1)
    );
}
