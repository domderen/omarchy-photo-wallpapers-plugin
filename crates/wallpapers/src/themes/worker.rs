//! Independent theme actor. Image/HTTP/subprocess work never runs in QML or the
//! wallpaper controller. Activation finishes before shutdown to preserve staging.
use super::{
    location::{ResolvedLocation, Resolver},
    palette::Generator,
    platform::{Manager, Outcome, Runner, SystemRunner},
};
use crate::{solar, storage::Store};
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Instant};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

#[async_trait::async_trait]
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
    async fn wait_until(&self, time: DateTime<Utc>) {
        tokio::time::sleep((time - self.now()).to_std().unwrap_or_default()).await;
    }
}
pub struct SystemClock;
#[async_trait::async_trait]
impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
    async fn wait_until(&self, time: DateTime<Utc>) {
        crate::platform::deadline(Some(time)).await;
    }
}
pub struct Runtime {
    pub home: PathBuf,
    pub runner: Arc<dyn Runner>,
    pub generator: Option<Generator>,
    pub clock: Arc<dyn Clock>,
    pub network: bool,
}
impl Runtime {
    pub fn system() -> Result<Self> {
        Ok(Self {
            home: dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("theme_environment_unavailable"))?,
            runner: Arc::new(SystemRunner),
            generator: None,
            clock: Arc::new(SystemClock),
            network: true,
        })
    }
}
#[derive(Clone)]
pub struct Handle {
    wake: watch::Sender<u64>,
    pub updates: watch::Receiver<Value>,
    stop: CancellationToken,
    task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}
impl Handle {
    pub fn notify(&self) {
        self.wake.send_modify(|v| *v = v.wrapping_add(1));
    }
    pub fn status(&self) -> Value {
        self.updates.borrow().clone()
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        if let Some(task) = self.task.lock().await.take() {
            let _ = task.await;
        }
    }
}
pub fn start(store: Store, runtime: Runtime, stop: CancellationToken) -> Handle {
    let (wake, rx) = watch::channel(0);
    let (updates, status) =
        watch::channel(json!({"enabled":false,"state":"disabled","error":null}));
    let manager = Manager::new(store.clone(), runtime.home.clone(), runtime.runner.clone());
    let generator = runtime.generator.clone();
    let now = runtime.clock.now();
    let worker = Worker {
        store,
        runtime,
        manager,
        rx,
        updates,
        stop: stop.clone(),
        status: json!({"enabled":false,"state":"disabled","error":null,"generation_jobs":0,"cache_hits":0}),
        generator,
        location: None,
        next_location: now,
        next_wake: now,
        weather_stamp: None,
        location_settings: None,
    };
    let task = tokio::spawn(worker.run());
    Handle {
        wake,
        updates: status,
        stop,
        task: Arc::new(Mutex::new(Some(task))),
    }
}
#[derive(Clone, Deserialize)]
struct Image {
    render_path: PathBuf,
    render_digest: String,
    #[serde(flatten)]
    layout: super::preference::ContentLayout,
}
struct Worker {
    store: Store,
    runtime: Runtime,
    manager: Manager,
    rx: watch::Receiver<u64>,
    updates: watch::Sender<Value>,
    stop: CancellationToken,
    status: Value,
    generator: Option<Generator>,
    location: Option<ResolvedLocation>,
    next_location: DateTime<Utc>,
    next_wake: DateTime<Utc>,
    weather_stamp: Option<(std::time::SystemTime, u64)>,
    location_settings: Option<(String, Option<super::Location>)>,
}
impl Worker {
    fn publish(&self) {
        self.updates.send_if_modified(|v| {
            if *v == self.status {
                false
            } else {
                *v = self.status.clone();
                true
            }
        });
    }
    fn state(&mut self, state: &str) {
        self.status["state"] = json!(state);
        self.publish();
    }
    async fn run(mut self) {
        let mut again = true;
        loop {
            if !again {
                tokio::select! {_=self.stop.cancelled()=>break,result=self.rx.changed()=>{if result.is_err(){break;}},_=self.runtime.clock.wait_until(self.next_wake)=>{}}
            }
            if self.stop.is_cancelled() {
                break;
            }
            self.rx.borrow_and_update();
            match self.cycle().await {
                Ok(immediate) => again = immediate || self.rx.has_changed().unwrap_or(false),
                Err(error) => {
                    if self.stop.is_cancelled() {
                        break;
                    }
                    self.status["error"] = json!(public_error(&error));
                    self.state("error");
                    self.next_wake = self.runtime.clock.now() + Duration::minutes(5);
                    again = self.rx.has_changed().unwrap_or(false);
                }
            }
        }
    }
    async fn cycle(&mut self) -> Result<bool> {
        let settings = self.store.settings()?;
        let privacy_generation = self.store.meta("privacy-generation")?;
        let now = self.runtime.clock.now();
        self.next_wake = now + Duration::minutes(10);
        self.status["enabled"] = json!(settings.match_colors);
        self.status["style"] = json!(settings.theme.style);
        self.status["error"] = Value::Null;
        if !settings.match_colors {
            self.manager.restore().await?;
            self.status["mode"] = Value::Null;
            self.status["solar"] = Value::Null;
            self.status["location_error"] = Value::Null;
            self.state("disabled");
            return Ok(false);
        }
        if self.manager.suspended()? {
            self.state("suspended");
            return Ok(false);
        }
        if settings.privacy.active() && self.store.meta("privacy-state-v1")?["state"] != "active" {
            self.state("waiting_for_screened_photo");
            return Ok(false);
        }
        let weather = self
            .runtime
            .home
            .join(".local/state/omarchy/settings/weather.json");
        let stamp = std::fs::metadata(weather)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())));
        let location_settings = (
            settings.theme.location_mode.clone(),
            settings.theme.location.clone(),
        );
        let location_settings_changed = self.location_settings.as_ref() != Some(&location_settings);
        if (settings.theme.mode == "auto" || settings.theme.location_mode == "fixed")
            && (now >= self.next_location
                || stamp != self.weather_stamp
                || location_settings_changed)
        {
            self.status["location_error"] = Value::Null;
            let resolver = Resolver::new()?;
            let resolution = tokio::select! {_=self.stop.cancelled()=>anyhow::bail!("theme_cancelled"),result=resolver.resolve(&self.store,&settings.theme,&self.runtime.home,self.runtime.network)=>result};
            match resolution {
                Ok(location) => self.location = Some(location),
                Err(_) => self.status["location_error"] = json!("location_unavailable"),
            };
            self.next_location = now
                + if self.location.as_ref().is_some_and(|v| !v.stale) {
                    Duration::hours(1)
                } else {
                    Duration::minutes(15)
                };
            self.weather_stamp = stamp;
            // Keep comparison data private, not in the public status projection.
            self.location_settings = Some(location_settings);
        }
        if settings.theme.mode == "auto" && self.next_location > now {
            self.next_wake = self.next_wake.min(self.next_location);
        }
        self.status["location"]=self.location.as_ref().map(|v|json!({"name":v.location.name,"provider":v.provider,"stale":v.stale,"resolved_at":v.resolved_at})).unwrap_or(Value::Null);
        let solar = self
            .location
            .as_ref()
            .map(|v| {
                solar::state(
                    now,
                    &v.location,
                    settings.theme.sunrise_offset_minutes,
                    settings.theme.sunset_offset_minutes,
                )
            })
            .transpose()?;
        let mode = if settings.theme.mode != "auto" {
            settings.theme.mode.clone()
        } else {
            solar.as_ref().map(|s| s.mode.clone()).unwrap_or_else(|| {
                self.store
                    .meta("theme-last-mode")
                    .ok()
                    .and_then(|v| {
                        v.as_str()
                            .filter(|s| ["light", "dark"].contains(s))
                            .map(str::to_owned)
                    })
                    .unwrap_or("dark".into())
            })
        };
        if settings.theme.mode == "auto"
            && let Some(next) = solar.as_ref().and_then(|s| s.next_change)
        {
            self.next_wake = self.next_wake.min(next);
        }
        self.status["mode"] = json!(mode);
        self.status["mode_source"] = json!(if settings.theme.mode != "auto" {
            "manual"
        } else if solar.is_some() {
            "solar"
        } else {
            "last_known"
        });
        self.status["solar"] = serde_json::to_value(&solar)?;
        let current_value = self.store.meta("current-v2")?;
        let current: Option<Image> = serde_json::from_value(current_value).ok();
        let Some(current) = current else {
            self.state("waiting_for_photo");
            return Ok(false);
        };
        if self.generator.is_none() {
            self.generator = Some(Generator::detect(&self.store).await?);
        }
        let generator = self
            .generator
            .clone()
            .ok_or_else(|| anyhow::anyhow!("theme_generator_unavailable"))?;
        self.status["generator"] = json!(generator.version);
        let mut images = vec![current.clone()];
        if let Ok(ready) = serde_json::from_value::<Vec<Image>>(self.store.meta("ready-v2")?) {
            for image in ready.into_iter().take(3) {
                if !images
                    .iter()
                    .any(|p| p.render_digest == image.render_digest)
                {
                    images.push(image);
                }
            }
        }
        for (index, image) in images.into_iter().enumerate() {
            self.state("preparing");
            let started = Instant::now();
            let generated = tokio::select! {_=self.stop.cancelled()=>anyhow::bail!("theme_cancelled"),result=self.rx.changed()=>{if result.is_err(){anyhow::bail!("theme_cancelled");}return Ok(true);},result=generator.prepare(&self.store,&image.render_path,&image.render_digest,&self.stop)=>result}?;
            if generated.cache_hit {
                self.status["cache_hits"] =
                    json!(self.status["cache_hits"].as_u64().unwrap_or(0) + 1);
            } else {
                self.status["generation_jobs"] =
                    json!(self.status["generation_jobs"].as_u64().unwrap_or(0) + 1);
                self.status["last_generation_ms"] = json!(started.elapsed().as_secs_f64() * 1000.);
            }
            let score_store = self.store.clone();
            let score_digest = image.render_digest.clone();
            let score_layout = image.layout.clone();
            // A missing/legacy thumbnail must not make a valid cached theme fail.
            let _ = tokio::task::spawn_blocking(move || {
                super::preference::measure_content(&score_store, &score_digest, &score_layout)
            })
            .await;
            let latest = self.store.settings()?;
            if !latest.match_colors
                || latest.theme != settings.theme
                || self.store.meta("privacy-generation")? != privacy_generation
                || self.store.meta("current-v2")?["render_digest"] != current.render_digest
            {
                return Ok(true);
            }
            if index == 0 {
                let styled = super::style::apply_pair(&generated.pair, &settings.theme.style)?;
                let palette = if mode == "light" {
                    &styled.light
                } else {
                    &styled.dark
                };
                self.state("applying");
                let started = Instant::now();
                let outcome = self.manager.apply(palette).await?;
                if outcome == Outcome::Suspended {
                    self.state("suspended");
                    return Ok(false);
                }
                if outcome == Outcome::Applied {
                    self.status["last_apply_ms"] = json!(started.elapsed().as_secs_f64() * 1000.);
                }
                if self.store.meta("theme-last-mode")? != json!(mode) {
                    self.store.set_meta("theme-last-mode", &json!(mode))?;
                }
            }
            if self.rx.has_changed().unwrap_or(false) {
                return Ok(true);
            }
        }
        self.state("active");
        Ok(false)
    }
}
fn public_error(error: &anyhow::Error) -> &'static str {
    match error.to_string().as_str() {
        "theme_generator_unavailable" => "theme_generator_unavailable",
        "palette_generation_timed_out" => "palette_generation_timed_out",
        "palette_generation_failed" => "palette_generation_failed",
        "palette_output_too_large" => "palette_output_too_large",
        "invalid_palette" => "invalid_palette",
        "theme_apply_failed" => "theme_apply_failed",
        "theme_apply_timed_out" => "theme_apply_timed_out",
        "theme_restore_failed" => "theme_restore_failed",
        "theme_destination_not_owned" => "theme_destination_not_owned",
        "theme_ownership_lost" => "theme_ownership_lost",
        "location_unavailable" => "location_unavailable",
        _ => "theme_operation_failed",
    }
}
