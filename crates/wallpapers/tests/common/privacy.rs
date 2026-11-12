use omarchy_photo_wallpapers::{
    engine::privacy::pixel_revision,
    imaging::Prepared,
    model::{Asset, Source, SourceRef},
    privacy::{
        policy::{Evidence, Signals},
        store::{AssetScope, ContentKey, DecisionKey, PrivacyStore},
    },
    storage::{Paths, Store},
};
use serde_json::json;
use sha2::{Digest, Sha256};

pub fn fixture() -> (tempfile::TempDir, Store, Prepared, ContentKey, DecisionKey) {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(root.path())).unwrap();
    store
        .set_meta("base-account-id", &json!("synthetic-account"))
        .unwrap();
    let asset = Asset {
        id: "asset".into(),
        library_id: "library".into(),
        revision: "metadata-v1".into(),
        rendition: "wallpaper".into(),
        width: 640,
        height: 400,
        ..Default::default()
    };
    store
        .activate_catalog(
            std::slice::from_ref(&asset),
            &[Source::library()],
            &[],
            "synthetic",
        )
        .unwrap();
    store
        .update_settings(0, json!({"sources":[SourceRef::library()]}))
        .unwrap();
    let privacy = PrivacyStore::new(store.clone());
    let preview = privacy.save_preview(b"synthetic-full-frame").unwrap();
    let content = ContentKey {
        scope: AssetScope {
            account: "synthetic-account".into(),
            library: "library".into(),
            asset: "asset".into(),
        },
        rendition: asset.rendition.clone(),
        rendition_revision: pixel_revision(&asset).unwrap(),
        media_sha256: "a".repeat(64),
        preview_sha256: preview,
    };
    let key = DecisionKey {
        modelset_sha256: "b".repeat(64),
        preprocessing_version: "full-frame-v1".into(),
        policy_version: "meeting-v1".into(),
    };
    let path = store.paths.data.join("synthetic.jpg");
    std::fs::write(&path, b"synthetic-render").unwrap();
    let prepared:Prepared=serde_json::from_value(json!({"asset_id":"asset","revision":"metadata-v1","render_path":path,"render_digest":format!("{:x}",Sha256::digest(b"synthetic-render")),"config_key":"synthetic","rect":{"x":0,"y":0,"width":640,"height":400},"mode":"fit","fingerprint":0,"score":0.5,"source_width":640,"source_height":400,"width":640,"height":400,"rendition":"wallpaper","decode_count":0,"cache_hit":true,"analysis_cache_hit":true})).unwrap();
    (root, store, prepared, content, key)
}
pub fn clear() -> Signals {
    Signals {
        nudity: Evidence::Clear,
        underwear: Evidence::Clear,
        bathing: Evidence::Clear,
        sleepwear: Evidence::Clear,
        swimwear: Evidence::Uncertain,
    }
}
