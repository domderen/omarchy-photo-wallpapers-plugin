use anyhow::Result;
use async_trait::async_trait;
use chrono::{Duration, Utc};
use omarchy_photo_wallpapers::{
    engine::{Engine, Pipeline, Schedule},
    imaging::{PrepareOptions, Prepared, Rect},
    ipc::{self, Request},
    model::{Asset, Source, SourceRef},
    platform::Platform,
    storage::{Paths, Store, atomic_bytes},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};
use tokio_util::sync::CancellationToken;
#[derive(Default)]
struct FakePlatform {
    applied: AtomicUsize,
}
#[async_trait]
impl Platform for FakePlatform {
    async fn geometry(&self) -> Result<(u32, u32)> {
        Ok((640, 400))
    }
    async fn apply(&self, _: &Prepared) -> Result<()> {
        self.applied.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct FakePipeline {
    calls: AtomicUsize,
    blocked: bool,
}
#[async_trait]
impl Pipeline for FakePipeline {
    async fn prepare(
        &self,
        store: Store,
        asset: Asset,
        options: PrepareOptions,
        stop: CancellationToken,
    ) -> Result<Prepared> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.blocked {
            stop.cancelled().await;
            anyhow::bail!("preparation_cancelled");
        }
        let render_path = store.paths.data.join(format!("{}.jpg", asset.id));
        atomic_bytes(&render_path, b"synthetic-ready-render")?;
        Ok(Prepared {
            asset_id: asset.id,
            revision: asset.revision,
            render_path,
            render_digest: {
                use sha2::{Digest, Sha256};
                hex::encode(Sha256::digest(b"synthetic-ready-render"))
            },
            config_key: omarchy_photo_wallpapers::imaging::config_key(&options)?,
            rect: Rect {
                x: 0,
                y: 0,
                width: 640,
                height: 400,
            },
            mode: "fit".into(),
            fingerprint: 0,
            score: 0.5,
            source_ids: Vec::new(),
            source_width: 640,
            source_height: 400,
            width: 640,
            height: 400,
            rendition: "edited".into(),
            provenance: "fixture".into(),
            applied_at: None,
            decode_count: 1,
            cache_hit: false,
            analysis_cache_hit: false,
        })
    }
}
fn setup() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(root.path())).unwrap();
    let assets: Vec<_> = (0..6)
        .map(|n| Asset {
            id: format!("asset-{n}"),
            revision: "r".into(),
            width: 640,
            height: 400,
            ..Asset::default()
        })
        .collect();
    store
        .activate_catalog(&assets, &[Source::library()], &[], "synthetic")
        .unwrap();
    store
        .update_settings(0, json!({"sources":[SourceRef::library()]}))
        .unwrap();
    (root, store)
}
fn request(command: &str) -> Request {
    serde_json::from_value(json!({"id":uuid::Uuid::new_v4().to_string(),"command":command}))
        .unwrap()
}
#[tokio::test]
async fn prefill_uses_two_suitable_cached_photos_and_keeps_one_exploration_slot() {
    let (_root, store) = setup();
    let pipeline = Arc::new(FakePipeline {
        calls: AtomicUsize::new(0),
        blocked: false,
    });
    let options = PrepareOptions {
        width: 640,
        height: 400,
        person_sensitive: false,
        fallback: "fit".into(),
        enhancements: Default::default(),
        preferred_crop: None,
    };
    for i in 0..6 {
        let asset = store.asset(&format!("asset-{i}")).unwrap().unwrap();
        let mut prepared = pipeline
            .prepare(
                store.clone(),
                asset,
                options.clone(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        prepared.render_digest = format!("digest-{i}");
        store
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO renders VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![
                    format!("key-{i}"),
                    prepared.asset_id,
                    prepared.revision,
                    prepared.config_key,
                    serde_json::to_string(&prepared).unwrap(),
                    1.
                ],
            )
            .unwrap();
        store
            .set_meta(
                &format!("suitability-v2:digest-{i}"),
                &json!({"day_score":if i<3{0.95}else{0.05}}),
            )
            .unwrap();
        if i == 5 {
            store
                .set_meta("current-v2", &serde_json::to_value(&prepared).unwrap())
                .unwrap();
        }
    }
    pipeline.calls.store(0, Ordering::SeqCst);
    store
        .update_settings(
            store.settings().unwrap().revision,
            json!({"match_colors":true,"theme":{"mode":"dark"}}),
        )
        .unwrap();
    let engine = Engine::start_with_theme_runtime(
        store.clone(),
        Arc::new(FakePlatform::default()),
        pipeline.clone(),
        false,
        None,
    )
    .await
    .unwrap();
    wait_status(&engine, |s| s["ready_count"] == 3).await;
    let ready: Vec<Prepared> = serde_json::from_value(store.meta("ready-v2").unwrap()).unwrap();
    assert_eq!(
        ready
            .iter()
            .filter(|p| ["asset-3", "asset-4"].contains(&p.asset_id.as_str()))
            .count(),
        2
    );
    assert_eq!(ready[0].asset_id, "asset-3");
    assert_eq!(ready[1].asset_id, "asset-4");
    assert_eq!(pipeline.calls.load(Ordering::SeqCst), 3);
    engine.shutdown().await;
}
#[tokio::test]
async fn theme_settings_preserve_ready_images_and_invalid_options_have_actionable_errors() {
    let (_root, store) = setup();
    let pipeline = Arc::new(FakePipeline {
        calls: AtomicUsize::new(0),
        blocked: false,
    });
    let engine = Engine::start(
        store.clone(),
        Arc::new(FakePlatform::default()),
        pipeline.clone(),
        false,
    )
    .await
    .unwrap();
    wait_status(&engine, |s| {
        s["ready_count"] == 3 && s["current"].is_object() && s["applying"] == false
    })
    .await;
    let ready = store.meta("ready-v2").unwrap();
    let calls = pipeline.calls.load(Ordering::SeqCst);
    let revision = store.settings().unwrap().revision;
    let updated = engine
        .request(Request {
            command: "config".into(),
            operation: "apply".into(),
            expected_revision: Some(revision),
            input: json!({"theme":{"mode":"dark"}}),
            ..request("config")
        })
        .await
        .unwrap();
    assert_eq!(updated["theme"]["mode"], "dark");
    assert_eq!(store.meta("ready-v2").unwrap(), ready);
    assert_eq!(pipeline.calls.load(Ordering::SeqCst), calls);
    assert_eq!(engine.status()["theme"]["state"], "disabled");
    let invalid = engine
        .request(Request {
            command: "config".into(),
            operation: "apply".into(),
            expected_revision: Some(revision + 1),
            input: json!({"theme":{"sunrise_offset_minutes":181}}),
            ..request("config")
        })
        .await
        .unwrap();
    assert_eq!(invalid["error"], "invalid_settings");
    engine.shutdown().await;
}
#[tokio::test]
async fn current_photo_period_feedback_is_dispatched_without_changing_eligibility() {
    let (_root, store) = setup();
    let engine = Engine::start(
        store.clone(),
        Arc::new(FakePlatform::default()),
        Arc::new(FakePipeline {
            calls: AtomicUsize::new(0),
            blocked: false,
        }),
        false,
    )
    .await
    .unwrap();
    wait_status(&engine, |s| {
        s["current"].is_object() && s["applying"] == false
    })
    .await;
    let selected = store.settings().unwrap().sources;
    let result = engine
        .request(Request {
            input: json!({"action":"period","period":"night"}),
            ..request("feedback")
        })
        .await
        .unwrap();
    assert_eq!(result["recorded"], true);
    wait_status(&engine, |s| s["current"]["period"] == "night").await;
    assert_eq!(store.settings().unwrap().sources, selected);
    engine.shutdown().await;
}
#[tokio::test]
async fn manual_light_and_dark_preferences_choose_the_matching_ready_photo_without_a_generator() {
    for (mode, preferred, other) in [("light", "day", "night"), ("dark", "night", "day")] {
        let (_root, store) = setup();
        let engine = Engine::start_with_theme_runtime(
            store.clone(),
            Arc::new(FakePlatform::default()),
            Arc::new(FakePipeline {
                calls: AtomicUsize::new(0),
                blocked: false,
            }),
            false,
            None,
        )
        .await
        .unwrap();
        wait_status(&engine, |s| {
            s["ready_count"] == 3 && s["current"].is_object() && s["applying"] == false
        })
        .await;
        engine.request(request("pause")).await.unwrap();
        let ready: Vec<Prepared> = serde_json::from_value(store.meta("ready-v2").unwrap()).unwrap();
        let selected = ready[0].asset_id.clone();
        for (i, p) in ready.iter().enumerate() {
            store
                .feedback(
                    &p.asset_id,
                    "period",
                    Some(json!({"period":if i==0{preferred}else{other}})),
                )
                .unwrap();
        }
        let revision = store.settings().unwrap().revision;
        engine
            .request(Request {
                command: "config".into(),
                operation: "apply".into(),
                expected_revision: Some(revision),
                input: json!({"match_colors":true,"theme":{"mode":mode}}),
                ..request("config")
            })
            .await
            .unwrap();
        engine.request(request("next")).await.unwrap();
        wait_status(&engine, |s| {
            s["current"]["asset_id"] == selected && s["applying"] == false
        })
        .await;
        engine.shutdown().await;
    }
}
#[test]
fn persisted_utc_deadline_collapses_overdue_intervals() {
    let now = Utc::now();
    let mut schedule = Schedule {
        next_due: Some(now - Duration::hours(8)),
    };
    assert!(schedule.due(now, false, false));
    assert!(!schedule.due(now, true, false));
    assert!(!schedule.due(now, false, true));
    schedule.advance(now, 3600);
    assert_eq!(schedule.next_due, Some(now + Duration::hours(1)));
    assert!(!schedule.due(now, false, false));
}
#[test]
fn client_retry_after_completion_does_not_create_another_job() {
    let (_root, store) = setup();
    let first = store.enqueue_once("next", "request-1").unwrap();
    store.claim_command().unwrap();
    let duplicate = store.enqueue_once("next", "request-2").unwrap();
    assert_eq!(duplicate, first);
    store.finish_command(&first, json!({"ok":true})).unwrap();
    assert_eq!(store.enqueue_once("next", "request-2").unwrap(), first);
    assert!(store.enqueue_once("refresh", "request-2").is_err());
}
#[tokio::test]
async fn pause_is_responsive_during_blocked_preparation_and_stale_work_never_applies() {
    let (_root, store) = setup();
    let platform = Arc::new(FakePlatform::default());
    let pipeline = Arc::new(FakePipeline {
        calls: AtomicUsize::new(0),
        blocked: true,
    });
    let engine = Engine::start(store, platform.clone(), pipeline.clone(), false)
        .await
        .unwrap();
    engine.request(request("next")).await.unwrap();
    for _ in 0..50 {
        if pipeline.calls.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(pipeline.calls.load(Ordering::SeqCst) > 0);
    let start = Instant::now();
    let paused = engine.request(request("pause")).await.unwrap();
    assert!(start.elapsed().as_millis() < 200);
    assert_eq!(paused["paused"], true);
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(platform.applied.load(Ordering::SeqCst), 0);
    assert_eq!(engine.status()["preparing"], false);
    engine.shutdown().await;
}
#[tokio::test]
async fn native_socket_subscriptions_revisions_and_malformed_input() {
    let (_root, store) = setup();
    let paths = store.paths.clone();
    let engine = Engine::start(
        store,
        Arc::new(FakePlatform::default()),
        Arc::new(FakePipeline {
            calls: AtomicUsize::new(0),
            blocked: true,
        }),
        false,
    )
    .await
    .unwrap();
    assert_eq!(engine.status()["protocol_version"], 1);
    assert_eq!(
        engine.status()["backend_version"],
        env!("CARGO_PKG_VERSION")
    );
    let stop = CancellationToken::new();
    let listener = ipc::bind(&paths).unwrap();
    let task = tokio::spawn(ipc::serve(listener, engine.clone(), stop.clone()));
    let config = ipc::call(
        &paths,
        serde_json::from_value(json!({"id":"settings","command":"config","operation":"get"}))
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(config["interval_seconds"], 3600);
    let result=ipc::call(&paths,serde_json::from_value(json!({"id":"save","command":"config","operation":"apply","expected_revision":1,"input":{"interval_seconds":900}})).unwrap()).await.unwrap();
    assert_eq!(result["interval_seconds"], 900);
    let stale=ipc::call(&paths,serde_json::from_value(json!({"id":"stale","command":"config","operation":"apply","expected_revision":1,"input":{"interval_seconds":60}})).unwrap()).await.unwrap();
    assert_eq!(stale["error"], "settings_conflict");
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = tokio::net::UnixStream::connect(paths.socket())
        .await
        .unwrap();
    let (mut read, mut write) = stream.into_split();
    write
        .write_all(b"{\"id\":\"watch\",\"command\":\"subscribe\"}\n")
        .await
        .unwrap();
    let mut lines = BufReader::new(&mut read).lines();
    let initial: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(initial["id"], "watch");
    engine.request(request("pause")).await.unwrap();
    let update: Value = serde_json::from_str(
        &tokio::time::timeout(std::time::Duration::from_secs(1), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(update["event"], "status");
    assert_eq!(update["status"]["paused"], true);
    let stream = tokio::net::UnixStream::connect(paths.socket())
        .await
        .unwrap();
    let (mut read, mut write) = stream.into_split();
    write.write_all(b"invalid-json\n").await.unwrap();
    let mut response = String::new();
    BufReader::new(&mut read)
        .read_line(&mut response)
        .await
        .unwrap();
    assert!(response.contains("invalid_request"));
    stop.cancel();
    task.await.unwrap().unwrap();
    engine.shutdown().await;
}

async fn wait_status(
    engine: &omarchy_photo_wallpapers::engine::EngineHandle,
    predicate: impl Fn(&Value) -> bool,
) {
    for _ in 0..200 {
        if predicate(&engine.status()) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!(
        "engine did not reach expected synthetic state: {}",
        engine.status()
    );
}
#[tokio::test]
async fn warm_next_previous_and_restart_use_ready_renders_without_preparation() {
    let (_root, store) = setup();
    let platform = Arc::new(FakePlatform::default());
    let pipeline = Arc::new(FakePipeline {
        calls: AtomicUsize::new(0),
        blocked: false,
    });
    let engine = Engine::start(store.clone(), platform.clone(), pipeline.clone(), false)
        .await
        .unwrap();
    wait_status(&engine, |s| {
        s["ready_count"] == 3 && s["current"].is_object() && s["applying"] == false
    })
    .await;
    engine.request(request("pause")).await.unwrap();
    let calls = pipeline.calls.load(Ordering::SeqCst);
    let first = engine.status()["current"]["asset_id"].clone();
    engine.request(request("pin")).await.unwrap();
    engine.request(request("next")).await.unwrap();
    assert!(
        engine.status()["pinned"].is_null(),
        "manual Next must unpin"
    );
    wait_status(&engine, |s| {
        s["current"]["asset_id"] != first && s["applying"] == false
    })
    .await;
    assert_eq!(calls, pipeline.calls.load(Ordering::SeqCst));
    engine.request(request("previous")).await.unwrap();
    wait_status(&engine, |s| {
        s["current"]["asset_id"] == first && s["applying"] == false
    })
    .await;
    assert_eq!(calls, pipeline.calls.load(Ordering::SeqCst));
    engine.shutdown().await;
    let restarted = Engine::start(store, platform, pipeline.clone(), false)
        .await
        .unwrap();
    assert_eq!(restarted.status()["paused"], true);
    assert!(restarted.status()["ready_count"].as_u64().unwrap() > 0);
    restarted.request(request("next")).await.unwrap();
    wait_status(&restarted, |s| {
        s["current"]["asset_id"] != first && s["applying"] == false
    })
    .await;
    assert_eq!(calls, pipeline.calls.load(Ordering::SeqCst));
    restarted.shutdown().await;
}
struct LatePipeline {
    release: Arc<tokio::sync::Notify>,
    started: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl Pipeline for LatePipeline {
    async fn prepare(
        &self,
        store: Store,
        asset: Asset,
        options: PrepareOptions,
        _stop: CancellationToken,
    ) -> Result<Prepared> {
        self.started.notify_one();
        self.release.notified().await;
        FakePipeline {
            calls: AtomicUsize::new(0),
            blocked: false,
        }
        .prepare(store, asset, options, CancellationToken::new())
        .await
    }
}
#[tokio::test]
async fn changed_sources_discard_late_success_even_from_noncooperative_worker() {
    let (_root, store) = setup();
    let platform = Arc::new(FakePlatform::default());
    let release = Arc::new(tokio::sync::Notify::new());
    let started = Arc::new(tokio::sync::Notify::new());
    let engine = Engine::start(
        store,
        platform.clone(),
        Arc::new(LatePipeline {
            release: release.clone(),
            started: started.clone(),
        }),
        false,
    )
    .await
    .unwrap();
    engine.request(request("next")).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
        .await
        .unwrap();
    let change:Request=serde_json::from_value(json!({"id":"change-sources","command":"config","operation":"apply","expected_revision":1,"input":{"sources":[]}})).unwrap();
    engine.request(change).await.unwrap();
    release.notify_one();
    wait_status(&engine, |s| s["preparing"] == false).await;
    assert_eq!(platform.applied.load(Ordering::SeqCst), 0);
    assert_eq!(engine.status()["ready_count"], 0);
    engine.shutdown().await;
}
#[tokio::test]
async fn interval_change_sets_and_persists_a_new_utc_deadline() {
    let (_root, store) = setup();
    let engine = Engine::start(
        store.clone(),
        Arc::new(FakePlatform::default()),
        Arc::new(FakePipeline {
            calls: AtomicUsize::new(0),
            blocked: true,
        }),
        false,
    )
    .await
    .unwrap();
    let change:Request=serde_json::from_value(json!({"id":"change-interval","command":"config","operation":"apply","expected_revision":1,"input":{"interval_seconds":900}})).unwrap();
    engine.request(change).await.unwrap();
    let deadline: chrono::DateTime<Utc> =
        serde_json::from_value(engine.status()["next_due"].clone()).unwrap();
    assert!((deadline - Utc::now()).num_seconds() >= 898);
    assert!((deadline - Utc::now()).num_seconds() <= 900);
    assert_eq!(
        store.meta("schedule-v2").unwrap()["next_due"],
        engine.status()["next_due"]
    );
    engine.shutdown().await;
}
struct FailingPipeline {
    calls: AtomicUsize,
}
#[async_trait]
impl Pipeline for FailingPipeline {
    async fn prepare(
        &self,
        _store: Store,
        _asset: Asset,
        _options: PrepareOptions,
        _stop: CancellationToken,
    ) -> Result<Prepared> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("apple_network_error")
    }
}
#[tokio::test]
async fn offline_preparation_backs_off_instead_of_walking_the_library() {
    let (_root, store) = setup();
    let pipeline = Arc::new(FailingPipeline {
        calls: AtomicUsize::new(0),
    });
    let engine = Engine::start(
        store,
        Arc::new(FakePlatform::default()),
        pipeline.clone(),
        false,
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    assert_eq!(pipeline.calls.load(Ordering::SeqCst), 1);
    assert!(engine.status()["preparation_retry_at"].is_string());
    engine.shutdown().await;
}

#[tokio::test]
async fn incomplete_socket_request_expires_without_echoing_sensitive_input() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (_root, store) = setup();
    let paths = store.paths.clone();
    let engine = Engine::start(
        store,
        Arc::new(FakePlatform::default()),
        Arc::new(FakePipeline {
            calls: AtomicUsize::new(0),
            blocked: true,
        }),
        false,
    )
    .await
    .unwrap();
    let stop = CancellationToken::new();
    let task = tokio::spawn(ipc::serve(
        ipc::bind(&paths).unwrap(),
        engine.clone(),
        stop.clone(),
    ));
    let mut stream = tokio::net::UnixStream::connect(paths.socket())
        .await
        .unwrap();
    stream
        .write_all(b"{\"input\":\"synthetic-sensitive-fragment")
        .await
        .unwrap();
    let mut output = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(7),
        stream.read_to_string(&mut output),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(output.contains("request_timeout"));
    assert!(!output.contains("synthetic-sensitive-fragment"));
    assert!(output.len() < 100);
    assert_eq!(
        ipc::call(&paths, request("status")).await.unwrap()["daemon_running"],
        true
    );
    stop.cancel();
    task.await.unwrap().unwrap();
    engine.shutdown().await;
}

#[tokio::test]
async fn fragmented_socket_request_survives_an_interleaved_status_update() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (_root, store) = setup();
    let paths = store.paths.clone();
    let engine = Engine::start(
        store,
        Arc::new(FakePlatform::default()),
        Arc::new(FakePipeline {
            calls: AtomicUsize::new(0),
            blocked: true,
        }),
        false,
    )
    .await
    .unwrap();
    let stop = CancellationToken::new();
    let task = tokio::spawn(ipc::serve(
        ipc::bind(&paths).unwrap(),
        engine.clone(),
        stop.clone(),
    ));
    let stream = tokio::net::UnixStream::connect(paths.socket())
        .await
        .unwrap();
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    write
        .write_all(b"{\"id\":\"subscribe\",\"command\":\"subscribe\"}\n")
        .await
        .unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    write
        .write_all(b"{\"id\":\"fragmented\",\"command\":")
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    engine.request(request("pause")).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            let event: Value = serde_json::from_str(&line).unwrap();
            if event["event"] == "status" && event["status"]["paused"] == true {
                break;
            }
        }
    })
    .await
    .unwrap();
    write.write_all(b"\"status\"}\n").await.unwrap();
    let reply = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            let result: Value = serde_json::from_str(&line).unwrap();
            if result.get("result").is_some() {
                break result;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(reply["id"], "fragmented");
    assert_eq!(reply["result"]["daemon_running"], true);
    stop.cancel();
    task.await.unwrap().unwrap();
    engine.shutdown().await;
}

#[tokio::test]
async fn oversized_socket_frames_are_rejected_without_echoing_the_body() {
    let (_root, store) = setup();
    let paths = store.paths.clone();
    let engine = Engine::start(
        store,
        Arc::new(FakePlatform::default()),
        Arc::new(FakePipeline {
            calls: AtomicUsize::new(0),
            blocked: true,
        }),
        false,
    )
    .await
    .unwrap();
    let stop = CancellationToken::new();
    let listener = ipc::bind(&paths).unwrap();
    let task = tokio::spawn(ipc::serve(listener, engine.clone(), stop.clone()));
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut stream = tokio::net::UnixStream::connect(paths.socket())
        .await
        .unwrap();
    stream.write_all(&vec![b'x'; 65537]).await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        BufReader::new(stream).read_line(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(response.contains("request_too_large"));
    assert!(response.len() < 100);
    assert_eq!(
        ipc::call(&paths, request("status")).await.unwrap()["daemon_running"],
        true
    );
    stop.cancel();
    task.await.unwrap().unwrap();
    engine.shutdown().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_realtime_deadline_wakes_without_a_polling_process() {
    let start = Instant::now();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        omarchy_photo_wallpapers::platform::deadline(Some(Utc::now() + Duration::milliseconds(30))),
    )
    .await
    .unwrap();
    assert!(start.elapsed().as_millis() >= 20);
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(20),
            omarchy_photo_wallpapers::platform::deadline(None)
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn saved_session_symlink_is_rejected_without_quarantining_or_chmodding_its_target() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (root, store) = setup();
    let unrelated = root.path().join("unrelated.json");
    std::fs::write(&unrelated, b"synthetic-unrelated-value").unwrap();
    std::fs::set_permissions(&unrelated, std::fs::Permissions::from_mode(0o644)).unwrap();
    let session = store.paths.config.join("apple-session.json");
    symlink(&unrelated, &session).unwrap();
    let engine = Engine::start(
        store.clone(),
        Arc::new(FakePlatform::default()),
        Arc::new(FakePipeline {
            calls: AtomicUsize::new(0),
            blocked: true,
        }),
        true,
    )
    .await
    .unwrap();
    wait_status(&engine, |s| {
        s["auth"]["error"] == "invalid_saved_apple_session"
    })
    .await;
    assert!(
        std::fs::symlink_metadata(&session)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read(&unrelated).unwrap(),
        b"synthetic-unrelated-value"
    );
    assert_eq!(
        std::fs::metadata(&unrelated).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert!(
        !engine
            .status()
            .to_string()
            .contains("synthetic-unrelated-value")
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn a_corrupt_saved_session_is_quarantined_and_native_reset_remains_usable() {
    let (_root, store) = setup();
    atomic_bytes(
        &store.paths.config.join("apple-session.json"),
        b"malformed synthetic-private-cookie",
    )
    .unwrap();
    let engine = Engine::start(
        store.clone(),
        Arc::new(FakePlatform::default()),
        Arc::new(FakePipeline {
            calls: AtomicUsize::new(0),
            blocked: true,
        }),
        true,
    )
    .await
    .unwrap();
    wait_status(&engine, |s| {
        s["auth"]["error"] == "invalid_saved_apple_session"
    })
    .await;
    assert!(
        !engine
            .status()
            .to_string()
            .contains("synthetic-private-cookie")
    );
    assert_eq!(
        std::fs::read_dir(store.paths.data.join("session-backups"))
            .unwrap()
            .count(),
        1
    );
    let reset: Request =
        serde_json::from_value(json!({"id":"reset","command":"auth","operation":"logout"}))
            .unwrap();
    assert_eq!(engine.request(reset).await.unwrap()["accepted"], true);
    wait_status(&engine, |s| {
        s["auth"]["state"] == "not_signed_in" && s["auth_busy"] == false
    })
    .await;
    engine.shutdown().await;
}
#[derive(Default)]
struct FailingApply {
    fail: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl Platform for FailingApply {
    async fn geometry(&self) -> Result<(u32, u32)> {
        Ok((640, 400))
    }
    async fn apply(&self, _: &Prepared) -> Result<()> {
        anyhow::ensure!(!self.fail.load(Ordering::SeqCst), "wallpaper_apply_failed");
        Ok(())
    }
}
#[tokio::test]
async fn failed_previous_keeps_current_and_history_position_and_can_be_retried() {
    let (_root, store) = setup();
    let platform = Arc::new(FailingApply::default());
    let pipeline = Arc::new(FakePipeline {
        calls: AtomicUsize::new(0),
        blocked: false,
    });
    let engine = Engine::start(store.clone(), platform.clone(), pipeline, false)
        .await
        .unwrap();
    wait_status(&engine, |s| {
        s["ready_count"] == 3 && s["current"].is_object() && s["applying"] == false
    })
    .await;
    engine.request(request("pause")).await.unwrap();
    let first = engine.status()["current"]["asset_id"].clone();
    engine.request(request("next")).await.unwrap();
    wait_status(&engine, |s| {
        s["current"]["asset_id"] != first && s["applying"] == false
    })
    .await;
    let second = engine.status()["current"]["asset_id"].clone();
    let index = store.meta("history-index-v2").unwrap();
    platform.fail.store(true, Ordering::SeqCst);
    engine.request(request("previous")).await.unwrap();
    wait_status(&engine, |s| {
        s["error"] == "wallpaper_apply_failed" && s["applying"] == false
    })
    .await;
    assert_eq!(engine.status()["current"]["asset_id"], second);
    assert_eq!(store.meta("history-index-v2").unwrap(), index);
    platform.fail.store(false, Ordering::SeqCst);
    engine.request(request("previous")).await.unwrap();
    wait_status(&engine, |s| {
        s["current"]["asset_id"] == first && s["applying"] == false
    })
    .await;
    engine.shutdown().await;
}
#[tokio::test]
async fn never_feedback_uses_another_ready_render_without_starting_new_analysis() {
    let (_root, store) = setup();
    let platform = Arc::new(FakePlatform::default());
    let pipeline = Arc::new(FakePipeline {
        calls: AtomicUsize::new(0),
        blocked: false,
    });
    let engine = Engine::start(store, platform, pipeline.clone(), false)
        .await
        .unwrap();
    wait_status(&engine, |s| {
        s["ready_count"] == 3 && s["current"].is_object() && s["applying"] == false
    })
    .await;
    engine.request(request("pause")).await.unwrap();
    let before = pipeline.calls.load(Ordering::SeqCst);
    let current = engine.status()["current"]["asset_id"].clone();
    let mut never = request("feedback");
    never.input = json!({"action":"never"});
    engine.request(never).await.unwrap();
    wait_status(&engine, |s| {
        s["current"]["asset_id"] != current && s["applying"] == false
    })
    .await;
    assert_eq!(pipeline.calls.load(Ordering::SeqCst), before);
    assert_eq!(engine.status()["eligible_count"], 5);
    engine.shutdown().await;
}
#[tokio::test]
async fn native_pipeline_rotates_cached_renders_repeatedly_without_network_or_original_files() {
    use omarchy_photo_wallpapers::engine::NativePipeline;
    let (root, store) = setup();
    for n in 0..4 {
        let path = root.path().join(format!("offline-{n}.png"));
        image::RgbImage::from_pixel(800, 600, image::Rgb([40 + n * 30, 100, 140]))
            .save(&path)
            .unwrap();
        let asset = store.asset(&format!("asset-{n}")).unwrap().unwrap();
        omarchy_photo_wallpapers::imaging::prepare(
            &store,
            &asset,
            &path,
            &PrepareOptions {
                width: 640,
                height: 400,
                person_sensitive: false,
                fallback: "fit".into(),
                enhancements: Default::default(),
                preferred_crop: None,
            },
            &CancellationToken::new(),
        )
        .unwrap();
        std::fs::remove_file(path).unwrap();
    }
    let engine = Engine::start(
        store,
        Arc::new(FakePlatform::default()),
        Arc::new(NativePipeline::default()),
        false,
    )
    .await
    .unwrap();
    wait_status(&engine, |s| {
        s["current"].is_object() && s["ready_count"] == 3 && !s["applying"].as_bool().unwrap()
    })
    .await;
    engine.request(request("pause")).await.unwrap();
    for _ in 0..8 {
        let current = engine.status()["current"]["asset_id"].clone();
        engine.request(request("next")).await.unwrap();
        wait_status(&engine, |s| {
            s["current"]["asset_id"] != current && s["applying"] == false
        })
        .await;
    }
    assert_eq!(engine.status()["metrics"]["analysis_jobs"], 0);
    assert!(
        engine.status()["metrics"]["render_cache_hits"]
            .as_u64()
            .unwrap()
            >= 8
    );
    engine.shutdown().await;
}
#[tokio::test]
async fn active_warm_change_defers_refill_instead_of_starting_analysis_beside_apply() {
    let (_root, store) = setup();
    let platform = Arc::new(FakePlatform::default());
    let pipeline = Arc::new(FakePipeline {
        calls: AtomicUsize::new(0),
        blocked: false,
    });
    let initial = Engine::start(store.clone(), platform.clone(), pipeline.clone(), false)
        .await
        .unwrap();
    wait_status(&initial, |s| {
        s["ready_count"] == 3 && s["current"].is_object() && s["applying"] == false
    })
    .await;
    initial.shutdown().await;
    let engine = Engine::start(store, platform, pipeline.clone(), true)
        .await
        .unwrap();
    let calls = pipeline.calls.load(Ordering::SeqCst);
    let current = engine.status()["current"]["asset_id"].clone();
    engine.request(request("next")).await.unwrap();
    wait_status(&engine, |s| {
        s["current"]["asset_id"] != current && s["applying"] == false
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(75)).await;
    assert_eq!(
        calls,
        pipeline.calls.load(Ordering::SeqCst),
        "refill must wait for the quiet interval"
    );
    engine.shutdown().await;
}
#[tokio::test]
async fn an_empty_source_selection_finishes_next_with_an_error_and_retains_the_wallpaper() {
    let (_root, store) = setup();
    let engine = Engine::start(
        store.clone(),
        Arc::new(FakePlatform::default()),
        Arc::new(FakePipeline {
            calls: AtomicUsize::new(0),
            blocked: false,
        }),
        false,
    )
    .await
    .unwrap();
    wait_status(&engine, |s| {
        s["current"].is_object() && s["ready_count"] == 3 && s["applying"] == false
    })
    .await;
    engine.request(request("pause")).await.unwrap();
    let current = engine.status()["current"].clone();
    let mut change = request("config");
    change.operation = "apply".into();
    change.expected_revision = engine.status()["settings_revision"].as_u64();
    change.input = json!({"sources":[]});
    engine.request(change).await.unwrap();
    engine.request(request("next")).await.unwrap();
    wait_status(&engine, |s| {
        s["error"] == "no_eligible_photos" && s["pending_actions"] == false
    })
    .await;
    assert_eq!(engine.status()["current"], current);
    assert_eq!(store.pending_actions().unwrap(), 0);
    engine.shutdown().await;
}
