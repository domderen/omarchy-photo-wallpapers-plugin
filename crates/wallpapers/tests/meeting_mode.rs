use omarchy_photo_wallpapers::storage::{Paths, Store};
use serde_json::json;

fn state() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(root.path())).unwrap();
    (root, store)
}
#[test]
fn meeting_request_is_durable_idempotent_and_preserves_personal_preferences() {
    let (_root, store) = state();
    let initial=store.update_settings(0,json!({"sources":[{"kind":"library","id":"all"}],"interval_seconds":900,"paused":true,"pinned":"personal-photo"})).unwrap();
    store
        .set_meta(
            "current-v2",
            &json!({"asset_id":"personal-photo","private_fixture":true}),
        )
        .unwrap();
    let enabled = store.set_meeting(true).unwrap();
    assert!(enabled.privacy.meeting);
    assert_eq!(enabled.interval_seconds, 900);
    assert!(enabled.paused);
    assert_eq!(enabled.pinned.as_deref(), Some("personal-photo"));
    assert_eq!(enabled.sources, initial.sources);
    assert_eq!(enabled.revision, initial.revision + 1);
    assert_eq!(store.meta("privacy-generation").unwrap(), 1);
    assert_eq!(
        store.meta("privacy-personal-context").unwrap()["current"]["asset_id"],
        "personal-photo"
    );
    assert_eq!(store.set_meeting(true).unwrap().revision, enabled.revision);
    assert_eq!(store.meta("privacy-generation").unwrap(), 1);
    assert!(
        Store::open(&store.paths)
            .unwrap()
            .settings()
            .unwrap()
            .privacy
            .meeting
    );
    let disabled = store.set_meeting(false).unwrap();
    assert!(!disabled.privacy.meeting);
    assert_eq!(disabled.pinned, initial.pinned);
    assert_eq!(disabled.revision, enabled.revision + 1);
    assert_eq!(store.meta("privacy-generation").unwrap(), 2);
    assert_eq!(
        store.set_meeting(false).unwrap().revision,
        disabled.revision
    );
}
#[test]
fn an_old_settings_draft_cannot_turn_off_a_new_meeting_request() {
    let (_root, store) = state();
    let old = store.settings().unwrap();
    let current = store.set_meeting(true).unwrap();
    assert!(
        store
            .update_settings(old.revision, json!({"interval_seconds":120}))
            .unwrap_err()
            .to_string()
            .contains("settings_conflict")
    );
    assert!(
        store
            .update_settings(current.revision, json!({"privacy":{"meeting":false}}))
            .is_err()
    );
    assert!(store.settings().unwrap().privacy.meeting);
}
#[test]
fn personal_filter_uses_save_cancel_revisions_and_cannot_overwrite_the_meeting_flag() {
    let (_root, store) = state();
    let initial = store.settings().unwrap();
    let saved = store
        .update_settings(
            initial.revision,
            json!({"privacy":{"filter_personal":true}}),
        )
        .unwrap();
    assert!(saved.privacy.filter_personal);
    assert!(!saved.privacy.meeting);
    assert_eq!(store.meta("privacy-generation").unwrap(), 1);
    let meeting = store.set_meeting(true).unwrap();
    let changed = store
        .update_settings(
            meeting.revision,
            json!({"privacy":{"filter_personal":false}}),
        )
        .unwrap();
    assert!(changed.privacy.meeting);
    assert!(!changed.privacy.filter_personal);
}
#[test]
fn neutral_fallback_is_offline_generated_and_not_an_arbitrary_history_image() {
    let (_root, store) = state();
    let fallback = omarchy_photo_wallpapers::engine::privacy::neutral(&store, 1, "dark").unwrap();
    assert_eq!(fallback.provenance, "meeting-neutral");
    assert!(fallback.render_path.is_file());
    let image = image::open(&fallback.render_path).unwrap().to_rgb8();
    let first = *image.get_pixel(0, 0);
    assert!(image.pixels().all(|p| *p == first));
    assert!(!store.paths.data.join("media-v2").exists());
    assert!(omarchy_photo_wallpapers::engine::privacy::is_neutral(
        &store, &fallback
    ));
    std::fs::write(&fallback.render_path, b"replacement").unwrap();
    assert!(!omarchy_photo_wallpapers::engine::privacy::is_neutral(
        &store, &fallback
    ));
}

#[derive(Default)]
struct Desktop {
    fail: bool,
    paths: std::sync::Mutex<Vec<std::path::PathBuf>>,
}
#[async_trait::async_trait]
impl omarchy_photo_wallpapers::platform::Platform for Desktop {
    async fn geometry(&self) -> anyhow::Result<(u32, u32)> {
        Ok((640, 400))
    }
    async fn output_names(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec!["screen-a".into(), "screen-b".into()])
    }
    async fn apply(&self, p: &omarchy_photo_wallpapers::imaging::Prepared) -> anyhow::Result<()> {
        if self.fail {
            anyhow::bail!("wallpaper_apply_failed")
        }
        self.paths.lock().unwrap().push(p.render_path.clone());
        Ok(())
    }
}
struct NoDownloads(std::sync::atomic::AtomicUsize);
#[async_trait::async_trait]
impl omarchy_photo_wallpapers::engine::Pipeline for NoDownloads {
    async fn prepare(
        &self,
        _: Store,
        _: omarchy_photo_wallpapers::model::Asset,
        _: omarchy_photo_wallpapers::imaging::PrepareOptions,
        _: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<omarchy_photo_wallpapers::imaging::Prepared> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        anyhow::bail!("fixture_no_downloads")
    }
}
async fn request(
    engine: &omarchy_photo_wallpapers::engine::EngineHandle,
    command: &str,
    operation: &str,
    input: serde_json::Value,
) -> serde_json::Value {
    engine
        .request(omarchy_photo_wallpapers::ipc::Request {
            command: command.into(),
            operation: operation.into(),
            input,
            ..Default::default()
        })
        .await
        .unwrap()
}
async fn wait(
    engine: &omarchy_photo_wallpapers::engine::EngineHandle,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    for _ in 0..300 {
        let status = engine.status();
        if predicate(&status) {
            return status;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("state deadline: {}", engine.status());
}
async fn confirm(engine: &omarchy_photo_wallpapers::engine::EngineHandle) -> serde_json::Value {
    let status = wait(engine, |s| s["privacy"]["transition_id"].is_string()).await;
    let p = &status["privacy"];
    for screen in ["screen-a", "screen-b"] {
        assert!(request(engine,"privacy","preloaded",json!({"generation":p["generation"],"transition_id":p["transition_id"],"output":screen})).await.get("error").is_none());
    }
    let status = wait(engine, |s| s["privacy"]["phase"] == "frame").await;
    assert_ne!(status["privacy"]["state"], "active");
    let p = &status["privacy"];
    request(engine,"privacy","frame",json!({"generation":p["generation"],"transition_id":p["transition_id"],"output":"screen-a"})).await;
    assert_ne!(engine.status()["privacy"]["state"], "active");
    request(engine,"privacy","frame",json!({"generation":p["generation"],"transition_id":p["transition_id"],"output":"screen-b"})).await;
    wait(engine, |s| s["privacy"]["state"] == "active").await
}
#[tokio::test]
async fn paused_pinned_entry_uses_neutral_and_requires_each_output_frame() {
    use omarchy_photo_wallpapers::engine::Engine;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (_root, store) = state();
    store
        .update_settings(
            0,
            json!({"paused":true,"pinned":"private-personal","interval_seconds":900}),
        )
        .unwrap();
    let platform = Arc::new(Desktop::default());
    let pipeline = Arc::new(NoDownloads(AtomicUsize::new(0)));
    let engine = Engine::start_with_theme_runtime(
        store.clone(),
        platform.clone(),
        pipeline.clone(),
        false,
        None,
    )
    .await
    .unwrap();
    assert!(
        request(&engine, "meeting", "on", serde_json::Value::Null)
            .await
            .get("error")
            .is_none()
    );
    let active = confirm(&engine).await;
    assert_eq!(active["current"]["provenance"], "meeting-neutral");
    assert_eq!(active["privacy"]["reason"], "no_sources");
    let settings = store.settings().unwrap();
    assert!(settings.paused && settings.privacy.meeting);
    assert_eq!(settings.pinned.as_deref(), Some("private-personal"));
    assert_eq!(settings.interval_seconds, 900);
    assert_eq!(pipeline.0.load(Ordering::SeqCst), 0);
    let calls = platform.paths.lock().unwrap().len();
    let revision = settings.revision;
    request(&engine, "meeting", "on", serde_json::Value::Null).await;
    assert_eq!(store.settings().unwrap().revision, revision);
    assert_eq!(platform.paths.lock().unwrap().len(), calls);
    engine.shutdown().await;
}
#[tokio::test]
async fn failed_replacement_and_old_ui_acknowledgements_never_report_active() {
    use omarchy_photo_wallpapers::engine::Engine;
    use std::sync::{Arc, atomic::AtomicUsize};
    let (_root, store) = state();
    let platform = Arc::new(Desktop {
        fail: true,
        ..Default::default()
    });
    let engine = Engine::start_with_theme_runtime(
        store.clone(),
        platform,
        Arc::new(NoDownloads(AtomicUsize::new(0))),
        false,
        None,
    )
    .await
    .unwrap();
    request(&engine, "meeting", "on", serde_json::Value::Null).await;
    let failed = wait(&engine, |s| s["privacy"]["state"] == "error").await;
    assert!(store.settings().unwrap().privacy.meeting);
    let p = &failed["privacy"];
    request(&engine,"privacy","frame",json!({"generation":p["generation"],"transition_id":p["transition_id"],"output":"screen-a"})).await;
    assert_ne!(engine.status()["privacy"]["state"], "active");
    request(&engine, "meeting", "off", serde_json::Value::Null).await;
    let reply=request(&engine,"privacy","frame",json!({"generation":p["generation"],"transition_id":p["transition_id"],"output":"screen-b"})).await;
    assert!(reply.get("error").is_some());
    assert!(!store.settings().unwrap().privacy.meeting);
    engine.shutdown().await;
}
#[tokio::test]
async fn requested_mode_rearms_after_restart_without_needing_accounts_or_models() {
    use omarchy_photo_wallpapers::engine::Engine;
    use std::sync::{Arc, atomic::AtomicUsize};
    let (_root, store) = state();
    store.set_meeting(true).unwrap();
    let before = store.meta("privacy-generation").unwrap().as_u64().unwrap();
    let engine = Engine::start_with_theme_runtime(
        store.clone(),
        Arc::new(Desktop::default()),
        Arc::new(NoDownloads(AtomicUsize::new(0))),
        false,
        None,
    )
    .await
    .unwrap();
    assert_ne!(engine.status()["privacy"]["state"], "active");
    let active = confirm(&engine).await;
    assert!(active["privacy"]["generation"].as_u64().unwrap() > before);
    assert_eq!(active["privacy"]["model_state"], "unavailable");
    engine.shutdown().await;
}

#[test]
fn protection_shadow_is_readable_at_shell_start_without_photo_or_account_data() {
    let (_root, store) = state();
    store.set_meeting(true).unwrap();
    let path = store.paths.data.join("privacy-mode.json");
    let text = std::fs::read_to_string(&path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["protected"], true);
    assert_eq!(value["meeting"], true);
    assert!(!text.contains("asset"));
    store.set_meeting(false).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(value["protected"], false);
}

#[tokio::test]
async fn models_are_optional_and_commands_never_start_a_background_scan() {
    use omarchy_photo_wallpapers::engine::Engine;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (_root, store) = state();
    let pipeline = Arc::new(NoDownloads(AtomicUsize::new(0)));
    let engine = Engine::start_with_theme_runtime(
        store,
        Arc::new(Desktop::default()),
        pipeline.clone(),
        false,
        None,
    )
    .await
    .unwrap();
    let status = request(&engine, "models", "status", serde_json::Value::Null).await;
    assert_eq!(status["state"], "unavailable");
    assert!(status["download_bytes"].as_u64().unwrap() > 250_000_000);
    let cancel = request(&engine, "models", "cancel", serde_json::Value::Null).await;
    assert!(cancel.get("error").is_none());
    assert_eq!(pipeline.0.load(Ordering::SeqCst), 0);
    assert!(!engine.status()["privacy"]["protected"].as_bool().unwrap());
    engine.shutdown().await;
}

#[path = "common/privacy.rs"]
mod fixtures;
#[tokio::test]
async fn safe_current_is_retained_without_inference_and_optional_corrections_rearm_immediately() {
    use omarchy_photo_wallpapers::{
        engine::Engine,
        privacy::store::{Decision, PrivacyStore},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (_root, store, prepared, content, key) = fixtures::fixture();
    let privacy = PrivacyStore::new(store.clone());
    privacy
        .bind_render(&prepared.render_digest, &content)
        .unwrap();
    privacy
        .save(&Decision::from_signals(
            content,
            key.clone(),
            fixtures::clear(),
        ))
        .unwrap();
    store.set_meta("privacy-policy-v1", &json!(key)).unwrap();
    store.set_meta("current-v2", &json!(prepared)).unwrap();
    let prefs = store
        .update_settings(1, json!({"paused":true,"pinned":"asset"}))
        .unwrap();
    let desktop = Arc::new(Desktop::default());
    let pipeline = Arc::new(NoDownloads(AtomicUsize::new(0)));
    let engine =
        Engine::start_with_theme_runtime(store.clone(), desktop, pipeline.clone(), false, None)
            .await
            .unwrap();
    request(&engine, "meeting", "on", serde_json::Value::Null).await;
    let active = confirm(&engine).await;
    assert_eq!(active["current"]["asset_id"], "asset");
    assert_eq!(active["privacy"]["counts"]["clear"], 1);
    assert_eq!(pipeline.0.load(Ordering::SeqCst), 0);
    let corrected = request(
        &engine,
        "feedback",
        "",
        json!({"action":"privacy","choice":"personal_only"}),
    )
    .await;
    assert!(corrected.get("error").is_none(), "{corrected}");
    let replaced = confirm(&engine).await;
    assert_eq!(replaced["current"]["provenance"], "meeting-neutral");
    request(&engine, "unpin", "", serde_json::Value::Null).await;
    assert_eq!(store.settings().unwrap().pinned, prefs.pinned);
    request(&engine, "meeting", "off", serde_json::Value::Null).await;
    wait(&engine, |s| {
        s["current"]["asset_id"] == "asset" && s["applying"] == false
    })
    .await;
    assert_eq!(store.settings().unwrap().pinned, prefs.pinned);
    assert!(store.settings().unwrap().paused);
    assert!(store.meta("privacy-personal-context").unwrap().is_null());
    engine.shutdown().await;
}

#[tokio::test]
async fn an_apply_dispatched_before_entry_is_drained_before_the_neutral_replacement() {
    use omarchy_photo_wallpapers::{engine::Engine, imaging::Prepared, platform::Platform};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Delayed {
        calls: AtomicUsize,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        paths: std::sync::Mutex<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl Platform for Delayed {
        async fn geometry(&self) -> anyhow::Result<(u32, u32)> {
            Ok((640, 400))
        }
        async fn output_names(&self) -> anyhow::Result<Vec<String>> {
            Ok(vec!["screen-a".into(), "screen-b".into()])
        }
        async fn apply(&self, p: &Prepared) -> anyhow::Result<()> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                self.release.notified().await;
            }
            self.paths.lock().unwrap().push(p.provenance.clone());
            Ok(())
        }
    }
    let (_root, store, prepared, _, _) = fixtures::fixture();
    store
        .set_meta("history-v2", &json!([prepared.clone(), prepared]))
        .unwrap();
    store.set_meta("history-index-v2", &json!(1)).unwrap();
    store.update_settings(1, json!({"paused":true})).unwrap();
    let desktop = Arc::new(Delayed {
        calls: AtomicUsize::new(0),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        paths: Default::default(),
    });
    let engine = Engine::start_with_theme_runtime(
        store,
        desktop.clone(),
        Arc::new(NoDownloads(AtomicUsize::new(0))),
        false,
        None,
    )
    .await
    .unwrap();
    request(&engine, "previous", "", serde_json::Value::Null).await;
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        desktop.entered.notified(),
    )
    .await
    .unwrap();
    request(&engine, "meeting", "on", serde_json::Value::Null).await;
    let p = engine.status()["privacy"].clone();
    for output in ["screen-a", "screen-b"] {
        request(&engine,"privacy","preloaded",json!({"generation":p["generation"],"transition_id":p["transition_id"],"output":output})).await;
    }
    assert_eq!(desktop.calls.load(Ordering::SeqCst), 1);
    assert_ne!(engine.status()["privacy"]["state"], "active");
    assert!(engine.status()["current"].is_null());
    desktop.release.notify_one();
    let active = confirm(&engine).await;
    assert_eq!(active["current"]["provenance"], "meeting-neutral");
    let calls = desktop.paths.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1], "meeting-neutral");
    engine.shutdown().await;
}

#[test]
fn neutral_appearance_follows_the_theme_even_without_photo_comfort_ranking() {
    let mut settings = omarchy_photo_wallpapers::model::Settings {
        match_colors: true,
        ..Default::default()
    };
    settings.theme.prefer_time_of_day = false;
    assert_eq!(
        omarchy_photo_wallpapers::engine::privacy::neutral_mode(
            &settings,
            &json!({"mode":"light"})
        ),
        "light"
    );
    settings.theme.mode = "dark".into();
    assert_eq!(
        omarchy_photo_wallpapers::engine::privacy::neutral_mode(
            &settings,
            &json!({"mode":"light"})
        ),
        "dark"
    );
}

#[tokio::test]
async fn a_cancelled_personal_preparation_cannot_refill_the_protected_queue() {
    use omarchy_photo_wallpapers::{
        engine::{Engine, Pipeline},
        imaging::{PrepareOptions, Prepared},
        model::Asset,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Delayed {
        calls: AtomicUsize,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        photo: Prepared,
    }
    #[async_trait::async_trait]
    impl Pipeline for Delayed {
        async fn prepare(
            &self,
            _: Store,
            _: Asset,
            _: PrepareOptions,
            _: tokio_util::sync::CancellationToken,
        ) -> anyhow::Result<Prepared> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                self.release.notified().await;
                Ok(self.photo.clone())
            } else {
                anyhow::bail!("privacy_filtered")
            }
        }
    }
    let (_root, store, photo, _, _) = fixtures::fixture();
    let desktop = Arc::new(Desktop::default());
    let pipeline = Arc::new(Delayed {
        calls: AtomicUsize::new(0),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        photo,
    });
    let engine =
        Engine::start_with_theme_runtime(store, desktop.clone(), pipeline.clone(), false, None)
            .await
            .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        pipeline.entered.notified(),
    )
    .await
    .unwrap();
    request(&engine, "meeting", "on", serde_json::Value::Null).await;
    confirm(&engine).await;
    pipeline.release.notify_one();
    let status = wait(&engine, |s| {
        pipeline.calls.load(Ordering::SeqCst) >= 2 && s["preparing"] == false
    })
    .await;
    assert_eq!(status["ready_count"], 0);
    assert_eq!(status["current"]["provenance"], "meeting-neutral");
    assert_eq!(status["metrics"]["analysis_jobs"], 0);
    assert!(
        desktop
            .paths
            .lock()
            .unwrap()
            .iter()
            .all(|p| p.to_string_lossy().contains("neutral-"))
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn remembered_policy_is_not_reported_as_installed_when_models_are_missing() {
    use omarchy_photo_wallpapers::engine::Engine;
    use std::sync::{Arc, atomic::AtomicUsize};
    let (_root, store, _, _, key) = fixtures::fixture();
    store.set_meta("privacy-policy-v1", &json!(key)).unwrap();
    let engine = Engine::start_with_theme_runtime(
        store,
        Arc::new(Desktop::default()),
        Arc::new(NoDownloads(AtomicUsize::new(0))),
        false,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        request(&engine, "models", "status", serde_json::Value::Null).await["state"],
        "unavailable"
    );
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        request(&engine, "models", "install", serde_json::Value::Null).await;
        let status = wait(&engine, |s| {
            s["privacy"]["models"]["error"] == "privacy_model_install_failed"
        })
        .await;
        assert_eq!(status["privacy"]["model_state"], "unavailable");
    }
    engine.shutdown().await;
}
