use omarchy_photo_wallpapers::{
    engine::privacy::{AdmissionContext, admit},
    imaging::Prepared,
    model::Asset,
    privacy::{
        policy::PrivacyChoice,
        store::{Decision, DecisionKey, PrivacyStore},
    },
    storage::Store,
};
use serde_json::json;
use sha2::{Digest, Sha256};

#[path = "common/privacy.rs"]
mod fixtures;
use fixtures::{clear, fixture};
fn context(key: DecisionKey) -> AdmissionContext {
    AdmissionContext {
        generation: 0,
        policy_key: Some(key),
        active: true,
    }
}

#[test]
fn cached_pixels_alone_never_authorize_a_photo_for_meetings() {
    let (_root, store, prepared, content, key) = fixture();
    let privacy = PrivacyStore::new(store.clone());
    assert!(!admit(&store, &prepared, &context(key.clone())).unwrap());
    privacy
        .save(&Decision::from_signals(
            content.clone(),
            key.clone(),
            clear(),
        ))
        .unwrap();
    assert!(!admit(&store, &prepared, &context(key.clone())).unwrap());
    privacy
        .bind_render(&prepared.render_digest, &content)
        .unwrap();
    assert!(admit(&store, &prepared, &context(key)).unwrap());
}
#[test]
fn missing_full_frame_proof_and_changed_policy_cannot_restore_cached_or_history_pixels() {
    let (_root, store, prepared, content, key) = fixture();
    let privacy = PrivacyStore::new(store.clone());
    privacy
        .save(&Decision::from_signals(
            content.clone(),
            key.clone(),
            clear(),
        ))
        .unwrap();
    privacy
        .bind_render(&prepared.render_digest, &content)
        .unwrap();
    let mut changed = key.clone();
    changed.policy_version = "v2".into();
    assert!(!admit(&store, &prepared, &context(changed)).unwrap());
    std::fs::remove_file(privacy.preview_path(&content.preview_sha256).unwrap()).unwrap();
    assert!(!admit(&store, &prepared, &context(key)).unwrap());
}
#[test]
fn never_and_current_account_are_stronger_than_a_content_bound_allow() {
    let (_root, store, prepared, content, key) = fixture();
    let privacy = PrivacyStore::new(store.clone());
    privacy
        .bind_render(&prepared.render_digest, &content)
        .unwrap();
    privacy.set_choice(&content, PrivacyChoice::Allow).unwrap();
    assert!(admit(&store, &prepared, &context(key.clone())).unwrap());
    store
        .set_meta("base-account-id", &json!("another-account"))
        .unwrap();
    assert!(!admit(&store, &prepared, &context(key.clone())).unwrap());
    store
        .set_meta("base-account-id", &json!("synthetic-account"))
        .unwrap();
    store.feedback("asset", "never", None).unwrap();
    assert!(!admit(&store, &prepared, &context(key)).unwrap());
}
#[test]
fn stale_generations_and_tampered_render_pixels_cannot_apply() {
    let (_root, store, prepared, content, key) = fixture();
    let privacy = PrivacyStore::new(store.clone());
    privacy
        .bind_render(&prepared.render_digest, &content)
        .unwrap();
    privacy.set_choice(&content, PrivacyChoice::Allow).unwrap();
    store.set_meta("privacy-generation", &json!(1)).unwrap();
    assert!(!admit(&store, &prepared, &context(key.clone())).unwrap());
    let mut ctx = context(key);
    ctx.generation = 1;
    assert!(admit(&store, &prepared, &ctx).unwrap());
    std::fs::write(&prepared.render_path, b"changed").unwrap();
    assert!(!admit(&store, &prepared, &ctx).unwrap());
}

#[tokio::test]
async fn native_cached_render_fast_path_cannot_bypass_meeting_admission() {
    use omarchy_photo_wallpapers::{
        engine::{NativePipeline, Pipeline},
        imaging::{PrepareOptions, config_key},
    };
    let (_root, store, mut prepared, _content, _key) = fixture();
    let options = PrepareOptions {
        width: 640,
        height: 400,
        person_sensitive: false,
        fallback: "fit".into(),
        enhancements: Default::default(),
        preferred_crop: None,
    };
    prepared.config_key = config_key(&options).unwrap();
    store.connection().unwrap().execute("INSERT INTO renders(key,asset_id,revision,config_key,data,used) VALUES('synthetic','asset','metadata-v1',?1,?2,0)",rusqlite::params![prepared.config_key,serde_json::to_string(&prepared).unwrap()]).unwrap();
    let mut settings = store.settings().unwrap();
    settings.privacy.meeting = true;
    store
        .set_meta("settings", &serde_json::to_value(settings).unwrap())
        .unwrap();
    let result = NativePipeline::default()
        .prepare(
            store.clone(),
            store.asset("asset").unwrap().unwrap(),
            options,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(
        result.is_err(),
        "an unscreened cached photo was returned for Meeting Mode"
    );
}

#[test]
fn decoded_pixels_cannot_be_attached_to_a_changed_source_file() {
    use omarchy_photo_wallpapers::imaging::{PrepareOptions, prepare_with_decoded};
    let (_root, store, _prepared, _content, _key) = fixture();
    let path = store.paths.data.join("source.png");
    let source = image::RgbImage::from_pixel(640, 400, image::Rgb([40, 120, 180]));
    source.save(&path).unwrap();
    let digest = format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap()));
    image::RgbImage::from_pixel(640, 400, image::Rgb([200, 30, 40]))
        .save(&path)
        .unwrap();
    let options = PrepareOptions {
        width: 640,
        height: 400,
        person_sensitive: false,
        fallback: "fit".into(),
        enhancements: Default::default(),
        preferred_crop: None,
    };
    omarchy_photo_wallpapers::imaging::prepare(
        &store,
        &store.asset("asset").unwrap().unwrap(),
        &path,
        &options,
        &tokio_util::sync::CancellationToken::new(),
    )
    .unwrap();
    let error = prepare_with_decoded(
        &store,
        &store.asset("asset").unwrap().unwrap(),
        &path,
        &options,
        &tokio_util::sync::CancellationToken::new(),
        Some((digest, source)),
    )
    .err()
    .expect("changed source accepted");
    assert!(error.to_string().contains("source_changed"));
}

#[derive(Default)]
struct CapturePlatform(std::sync::Mutex<Vec<String>>);
#[async_trait::async_trait]
impl omarchy_photo_wallpapers::platform::Platform for CapturePlatform {
    async fn geometry(&self) -> anyhow::Result<(u32, u32)> {
        Ok((640, 400))
    }
    async fn apply(&self, prepared: &Prepared) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(prepared.provenance.clone());
        Ok(())
    }
}
struct NoColdPipeline;
#[async_trait::async_trait]
impl omarchy_photo_wallpapers::engine::Pipeline for NoColdPipeline {
    async fn prepare(
        &self,
        _: Store,
        _: Asset,
        _: omarchy_photo_wallpapers::imaging::PrepareOptions,
        _: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<Prepared> {
        anyhow::bail!("fixture_no_cold_source")
    }
}

#[derive(Default)]
struct TrackColdPreparation {
    inner: omarchy_photo_wallpapers::engine::NativePipeline,
    cold: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl omarchy_photo_wallpapers::engine::Pipeline for TrackColdPreparation {
    async fn prepare(
        &self,
        store: Store,
        asset: Asset,
        options: omarchy_photo_wallpapers::imaging::PrepareOptions,
        stop: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<Prepared> {
        if asset.id.contains("uncleared") {
            self.cold.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.inner.prepare(store, asset, options, stop).await
    }
}

#[tokio::test]
async fn protected_next_uses_cleared_cached_alternatives_before_unscreened_work() {
    use omarchy_photo_wallpapers::{
        engine::Engine,
        imaging::{PrepareOptions, config_key},
        ipc::Request,
        model::Source,
    };
    use std::{
        sync::{Arc, atomic::Ordering},
        time::Duration,
    };
    for meeting in [false, true] {
        let (_root, store, mut prepared, content, key) = fixture();
        let original = store.asset("asset").unwrap().unwrap();
        let mut assets = vec![original.clone()];
        for id in ["000-uncleared", "001-uncleared", "warm-alternative"] {
            let mut asset = original.clone();
            asset.id = id.into();
            assets.push(asset);
        }
        store
            .activate_catalog(&assets, &[Source::library()], &[], "synthetic")
            .unwrap();
        let options = PrepareOptions {
            width: 640,
            height: 400,
            person_sensitive: false,
            fallback: "fit".into(),
            enhancements: Default::default(),
            preferred_crop: None,
        };
        prepared.config_key = config_key(&options).unwrap();
        for asset in &assets {
            let mut cached = prepared.clone();
            cached.asset_id = asset.id.clone();
            store.connection().unwrap().execute("INSERT INTO renders(key,asset_id,revision,config_key,data,used) VALUES(?1,?1,?2,?3,?4,0)", rusqlite::params![asset.id,asset.revision,cached.config_key,serde_json::to_string(&cached).unwrap()]).unwrap();
        }
        let privacy = PrivacyStore::new(store.clone());
        for id in ["asset", "warm-alternative"] {
            let mut evidence = content.clone();
            evidence.scope.asset = id.into();
            privacy
                .save(&Decision::from_signals(
                    evidence.clone(),
                    key.clone(),
                    clear(),
                ))
                .unwrap();
            privacy
                .bind_render(&prepared.render_digest, &evidence)
                .unwrap();
        }
        store
            .set_meta("privacy-policy-v1", &serde_json::to_value(&key).unwrap())
            .unwrap();
        store
            .set_meta("current-v2", &serde_json::to_value(&prepared).unwrap())
            .unwrap();
        let mut settings = store.settings().unwrap();
        settings.paused = true;
        settings.privacy.meeting = meeting;
        settings.privacy.filter_personal = !meeting;
        // A strong night preference must stay inside the cleared candidate pool.
        settings.match_colors = meeting;
        settings.theme.mode = "dark".into();
        for id in ["000-uncleared", "001-uncleared"] {
            store
                .feedback(id, "period", Some(json!({"period":"night"})))
                .unwrap();
        }
        store
            .feedback("warm-alternative", "period", Some(json!({"period":"day"})))
            .unwrap();
        store
            .set_meta("settings", &serde_json::to_value(settings).unwrap())
            .unwrap();
        let pipeline = Arc::new(TrackColdPreparation::default());
        let engine = Engine::start_with_theme_runtime(
            store.clone(),
            Arc::new(CapturePlatform::default()),
            pipeline.clone(),
            false,
            None,
        )
        .await
        .unwrap();
        for (operation, phase) in [("preloaded", "preload"), ("frame", "frame")] {
            let status = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let status = engine.status();
                    if status["privacy"]["phase"] == phase {
                        break status;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("fixture surface never became ready");
            let p = &status["privacy"];
            for output in p["outputs"].as_array().unwrap() {
                engine.request(Request {command:"privacy".into(),operation:operation.into(),input:json!({"generation":p["generation"],"transition_id":p["transition_id"],"output":output}),..Default::default()}).await.unwrap();
            }
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while engine.status()["privacy"]["state"] != "active" {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        engine
            .request(Request {
                command: "next".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let advanced = tokio::time::timeout(Duration::from_secs(3), async {
            while engine.status()["current"]["asset_id"] != "warm-alternative" {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .is_ok();
        let cold = pipeline.cold.load(Ordering::Relaxed);
        let metrics = engine.status()["metrics"].clone();
        engine.shutdown().await;
        assert!(
            advanced,
            "an already cleared cached alternative was not applied"
        );
        assert_eq!(
            cold, 0,
            "protected Next tried unscanned work before its cleared cached alternative"
        );
        assert_eq!(metrics["analysis_jobs"], 0);
    }
}

#[tokio::test]
async fn startup_ready_queue_and_previous_history_are_revalidated() {
    use omarchy_photo_wallpapers::{
        engine::Engine,
        imaging::{PrepareOptions, config_key},
        ipc::Request,
    };
    use std::sync::Arc;
    let (_root, store, mut prepared, _content, _key) = fixture();
    prepared.config_key = config_key(&PrepareOptions {
        width: 640,
        height: 400,
        person_sensitive: false,
        fallback: "fit".into(),
        enhancements: Default::default(),
        preferred_crop: None,
    })
    .unwrap();
    store
        .set_meta("ready-v2", &json!([prepared.clone()]))
        .unwrap();
    store
        .set_meta("history-v2", &json!([prepared.clone(), prepared]))
        .unwrap();
    store.set_meta("history-index-v2", &json!(1)).unwrap();
    let mut settings = store.settings().unwrap();
    settings.paused = true;
    settings.privacy.meeting = true;
    store
        .set_meta("settings", &serde_json::to_value(settings).unwrap())
        .unwrap();
    let platform = Arc::new(CapturePlatform::default());
    let engine = Engine::start_with_theme_runtime(
        store.clone(),
        platform.clone(),
        Arc::new(NoColdPipeline),
        false,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        engine.status()["ready_count"],
        0,
        "old unscreened ready item survived Meeting startup"
    );
    let response = engine
        .request(Request {
            command: "previous".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let id = response["job_id"].as_str().unwrap();
    let mut result = None;
    for _ in 0..200 {
        let data: Option<String> = store
            .connection()
            .unwrap()
            .query_row(
                "SELECT result FROM commands WHERE id=?1 AND state='done'",
                [id],
                |r| r.get(0),
            )
            .ok();
        if data.is_some() {
            result = data;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&result.expect("previous did not finish"))
            .unwrap()["error"],
        "no_previous_photo"
    );
    assert!(
        platform
            .0
            .lock()
            .unwrap()
            .iter()
            .all(|provenance| provenance == "meeting-neutral")
    );
    engine.shutdown().await;
}

#[test]
fn full_frame_previews_share_the_disk_budget_without_evicting_current_evidence() {
    let (_root, store, _prepared, content, _key) = fixture();
    let privacy = PrivacyStore::new(store.clone());
    let first = privacy.save_preview(&vec![10; 600_000]).unwrap();
    let second = privacy.save_preview(&vec![20; 600_000]).unwrap();
    let protected = privacy.preview_path(&content.preview_sha256).unwrap();
    let remaining = omarchy_photo_wallpapers::imaging::evict(
        &store,
        1_048_576,
        std::slice::from_ref(&protected),
    )
    .unwrap();
    assert!(remaining <= 1_048_576);
    assert!(protected.exists());
    assert!(
        !privacy.preview_path(&first).unwrap().exists()
            || !privacy.preview_path(&second).unwrap().exists(),
        "screening previews grew outside the configured disk budget"
    );
}

#[test]
fn a_substituted_cache_directory_never_deletes_files_outside_the_cache() {
    let (_root, store, _prepared, _content, _key) = fixture();
    let outside = tempfile::tempdir().unwrap();
    let photo = outside.path().join("synthetic-original.jpg");
    std::fs::write(&photo, b"keep-original").unwrap();
    std::os::unix::fs::symlink(outside.path(), store.paths.data.join("media-v2")).unwrap();
    assert!(omarchy_photo_wallpapers::imaging::evict(&store, 0, &[]).is_err());
    assert_eq!(std::fs::read(&photo).unwrap(), b"keep-original");
}
