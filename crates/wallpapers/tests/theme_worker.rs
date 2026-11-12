use async_trait::async_trait;
use chrono::{DateTime, Utc};
use omarchy_photo_wallpapers::{
    storage::{Paths, Store},
    themes::{
        palette::Generator,
        platform::Runner,
        worker::{self, Clock, Runtime},
    },
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
struct Time(Mutex<DateTime<Utc>>);
impl Clock for Time {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}
struct PastClock(std::sync::atomic::AtomicUsize);
impl Clock for PastClock {
    fn now(&self) -> DateTime<Utc> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        "2020-03-20T12:00:00Z".parse().unwrap()
    }
}
#[tokio::test]
async fn a_controlled_past_clock_does_not_spin_between_events() {
    let (_root, store, mut runtime, _) = setup();
    store
        .update_settings(1, json!({"match_colors":false}))
        .unwrap();
    let clock = Arc::new(PastClock(std::sync::atomic::AtomicUsize::new(0)));
    runtime.clock = clock.clone();
    let handle = worker::start(store, runtime, CancellationToken::new());
    tokio::time::sleep(Duration::from_millis(120)).await;
    handle.shutdown().await;
    assert!(
        clock.0.load(std::sync::atomic::Ordering::Relaxed) < 10,
        "Controlled clocks must sleep relative to their own time"
    );
}
struct Desktop(PathBuf);
#[async_trait]
impl Runner for Desktop {
    async fn apply(&self, name: &str) -> anyhow::Result<()> {
        let current = self.0.join(".local/state/omarchy/current");
        std::fs::write(current.join("theme.name"), name)?;
        std::fs::copy(
            self.0
                .join(".config/omarchy/themes")
                .join(name)
                .join("colors.toml"),
            current.join("theme/colors.toml"),
        )?;
        Ok(())
    }
}
fn fixture() -> Value {
    let roles = [
        ("background", "#fff8f5", "#171310"),
        ("on_background", "#201a17", "#ece0d8"),
        ("surface_container_lowest", "#ffffff", "#100d0a"),
        ("surface_container_high", "#eaded5", "#312923"),
        ("on_surface_variant", "#50443b", "#d5c3b5"),
        ("primary", "#885020", "#ffb77a"),
        ("primary_container", "#ffdcc0", "#6b390b"),
        ("on_primary_container", "#301500", "#ffdcc0"),
        ("secondary", "#745943", "#e3bfa4"),
        ("tertiary", "#5b6234", "#c3cd8d"),
        ("error", "#ba1a1a", "#ffb4ab"),
    ];
    json!({"colors":roles.into_iter().map(|(k,l,d)|(k.to_string(),json!({"light":{"color":l},"dark":{"color":d}}))).collect::<serde_json::Map<String,Value>>()})
}
fn setup() -> (tempfile::TempDir, Store, Runtime, Arc<Time>) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let current = home.join(".local/state/omarchy/current/theme");
    std::fs::create_dir_all(&current).unwrap();
    let base = home.join(".config/omarchy/themes/tokyo-night");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("colors.toml"), "background=\"#111111\"\n").unwrap();
    std::fs::copy(base.join("colors.toml"), current.join("colors.toml")).unwrap();
    std::fs::write(current.parent().unwrap().join("theme.name"), "tokyo-night").unwrap();
    let store = Store::open(&Paths::under(&root.path().join("app"))).unwrap();
    let render = store.paths.data.join("render.png");
    image::RgbImage::from_pixel(100, 70, image::Rgb([100, 120, 60]))
        .save(&render)
        .unwrap();
    store
        .set_meta(
            "current-v2",
            &json!({"asset_id":"one","render_path":render,"render_digest":"fixture-render"}),
        )
        .unwrap();
    store.update_settings(0,json!({"match_colors":true,"paused":true,"pinned":"one","theme":{"mode":"auto","location_mode":"fixed","location":{"name":"Fixture","latitude":51.48,"longitude":0.0}}})).unwrap();
    std::fs::write(root.path().join("fixture.json"), fixture().to_string()).unwrap();
    let exe = root.path().join("generator");
    std::fs::write(&exe, "#!/bin/sh\ncat \"$(dirname \"$0\")/fixture.json\"\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
    let time = Arc::new(Time(Mutex::new("2030-03-20T12:00:00Z".parse().unwrap())));
    let runtime = Runtime {
        home: home.clone(),
        runner: Arc::new(Desktop(home)),
        generator: Some(Generator::for_program(
            exe,
            "matugen 4.2.0".into(),
            Duration::from_secs(2),
        )),
        clock: time.clone(),
        network: false,
    };
    (root, store, runtime, time)
}
async fn wait(handle: &worker::Handle, predicate: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            let value = handle.status();
            if predicate(&value) {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn solar_mode_changes_while_paused_and_pinned_using_cached_variants_only() {
    let (root, store, runtime, time) = setup();
    let home = runtime.home.clone();
    let handle = worker::start(store.clone(), runtime, CancellationToken::new());
    let first = wait(&handle, |v| v["state"] == "active" && v["mode"] == "light").await;
    assert_eq!(first["generation_jobs"], 1);
    assert!(
        !first.to_string().contains("\"latitude\"") && !first.to_string().contains("\"longitude\""),
        "Status must not expose private coordinates"
    );
    std::fs::remove_file(root.path().join("generator")).unwrap();
    std::fs::remove_file(store.paths.data.join("render.png")).unwrap();
    *time.0.lock().unwrap() = "2030-03-20T23:00:00Z".parse().unwrap();
    handle.notify();
    let dark = wait(&handle, |v| v["state"] == "active" && v["mode"] == "dark").await;
    assert_eq!(dark["generation_jobs"], 1);
    assert!(dark["cache_hits"].as_u64().unwrap() >= 1);
    assert!(store.settings().unwrap().paused && store.settings().unwrap().pinned.is_some());
    store
        .update_settings(1, json!({"match_colors":false}))
        .unwrap();
    handle.notify();
    wait(&handle, |v| v["state"] == "disabled").await;
    assert_eq!(
        std::fs::read_to_string(home.join(".local/state/omarchy/current/theme.name")).unwrap(),
        "tokyo-night"
    );
    handle.shutdown().await;
}
#[tokio::test]
async fn disabling_during_generation_discards_obsolete_result_and_keeps_photo_state() {
    let (root, store, runtime, _) = setup();
    let home = runtime.home.clone();
    let exe = root.path().join("generator");
    std::fs::write(&exe, "#!/bin/sh\nexec sleep 10\n").unwrap();
    let current = store.meta("current-v2").unwrap();
    let handle = worker::start(store.clone(), runtime, CancellationToken::new());
    wait(&handle, |v| v["state"] == "preparing").await;
    store
        .update_settings(1, json!({"match_colors":false}))
        .unwrap();
    handle.notify();
    wait(&handle, |v| v["state"] == "disabled").await;
    assert_eq!(store.meta("current-v2").unwrap(), current);
    assert_eq!(
        std::fs::read_to_string(home.join(".local/state/omarchy/current/theme.name")).unwrap(),
        "tokyo-night"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn warm_style_changes_keep_the_photo_and_do_not_generate_again() {
    let (root, store, runtime, _) = setup();
    let current = store.meta("current-v2").unwrap();
    let handle = worker::start(store.clone(), runtime, CancellationToken::new());
    let first = wait(&handle, |v| v["state"] == "active").await;
    std::fs::remove_file(root.path().join("generator")).unwrap();
    std::fs::remove_file(store.paths.data.join("render.png")).unwrap();
    for style in ["subtle", "tinted", "rich", "neutral"] {
        let settings = store.settings().unwrap();
        let mut theme = serde_json::to_value(settings.theme).unwrap();
        theme["style"] = json!(style);
        store
            .update_settings(settings.revision, json!({"theme":theme}))
            .unwrap();
        handle.notify();
        let result = wait(&handle, |v| v["state"] == "active" && v["style"] == style).await;
        assert_eq!(result["generation_jobs"], first["generation_jobs"]);
        assert_eq!(store.meta("current-v2").unwrap(), current);
    }
    handle.shutdown().await;
}

#[tokio::test]
async fn arming_protection_never_generates_a_theme_from_the_old_private_photo() {
    let (_root, store, runtime, _) = setup();
    store.set_meeting(true).unwrap();
    let handle = worker::start(store, runtime, CancellationToken::new());
    let status = wait(&handle, |s| {
        s["state"] == "waiting_for_screened_photo" || s["state"] == "active"
    })
    .await;
    assert_eq!(status["state"], "waiting_for_screened_photo");
    assert_eq!(status["generation_jobs"], 0);
    handle.shutdown().await;
}
