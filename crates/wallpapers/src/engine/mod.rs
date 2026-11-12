mod network;
pub mod privacy;
use crate::{
    icloud::{auth::AuthInfo, photos::PhotosProvider},
    imaging::{self, PrepareOptions, Prepared},
    ipc::Request,
    model::{Asset, Command, Settings, Source},
    platform::Platform,
    storage::Store,
};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::{RwLock, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Schedule {
    pub next_due: Option<DateTime<Utc>>,
}
impl Schedule {
    pub fn due(&self, now: DateTime<Utc>, paused: bool, pinned: bool) -> bool {
        !paused && !pinned && self.next_due.is_some_and(|d| d <= now)
    }
    pub fn advance(&mut self, now: DateTime<Utc>, interval: u64) {
        self.next_due = Some(now + Duration::seconds(interval as i64));
    }
}
type ProviderSlot = Arc<RwLock<Option<PhotosProvider>>>;
#[async_trait]
pub trait Pipeline: Send + Sync {
    async fn prepare(
        &self,
        store: Store,
        asset: Asset,
        options: PrepareOptions,
        stop: CancellationToken,
    ) -> Result<Prepared>;
    fn provider_slot(&self) -> Option<ProviderSlot> {
        None
    }
}
pub struct NativePipeline {
    provider: ProviderSlot,
    heavy: Arc<tokio::sync::Semaphore>,
}
impl Default for NativePipeline {
    fn default() -> Self {
        Self {
            provider: Default::default(),
            heavy: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}
#[async_trait]
impl Pipeline for NativePipeline {
    fn provider_slot(&self) -> Option<ProviderSlot> {
        Some(self.provider.clone())
    }
    async fn prepare(
        &self,
        store: Store,
        asset: Asset,
        options: PrepareOptions,
        stop: CancellationToken,
    ) -> Result<Prepared> {
        let cache_store = store.clone();
        tokio::task::spawn_blocking(move || {
            let mut protected = Vec::new();
            for key in ["current-v2", "history-v2", "ready-v2", "applying-v2"] {
                let value = cache_store.meta(key)?;
                for item in value.as_array().cloned().unwrap_or_else(|| vec![value]) {
                    if let Some(path) = item["render_path"].as_str() {
                        protected.push(std::path::PathBuf::from(path));
                    }
                    if let Some(digest)=item["render_digest"].as_str() {
                        let db=cache_store.connection()?;
                        let mut query=db.prepare("SELECT data FROM privacy_render_content WHERE render_sha256=?1 AND length(data)<=16384 LIMIT 1000")?;
                        let rows=query.query_map([digest],|r|r.get::<_,String>(0))?;
                        let privacy_store=crate::privacy::store::PrivacyStore::new(cache_store.clone());
                        for row in rows {
                            if let Ok(content)=serde_json::from_str::<crate::privacy::store::ContentKey>(&row?)
                                && content.validate().is_ok()
                            { protected.push(privacy_store.preview_path(&content.preview_sha256)?); }
                        }
                    }
                }
            }
            imaging::evict(
                &cache_store,
                cache_store.settings()?.cache_bytes,
                &protected,
            )
        })
        .await??;
        let admission = privacy::AdmissionContext::from_store(&store, privacy::policy_key(&store))?;
        let worker = store.clone();
        let cached_asset = asset.clone();
        let cached_options = options.clone();
        let cached = tokio::task::spawn_blocking(move || {
            imaging::cached_render(&worker, &cached_asset, &cached_options)
        })
        .await??;
        if let Some(prepared) = &cached
            && (!admission.active || privacy::admit(&store, prepared, &admission)?)
        {
            return Ok(prepared.clone());
        }
        let screener = if admission.active {
            Some(
                privacy::screening_worker(
                    &store,
                    admission
                        .policy_key
                        .as_ref()
                        .context("privacy_models_unavailable")?,
                )
                .context("privacy_models_unavailable")?,
            )
        } else {
            None
        };
        let provider = self
            .provider
            .read()
            .await
            .clone()
            .context("apple_sign_in_required")?;
        let path = tokio::select! {_ =stop.cancelled()=>anyhow::bail!("preparation_cancelled"),result=provider.download(&asset,&store.paths)=>result?};
        let permit = Arc::new(
            tokio::select! { _=stop.cancelled()=>anyhow::bail!("preparation_cancelled"),p=self.heavy.clone().acquire_owned()=>p? },
        );
        let mut decoded = None;
        let mut binding = None;
        if let Some(screener) = screener {
            let s = store.clone();
            let a = asset.clone();
            let p = path.clone();
            let hold = permit.clone();
            let (content, rgb, job) = tokio::task::spawn_blocking(move || {
                let _hold = hold;
                privacy::full_frame_preview(&s, &a, &p)
            })
            .await??;
            ensure!(!stop.is_cancelled(), "preparation_cancelled");
            let key = admission
                .policy_key
                .as_ref()
                .context("privacy_models_unavailable")?;
            let privacy_store = crate::privacy::store::PrivacyStore::new(store.clone());
            let choice = privacy_store.choice_for(&content)?;
            ensure!(
                choice != crate::privacy::policy::PrivacyChoice::PersonalOnly,
                "privacy_filtered"
            );
            if choice != crate::privacy::policy::PrivacyChoice::Allow {
                let decision = if let Some(decision) = privacy_store.lookup(&content, key)? {
                    decision
                } else {
                    let jobs = store.meta("privacy-screening-jobs")?.as_u64().unwrap_or(0);
                    store.set_meta("privacy-screening-jobs", &json!(jobs.saturating_add(1)))?;
                    let result = screener
                        .screen(job, stop.clone())
                        .await
                        .context("privacy_screening_failed")?;
                    ensure!(
                        result.qualified
                            && result.modelset_sha256 == key.modelset_sha256
                            && result.preview_sha256 == content.preview_sha256,
                        "privacy_models_unavailable"
                    );
                    let decision = crate::privacy::store::Decision::from_signals(
                        content.clone(),
                        key.clone(),
                        result.signals,
                    );
                    ensure!(!stop.is_cancelled(), "preparation_cancelled");
                    privacy_store.save(&decision)?;
                    decision
                };
                ensure!(
                    decision.state == crate::privacy::policy::ScreeningState::Clear,
                    "privacy_filtered"
                );
            }
            decoded = Some((content.media_sha256.clone(), rgb));
            binding = Some(content);
        }
        ensure!(!stop.is_cancelled(), "preparation_cancelled");
        let hold = permit.clone();
        let render_store = store.clone();
        let prepared = if let Some(cached) = cached {
            cached
        } else {
            tokio::task::spawn_blocking(move || {
                let _hold = hold;
                imaging::prepare_with_decoded(
                    &render_store,
                    &asset,
                    &path,
                    &options,
                    &stop,
                    decoded,
                )
            })
            .await??
        };
        if let Some(content) = binding {
            crate::privacy::store::PrivacyStore::new(store.clone())
                .bind_render(&prepared.render_digest, &content)?;
        }
        ensure!(
            !admission.active || privacy::admit(&store, &prepared, &admission)?,
            "privacy_filtered"
        );
        Ok(prepared)
    }
}
pub(crate) enum Message {
    ModelsProgress {
        job: String,
        value: Value,
    },
    ModelsDone {
        job: String,
        result: Result<()>,
    },
    PrivacyApplied {
        transition_id: String,
        generation: u64,
        cover: bool,
        result: Result<()>,
    },
    Request(Request, oneshot::Sender<Value>),
    Wake,
    Geometry,
    GeometryResult(Result<((u32, u32), Vec<String>)>),
    Helper(Value),
    Theme(Value),
    SyncProgress(Value),
    Prepared {
        epoch: u64,
        id: String,
        result: Result<Prepared>,
    },
    Applied {
        epoch: u64,
        job: String,
        prepared: Box<Prepared>,
        history_index: Option<usize>,
        elapsed_ms: f64,
        result: Result<()>,
    },
    Auth {
        info: AuthInfo,
        busy: bool,
        scope: Option<String>,
    },
    Synced {
        job: String,
        result: Result<()>,
    },
    Paired {
        result: Result<()>,
    },
}
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<Message>,
    pub updates: watch::Receiver<Value>,
    pub paths: crate::storage::Paths,
    stop: CancellationToken,
    themes: Option<crate::themes::worker::Handle>,
}
impl EngineHandle {
    pub fn status(&self) -> Value {
        self.updates.borrow().clone()
    }
    pub async fn request(&self, mut request: Request) -> Result<Value> {
        if request.command == "status" {
            return Ok(self.status());
        }
        if request.id.is_empty() {
            request.id = uuid::Uuid::new_v4().to_string();
        }
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Message::Request(request, tx))
            .await
            .map_err(|_| anyhow::anyhow!("engine_unavailable"))?;
        rx.await.map_err(|_| anyhow::anyhow!("engine_unavailable"))
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        let mut updates = self.updates.clone();
        while updates.borrow_and_update()["daemon_running"] == true {
            if updates.changed().await.is_err() {
                break;
            }
        }
        if let Some(themes) = &self.themes {
            themes.shutdown().await;
        }
    }
    pub async fn geometry_changed(&self) {
        let _ = self.tx.send(Message::Geometry).await;
    }
}
pub struct Engine;
impl Engine {
    pub async fn start(
        store: Store,
        platform: Arc<dyn Platform>,
        pipeline: Arc<dyn Pipeline>,
        background: bool,
    ) -> Result<EngineHandle> {
        let theme_runtime = crate::themes::worker::Runtime::system()
            .ok()
            .map(|mut runtime| {
                runtime.network = background;
                runtime
            });
        Self::start_with_theme_runtime(store, platform, pipeline, background, theme_runtime).await
    }
    /// Explicit environment dependencies support headless operation and
    /// controlled clock/location integration without diagnostic IPC switches.
    pub async fn start_with_theme_runtime(
        store: Store,
        platform: Arc<dyn Platform>,
        pipeline: Arc<dyn Pipeline>,
        background: bool,
        theme_runtime: Option<crate::themes::worker::Runtime>,
    ) -> Result<EngineHandle> {
        crate::migration::discover_legacy(&store)?;
        store.recover_commands()?;
        let settings = store.settings()?;
        let geometry = platform.geometry().await.unwrap_or((0, 0));
        let output_names = platform.output_names().await.unwrap_or_default();
        let pool = store.eligible_ids(&settings.sources)?;
        let sources = store.sources()?;
        let current: Option<Prepared> =
            serde_json::from_value(store.meta("current-v2")?).unwrap_or_default();
        let history: Vec<Prepared> =
            serde_json::from_value(store.meta("history-v2")?).unwrap_or_default();
        let history_index = store
            .meta("history-index-v2")?
            .as_u64()
            .map(|n| n as usize)
            .unwrap_or_else(|| history.len().saturating_sub(1));
        let mut schedule: Schedule =
            serde_json::from_value(store.meta("schedule-v2")?).unwrap_or_default();
        if current.is_some() && schedule.next_due.is_none() {
            schedule.advance(Utc::now(), settings.interval_seconds);
            store.set_meta("schedule-v2", &serde_json::to_value(&schedule)?)?;
        }
        let last_refresh = serde_json::from_value(store.meta("last_sync")?).unwrap_or_default();
        let ready: Vec<Prepared> =
            serde_json::from_value(store.meta("ready-v2")?).unwrap_or_default();
        let (tx, rx) = mpsc::channel(64);
        let (updates, receiver) = watch::channel(json!({}));
        let stop = CancellationToken::new();
        let net = background.then(|| {
            network::start(
                store.clone(),
                tx.clone(),
                pipeline.provider_slot().unwrap_or_default(),
                stop.clone(),
            )
        });
        let themes = theme_runtime.map(|runtime| {
            crate::themes::worker::start(store.clone(), runtime, stop.child_token())
        });
        if let Some(themes) = &themes {
            let mut statuses = themes.updates.clone();
            let tx = tx.clone();
            let stopped = stop.clone();
            tokio::spawn(async move {
                loop {
                    let value = statuses.borrow_and_update().clone();
                    if tx.send(Message::Theme(value)).await.is_err() {
                        break;
                    }
                    tokio::select! {_=stopped.cancelled()=>break,result=statuses.changed()=>{if result.is_err(){break;}}}
                }
            });
        }
        let handle = EngineHandle {
            tx: tx.clone(),
            updates: receiver,
            paths: store.paths.clone(),
            stop: stop.clone(),
            themes: themes.clone(),
        };
        let theme_status = json!({"enabled":settings.match_colors,"state":"disabled","error":null});
        let models_status =
            json!({"state":if privacy::models_present(&store){"ready"}else{"unavailable"}});
        let mut controller = Controller {
            store,
            settings,
            geometry,
            platform,
            pipeline,
            tx,
            updates,
            stop,
            net,
            pool,
            sources,
            current,
            history,
            history_index,
            schedule,
            ready,
            epoch: 0,
            prep: None,
            applying: None,
            want: None,
            failed: HashMap::new(),
            cursor: 0,
            refill_count: 0,
            auth: AuthInfo::default(),
            auth_busy: false,
            auth_transition: false,
            sync_progress: Value::Null,
            error: None,
            last_transition_ms: None,
            analysis_jobs: 0,
            render_hits: 0,
            last_refresh,
            retry_at: None,
            consecutive_failures: 0,
            refill_after: None,
            quiet_delay: if background { 15 } else { 0 },
            helper: json!({}),
            next_sync: background.then(|| Utc::now() + Duration::hours(1)),
            themes,
            theme_status,
            protection: privacy::Session::off(),
            output_names,
            restore_pending: false,
            models_job: None,
            models_status,
            privacy_counts: json!({}),
            privacy_eligible_ids: HashSet::new(),
        };
        controller.pool.shuffle(&mut rand::rng());
        controller.refresh_privacy_counts()?;
        controller.prune_ready()?;
        if controller.settings.privacy.active() {
            controller.arm_protection(true)?;
        }
        controller.publish();
        tokio::spawn(controller.run(rx));
        let _ = handle.tx.send(Message::Wake).await;
        Ok(handle)
    }
}
struct Controller {
    store: Store,
    settings: Settings,
    geometry: (u32, u32),
    platform: Arc<dyn Platform>,
    pipeline: Arc<dyn Pipeline>,
    tx: mpsc::Sender<Message>,
    updates: watch::Sender<Value>,
    stop: CancellationToken,
    net: Option<mpsc::Sender<network::Job>>,
    pool: Vec<String>,
    sources: Vec<Source>,
    current: Option<Prepared>,
    history: Vec<Prepared>,
    history_index: usize,
    schedule: Schedule,
    ready: Vec<Prepared>,
    epoch: u64,
    prep: Option<CancellationToken>,
    applying: Option<CancellationToken>,
    want: Option<String>,
    failed: HashMap<String, DateTime<Utc>>,
    cursor: usize,
    refill_count: u64,
    auth: AuthInfo,
    auth_busy: bool,
    auth_transition: bool,
    sync_progress: Value,
    error: Option<String>,
    last_transition_ms: Option<f64>,
    analysis_jobs: u64,
    render_hits: u64,
    last_refresh: Option<DateTime<Utc>>,
    retry_at: Option<DateTime<Utc>>,
    consecutive_failures: u32,
    refill_after: Option<DateTime<Utc>>,
    quiet_delay: i64,
    helper: Value,
    next_sync: Option<DateTime<Utc>>,
    themes: Option<crate::themes::worker::Handle>,
    theme_status: Value,
    protection: privacy::Session,
    output_names: Vec<String>,
    restore_pending: bool,
    models_job: Option<(String, CancellationToken)>,
    models_status: Value,
    privacy_counts: Value,
    privacy_eligible_ids: HashSet<String>,
}
impl Controller {
    fn privacy_status(&self) -> Value {
        let mut value = self.protection.status(&self.store);
        value["counts"] = self.privacy_counts.clone();
        value["models"] = self.model_status();
        value["model_state"] = self.models_status["state"].clone();
        value
    }
    fn refresh_model_presence(&mut self) {
        if self.models_job.is_none()
            && self.models_status["state"] == "ready"
            && !privacy::models_present(&self.store)
        {
            self.models_status = json!({"state":"unavailable"});
        }
    }
    fn refresh_privacy_counts(&mut self) -> Result<()> {
        let (counts, eligible) = crate::privacy::store::PrivacyStore::new(self.store.clone())
            .summary(privacy::policy_key(&self.store).as_ref(), &self.pool)?;
        self.privacy_counts = counts;
        self.privacy_eligible_ids = eligible;
        Ok(())
    }
    fn model_status(&self) -> Value {
        let mut status = self.models_status.clone();
        static BYTES: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        status["download_bytes"] = json!(*BYTES.get_or_init(|| {
            crate::privacy::install::selected()
                .map(|set| {
                    set.runtime_manifest
                        .models
                        .iter()
                        .map(|m| m.artifact.bytes)
                        .sum::<u64>()
                        + set.runtime_archive.bytes
                })
                .unwrap_or(0)
        }));
        status["supported"] = json!(cfg!(all(target_os = "linux", target_arch = "x86_64")));
        status
    }
    fn options(&self) -> PrepareOptions {
        PrepareOptions {
            width: self.geometry.0,
            height: self.geometry.1,
            person_sensitive: self.settings.sources.iter().any(|s| s.kind == "person"),
            fallback: self.settings.fallback.clone(),
            enhancements: self.settings.enhancements.clone(),
            preferred_crop: None,
        }
    }
    fn publish(&self) {
        let current=self.current.as_ref().filter(|p| !self.settings.privacy.active() || self.admission().is_ok_and(|context|privacy::admit(&self.store,p,&context).unwrap_or(false))).map(|p| {
            let (choice,can_allow)=privacy::current_choice(&self.store,p).unwrap_or((crate::privacy::policy::PrivacyChoice::Auto,false));
            json!({"asset_id":p.asset_id,"revision":p.revision,"render_path":p.render_path,"preview_path":p.render_path,"mode":p.mode,"rendition":p.rendition,"provenance":p.provenance,"period":crate::themes::preference::period(&self.store,&p.asset_id).unwrap_or("auto".into()),"privacy_choice":choice,"privacy_allow_available":can_allow})
        });
        let value = json!({"daemon_running":!self.stop.is_cancelled(),"backend":"rust","connection":if self.auth.state=="authenticated"{"online"}else{&self.auth.state},"auth":self.auth,"auth_busy":self.auth_busy,"account_sync_pending":self.auth_transition,"paused":self.settings.paused,"pinned":self.effective_pin(),"personal_pinned":self.settings.pinned,"current":current,"next_due":self.schedule.next_due,"settings_revision":self.settings.revision,"interval_seconds":self.settings.interval_seconds,"selected_source_count":self.settings.sources.len(),"eligible_count":self.pool.len(),"ready_count":self.ready.len(),"preparing":self.prep.is_some(),"applying":self.applying.is_some(),"pending_actions":self.want.is_some(),"last_sync":self.last_refresh,"error":self.error,"metrics":{"screening_jobs":self.store.meta("privacy-screening-jobs").ok().and_then(|v|v.as_u64()).unwrap_or(0),"analysis_jobs":self.analysis_jobs,"render_cache_hits":self.render_hits,"last_transition_ms":self.last_transition_ms},"helper_capability":"optional_metadata_only","helper":self.helper,"helper_url":self.helper["url"],"source_count":self.sources.len(),"preparation_retry_at":self.retry_at});
        let mut value = value;
        value["protocol_version"] = json!(crate::ipc::PROTOCOL_VERSION);
        value["backend_version"] = json!(env!("CARGO_PKG_VERSION"));
        value["sync_progress"] = self.sync_progress.clone();
        value["theme"] = self.theme_status.clone();
        value["privacy"] = self.privacy_status();
        self.updates.send_if_modified(|old| {
            if *old == value {
                false
            } else {
                *old = value;
                true
            }
        });
    }
    fn persist_queue(&self) -> Result<()> {
        self.store
            .set_meta("ready-v2", &serde_json::to_value(&self.ready)?)?;
        self.notify_theme();
        Ok(())
    }
    fn notify_theme(&self) {
        if let Some(themes) = &self.themes {
            themes.notify();
        }
    }
    fn persist_schedule(&self) -> Result<()> {
        self.store
            .set_meta("schedule-v2", &serde_json::to_value(&self.schedule)?)
    }
    fn cancel(&mut self, clear_ready: bool) {
        self.epoch += 1;
        if let Some(stop) = &self.prep {
            stop.cancel();
        }
        if let Some(stop) = &self.applying {
            stop.cancel();
        }
        if let Some(job) = self.want.take() {
            let _ = self
                .store
                .finish_command(&job, json!({"error":"command_cancelled"}));
        }
        if clear_ready {
            self.ready.clear();
            let _ = self.persist_queue();
        }
    }
    fn effective_pin(&self) -> Option<String> {
        if !self.settings.privacy.active() {
            return self.settings.pinned.clone();
        }
        let candidate = if self.settings.privacy.meeting
            && self.store.meta("privacy-meeting-pin-set").ok() == Some(json!(true))
        {
            self.store
                .meta("privacy-meeting-pin")
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
        } else {
            self.settings.pinned.clone()
        };
        candidate.filter(|id| {
            self.current.as_ref().is_some_and(|p| {
                &p.asset_id == id
                    && self
                        .admission()
                        .is_ok_and(|c| privacy::admit(&self.store, p, &c).unwrap_or(false))
            })
        })
    }
    fn persist_protection(&self) -> Result<()> {
        self.store
            .set_meta("privacy-state-v1", &self.protection.status(&self.store))
    }
    fn arm_protection(&mut self, advance: bool) -> Result<()> {
        self.refresh_model_presence();
        if advance {
            self.store.advance_privacy_generation()?;
        }
        self.cancel(false);
        self.restore_pending = false;
        let context = self.admission()?;
        let mode = privacy::neutral_mode(&self.settings, &self.theme_status);
        let cover = privacy::neutral(&self.store, context.generation, mode)?;
        let target = self
            .current
            .as_ref()
            .filter(|p| {
                (!privacy::is_neutral(&self.store, p) || p.rendition == mode)
                    && privacy::admit(&self.store, p, &context).unwrap_or(false)
            })
            .cloned()
            .unwrap_or_else(|| cover.clone());
        self.protection =
            privacy::Session::arm(context.generation, target, cover, self.output_names.clone());
        if self.protection.outputs.is_empty() {
            self.protection.reason = Some("no_active_outputs");
        }
        self.refresh_privacy_counts()?;
        self.persist_protection()?;
        self.prune_ready()?;
        self.persist_queue()?;
        self.ensure_protection()
    }
    fn leave_protection(&mut self) -> Result<()> {
        self.cancel(false);
        self.protection = privacy::Session::off();
        self.restore_pending = true;
        self.persist_protection()?;
        self.ensure_protection()
    }
    fn start_privacy_apply(&mut self, prepared: Prepared, cover: bool) -> Result<()> {
        ensure!(self.applying.is_none(), "apply_pending");
        let context = self.admission()?;
        ensure!(
            privacy::admit(&self.store, &prepared, &context)?,
            "privacy_filtered"
        );
        self.store
            .set_meta("applying-v2", &serde_json::to_value(&prepared)?)?;
        self.applying = Some(CancellationToken::new());
        let platform = self.platform.clone();
        let tx = self.tx.clone();
        let transition_id = self.protection.transition_id.clone();
        let generation = self.protection.generation;
        tokio::spawn(async move {
            let result = platform.apply_instant(&prepared).await;
            let _ = tx
                .send(Message::PrivacyApplied {
                    transition_id,
                    generation,
                    cover,
                    result,
                })
                .await;
        });
        Ok(())
    }
    fn fallback_reason(&self) -> Option<&'static str> {
        if !self
            .current
            .as_ref()
            .is_some_and(|p| privacy::is_neutral(&self.store, p))
        {
            return None;
        }
        Some(if self.settings.sources.is_empty() {
            "no_sources"
        } else if self.pool.is_empty() {
            "empty_source_pool"
        } else if self.settings.paused {
            "rotation_paused"
        } else if self.models_status["state"] != "ready" {
            "privacy_models_unavailable"
        } else if self.privacy_counts["clear"].as_u64().unwrap_or(0) > 0 {
            "preparing_screened_photo"
        } else if self.retry_at.is_some() {
            "screening_waiting"
        } else {
            "no_screened_photo"
        })
    }
    fn ensure_protection(&mut self) -> Result<()> {
        if !self.settings.privacy.active() {
            if self.restore_pending && self.applying.is_none() {
                self.restore_pending = false;
                let saved = self.store.meta("privacy-personal-context")?;
                let previous = serde_json::from_value::<Prepared>(saved["current"].clone()).ok();
                if let Some(previous) = previous
                    && saved["account"] == self.store.meta("base-account-id")?
                    && valid_render(&previous)
                    && privacy::admit(&self.store, &previous, &self.admission()?)?
                {
                    self.start_apply(
                        format!("privacy-restore:{}", uuid::Uuid::new_v4()),
                        previous,
                        None,
                    );
                } else {
                    self.store
                        .set_meta("privacy-personal-context", &Value::Null)?;
                }
            }
            return Ok(());
        }
        if self.protection.state == "active" {
            if self.current.as_ref().is_none_or(|p| {
                (privacy::is_neutral(&self.store, p)
                    && p.rendition != privacy::neutral_mode(&self.settings, &self.theme_status))
                    || !privacy::admit(
                        &self.store,
                        p,
                        &self.admission().unwrap_or(privacy::AdmissionContext {
                            generation: u64::MAX,
                            policy_key: None,
                            active: true,
                        }),
                    )
                    .unwrap_or(false)
            }) {
                self.arm_protection(true)?;
            } else {
                let reason = self.fallback_reason();
                if self.protection.reason != reason {
                    self.protection.reason = reason;
                    self.persist_protection()?;
                }
            }
            return Ok(());
        }
        if self.protection.state == "error" {
            return Ok(());
        }
        if !self.protection.cover_started && self.applying.is_none() {
            self.protection.cover_started = true;
            self.start_privacy_apply(
                self.protection
                    .cover
                    .clone()
                    .context("privacy_state_unavailable")?,
                true,
            )?;
        }
        if self.protection.cover_done
            && self.protection.all_preloaded()
            && !self.protection.target_started
            && self.applying.is_none()
        {
            self.protection.target_started = true;
            let target = self
                .protection
                .target
                .clone()
                .context("privacy_state_unavailable")?;
            if self
                .protection
                .cover
                .as_ref()
                .is_some_and(|p| p.render_path == target.render_path)
            {
                self.protection.target_done = true;
            } else {
                self.start_privacy_apply(target, false)?;
            }
        }
        if self.protection.target_done {
            self.protection.phase = "frame";
            self.protection.reason = Some("waiting_for_desktop_frame");
            if self.protection.all_framed() {
                let mut current = self
                    .protection
                    .target
                    .clone()
                    .context("privacy_state_unavailable")?;
                ensure!(
                    privacy::admit(&self.store, &current, &self.admission()?)?,
                    "privacy_filtered"
                );
                current.applied_at = Some(Utc::now());
                self.current = Some(current);
                self.store
                    .set_meta("current-v2", &serde_json::to_value(&self.current)?)?;
                self.protection.state = "active";
                self.protection.phase = "active";
                self.protection.reason = self.fallback_reason();
                self.notify_theme();
            }
            self.persist_protection()?;
        }
        Ok(())
    }
    fn admission(&self) -> Result<privacy::AdmissionContext> {
        privacy::AdmissionContext::from_store(&self.store, privacy::policy_key(&self.store))
    }
    fn prune_ready(&mut self) -> Result<()> {
        let admission = self.admission()?;
        let key = imaging::config_key(&self.options())?;
        let mut keep = Vec::new();
        for p in self.ready.drain(..) {
            if p.config_key == key
                && self.pool.contains(&p.asset_id)
                && self.store.asset(&p.asset_id)?.is_some_and(|a| {
                    a.revision == p.revision || p.provenance == "legacy-mac-render"
                })
                && valid_render(&p)
                && (!admission.active || privacy::admit(&self.store, &p, &admission)?)
            {
                keep.push(p);
            }
        }
        self.ready = keep;
        Ok(())
    }
    fn reload_pool(&mut self) -> Result<()> {
        self.settings = self.store.settings()?;
        if self.current.is_none() {
            self.current =
                serde_json::from_value(self.store.meta("current-v2")?).unwrap_or_default();
        }
        if self.history.is_empty() {
            self.history =
                serde_json::from_value(self.store.meta("history-v2")?).unwrap_or_default();
            self.history_index = self
                .store
                .meta("history-index-v2")?
                .as_u64()
                .map(|n| n as usize)
                .unwrap_or_else(|| self.history.len().saturating_sub(1));
        }
        self.pool = self.store.eligible_ids(&self.settings.sources)?;
        self.pool.shuffle(&mut rand::rng());
        self.sources = self.store.sources()?;
        self.cursor = 0;
        self.failed.clear();
        self.retry_at = None;
        self.consecutive_failures = 0;
        self.refresh_privacy_counts()?;
        Ok(())
    }
    fn change_settings(&mut self, expected: u64, changes: Value) -> Result<Value> {
        let previous = self.settings.clone();
        self.settings = self.store.update_settings(expected, changes)?;
        if previous.privacy != self.settings.privacy {
            if self.settings.privacy.active() {
                self.arm_protection(false)?;
            } else {
                self.leave_protection()?;
            }
        }
        let content = serde_json::to_value((
            &previous.sources,
            &previous.enhancements,
            &previous.fallback,
        ))? != serde_json::to_value((
            &self.settings.sources,
            &self.settings.enhancements,
            &self.settings.fallback,
        ))?;
        if content {
            self.cancel(true);
            self.reload_pool()?;
        }
        if (!previous.paused && self.settings.paused)
            || (previous.pinned.is_none() && self.settings.pinned.is_some())
        {
            self.cancel(false);
        }
        if previous.interval_seconds != self.settings.interval_seconds
            || previous.paused != self.settings.paused
            || previous.pinned != self.settings.pinned
        {
            self.schedule
                .advance(Utc::now(), self.settings.interval_seconds);
            self.persist_schedule()?;
        }
        self.notify_theme();
        Ok(serde_json::to_value(&self.settings)?)
    }
    async fn request(&mut self, mut request: Request) -> Result<Value> {
        match request.command.as_str() {
            "models" => {
                self.refresh_model_presence();
                match request.operation.as_str() {
                    "status" => {}
                    "cancel" => {
                        if let Some((_, stop)) = &self.models_job {
                            stop.cancel();
                            self.models_status = json!({"state":"cancelling"});
                        }
                    }
                    "install" => {
                        if self.models_job.is_none() {
                            let job = uuid::Uuid::new_v4().to_string();
                            let stop = self.stop.child_token();
                            self.models_job = Some((job.clone(), stop.clone()));
                            self.models_status =
                                json!({"state":"downloading","completed":0,"total":4});
                            let store = self.store.clone();
                            let tx = self.tx.clone();
                            let progress_tx = tx.clone();
                            let progress_job = job.clone();
                            tokio::spawn(async move {
                                let progress: crate::privacy::install::Progress =
                                    Arc::new(move |value| {
                                        let _ = progress_tx.try_send(Message::ModelsProgress {
                                            job: progress_job.clone(),
                                            value,
                                        });
                                    });
                                let result =
                                    crate::privacy::install::install(store, stop, progress)
                                        .await
                                        .map(|_| ());
                                let _ = tx.send(Message::ModelsDone { job, result }).await;
                            });
                        }
                    }
                    _ => anyhow::bail!("invalid_operation"),
                }
                Ok(self.model_status())
            }
            "meeting" => {
                match request.operation.as_str() {
                    "status" => {}
                    "on" | "off" => {
                        let previous = self.settings.clone();
                        self.settings = self.store.set_meeting(request.operation == "on")?;
                        if previous.privacy != self.settings.privacy {
                            if self.settings.privacy.active() {
                                self.arm_protection(false)?;
                            } else {
                                self.leave_protection()?;
                            }
                        } else if self.settings.privacy.active() && self.protection.state == "error"
                        {
                            self.arm_protection(true)?;
                        } else if !self.settings.privacy.active()
                            && !self.store.meta("privacy-personal-context")?.is_null()
                        {
                            self.restore_pending = true;
                        }
                    }
                    _ => anyhow::bail!("invalid_operation"),
                }
                Ok(self.privacy_status())
            }
            "privacy" => {
                match request.operation.as_str() {
                    "preloaded" => self.protection.acknowledge(&request.input, false)?,
                    "frame" => self.protection.acknowledge(&request.input, true)?,
                    "render_error" => {
                        self.protection.acknowledge(&request.input, false)?;
                        self.protection.state = "error";
                        self.protection.phase = "error";
                        self.protection.reason = Some("desktop_render_failed");
                    }
                    _ => anyhow::bail!("invalid_operation"),
                }
                self.ensure_protection()?;
                Ok(self.privacy_status())
            }
            "config" => match request.operation.as_str() {
                "get" => Ok(serde_json::to_value(&self.settings)?),
                "apply" => self.change_settings(
                    request
                        .expected_revision
                        .context("expected_revision_required")?,
                    request.input,
                ),
                _ => anyhow::bail!("invalid_operation"),
            },
            "sources" => Ok(json!({"sources":self.store.sources()?})),
            "next" | "previous" | "refresh" => {
                let id = self.store.enqueue_once(&request.command, &request.id)?;
                Ok(json!({"job_id":id,"accepted":true}))
            }
            "pause" | "resume" => self.change_settings(
                self.settings.revision,
                json!({"paused":request.command=="pause"}),
            ),
            "pin" => {
                let id = self
                    .current
                    .as_ref()
                    .context("no_current_photo")?
                    .asset_id
                    .clone();
                if self.settings.privacy.meeting {
                    ensure!(
                        !self
                            .current
                            .as_ref()
                            .is_some_and(|p| privacy::is_neutral(&self.store, p)),
                        "no_current_photo"
                    );
                    self.store.set_meta("privacy-meeting-pin", &json!(id))?;
                    self.store
                        .set_meta("privacy-meeting-pin-set", &json!(true))?;
                    self.cancel(false);
                    Ok(json!({"pinned":true}))
                } else {
                    self.change_settings(self.settings.revision, json!({"pinned":id}))
                }
            }
            "unpin" => {
                if self.settings.privacy.meeting {
                    self.store.set_meta("privacy-meeting-pin", &Value::Null)?;
                    self.store
                        .set_meta("privacy-meeting-pin-set", &json!(true))?;
                    self.cancel(false);
                    Ok(json!({"pinned":false}))
                } else {
                    self.change_settings(self.settings.revision, json!({"pinned":null}))
                }
            }
            "feedback" => {
                let id = request.input["asset_id"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| self.current.as_ref().map(|p| p.asset_id.clone()))
                    .context("no_current_photo")?;
                let action = request.input["action"]
                    .as_str()
                    .context("invalid_feedback")?;
                if action == "privacy" {
                    use crate::privacy::{
                        policy::PrivacyChoice,
                        store::{AssetScope, PrivacyStore},
                    };
                    let choice: PrivacyChoice =
                        serde_json::from_value(request.input["choice"].clone())
                            .context("invalid_privacy_choice")?;
                    let asset = self.store.asset(&id)?.context("no_current_photo")?;
                    let account = self
                        .store
                        .meta("base-account-id")?
                        .as_str()
                        .context("privacy_account_unavailable")?
                        .to_owned();
                    let scope = AssetScope {
                        account,
                        library: asset.library_id.clone(),
                        asset: id,
                    };
                    let store = PrivacyStore::new(self.store.clone());
                    if choice == PrivacyChoice::Allow {
                        let content = self
                            .current
                            .as_ref()
                            .filter(|p| p.asset_id == scope.asset)
                            .and_then(|p| {
                                store
                                    .content_for_render(&p.render_digest, &scope)
                                    .ok()
                                    .flatten()
                            })
                            .filter(|c| {
                                c.rendition_revision
                                    == privacy::pixel_revision(&asset).unwrap_or_default()
                                    && store.has_evidence(c)
                            })
                            .or(store
                                .content_for_asset(&scope, &privacy::pixel_revision(&asset)?)?)
                            .context("privacy_evidence_unavailable")?;
                        store.set_choice(&content, choice)?;
                    } else {
                        store.set_scope_choice(&scope, choice)?;
                    }
                    self.store.advance_privacy_generation()?;
                    if self.settings.privacy.active() {
                        self.arm_protection(false)?;
                    } else {
                        self.cancel(false);
                    }
                    self.failed.clear();
                    self.retry_at = None;
                    self.refresh_privacy_counts()?;
                    return Ok(json!({"recorded":true}));
                }
                let detail = if action == "period" {
                    Some(json!({"period":request.input["period"]}))
                } else {
                    request.input.get("crop").cloned()
                };
                self.store.feedback(&id, action, detail)?;
                if action != "period" {
                    self.cancel(false);
                    self.reload_pool()?;
                    if action == "crop" {
                        self.ready.retain(|p| p.asset_id != id);
                    }
                    self.prune_ready()?;
                    self.persist_queue()?;
                }
                if action == "never" {
                    self.store.enqueue("next")?;
                }
                Ok(json!({"recorded":true}))
            }
            "auth" | "pair" => {
                ensure!(!self.auth_busy, "apple_operation_pending");
                let net = self.net.as_ref().context("apple_sign_in_unavailable")?;
                let job = network::Job::from_request(&mut request)?;
                net.try_send(job)
                    .map_err(|_| anyhow::anyhow!("apple_operation_pending"))?;
                self.auth_busy = true;
                if request.command == "auth" {
                    if request.operation == "login" || request.operation == "logout" {
                        self.auth_transition = true;
                    }
                    self.cancel(true);
                }
                Ok(json!({"accepted":true}))
            }
            _ => anyhow::bail!("invalid_command"),
        }
    }
    async fn commands(&mut self) -> Result<()> {
        for _ in 0..32 {
            let Some(Command { id, action }) = self.store.claim_command()? else {
                break;
            };
            match action.as_str() {
                "next" => {
                    if self.settings.privacy.meeting {
                        self.store.set_meta("privacy-meeting-pin", &Value::Null)?;
                        self.store
                            .set_meta("privacy-meeting-pin-set", &json!(true))?;
                    } else if self.settings.pinned.is_some() {
                        self.change_settings(self.settings.revision, json!({"pinned":null}))?;
                    }
                    self.want = Some(id);
                }
                "previous" => {
                    let admission = self.admission()?;
                    let mut previous = None;
                    for i in (0..self.history_index.min(self.history.len())).rev() {
                        let p = &self.history[i];
                        if self.pool.contains(&p.asset_id)
                            && self.store.asset(&p.asset_id)?.is_some_and(|a| {
                                a.revision == p.revision || p.provenance == "legacy-mac-render"
                            })
                            && valid_render(p)
                            && (!admission.active || privacy::admit(&self.store, p, &admission)?)
                        {
                            previous = Some((i, p.clone()));
                            break;
                        }
                    }
                    if let Some((index, p)) = previous {
                        self.start_apply(id, p, Some(index));
                    } else {
                        self.store
                            .finish_command(&id, json!({"error":"no_previous_photo"}))?;
                    }
                }
                "refresh" => {
                    self.notify_theme();
                    if let Some(net) = &self.net {
                        if net.try_send(network::Job::Sync(id.clone())).is_err() {
                            self.store
                                .finish_command(&id, json!({"error":"apple_operation_pending"}))?;
                        }
                    } else {
                        self.store
                            .finish_command(&id, json!({"error":"apple_sign_in_required"}))?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn start_apply(&mut self, job: String, prepared: Prepared, history_index: Option<usize>) {
        let admission = match self.admission() {
            Ok(context) => context,
            Err(_) => {
                let _ = self
                    .store
                    .finish_command(&job, json!({"error":"privacy_state_unavailable"}));
                return;
            }
        };
        if admission.active && !privacy::admit(&self.store, &prepared, &admission).unwrap_or(false)
        {
            let _ = self
                .store
                .finish_command(&job, json!({"error":"privacy_filtered"}));
            return;
        }
        if self.applying.is_some() {
            let _ = self
                .store
                .finish_command(&job, json!({"error":"apply_pending"}));
            return;
        }
        if self
            .store
            .write_projection(
                &[
                    (
                        "applying-v2",
                        serde_json::to_value(&prepared).unwrap_or_default(),
                    ),
                    (
                        "ready-v2",
                        serde_json::to_value(&self.ready).unwrap_or_default(),
                    ),
                ],
                None,
            )
            .is_err()
        {
            let _ = self
                .store
                .finish_command(&job, json!({"error":"operation_failed"}));
            return;
        }
        let platform = self.platform.clone();
        let store = self.store.clone();
        let tx = self.tx.clone();
        let epoch = self.epoch;
        let stop = CancellationToken::new();
        self.applying = Some(stop.clone());
        tokio::spawn(async move {
            let start = std::time::Instant::now();
            // Drain an already dispatched public background command. Meeting
            // activation waits for this acknowledgement before its replacement.
            let result = if stop.is_cancelled() {
                Err(anyhow::anyhow!("command_cancelled"))
            } else if admission.active
                && !privacy::admit(&store, &prepared, &admission).unwrap_or(false)
            {
                Err(anyhow::anyhow!("privacy_filtered"))
            } else {
                platform.apply(&prepared).await
            };
            let _ = tx
                .send(Message::Applied {
                    epoch,
                    job,
                    prepared: Box::new(prepared),
                    history_index,
                    elapsed_ms: start.elapsed().as_secs_f64() * 1000.,
                    result,
                })
                .await;
        });
    }
    fn transition(&mut self) -> Result<()> {
        if self.auth_transition
            || self.applying.is_some()
            || self.want.is_none()
            || self.ready.is_empty()
        {
            return Ok(());
        }
        let history = &self.history;
        let index = self
            .ready
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                let rank = |p: &Prepared| {
                    let penalty: f64 = history
                        .iter()
                        .rev()
                        .take(30)
                        .enumerate()
                        .map(|(i, h)| {
                            let weight = 1. / (1. + i as f64 * 0.2);
                            let identity = if h.asset_id == p.asset_id {
                                3. * weight
                            } else if (h.fingerprint ^ p.fingerprint).count_ones() <= 8 {
                                1.8 * weight
                            } else {
                                0.
                            };
                            identity
                                + if p.source_ids.iter().any(|s| h.source_ids.contains(s)) {
                                    0.15 * weight
                                } else {
                                    0.
                                }
                        })
                        .sum();
                    let time_bonus = if let Some(mode) =
                        crate::themes::preference::mode(&self.settings, &self.theme_status)
                    {
                        crate::themes::preference::bonus(
                            &self.store,
                            &p.asset_id,
                            &p.render_digest,
                            mode,
                        )
                        .unwrap_or(0.)
                    } else {
                        0.
                    };
                    p.score + time_bonus - penalty
                };
                rank(a).total_cmp(&rank(b))
            })
            .map(|(i, _)| i)
            .unwrap();
        let prepared = self.ready.remove(index);
        let job = self.want.take().unwrap();
        self.start_apply(job, prepared, None);
        Ok(())
    }
    fn replenish(&mut self) -> Result<()> {
        if self.auth_transition
            || (self.settings.privacy.active() && self.protection.state != "active")
        {
            return Ok(());
        }
        if self.pool.is_empty()
            && let Some(job) = self.want.take()
        {
            self.error = Some("no_eligible_photos".into());
            self.store
                .finish_command(&job, json!({"error":"no_eligible_photos"}))?;
            return Ok(());
        }
        if self.want.is_none() && self.refill_after.is_some_and(|t| t > Utc::now()) {
            return Ok(());
        }
        if self.applying.is_some()
            || self.prep.is_some()
            || self.ready.len() >= 3
            || self.pool.is_empty()
            || self.geometry.0 == 0
            || self.geometry.1 == 0
            || ((self.settings.paused || self.effective_pin().is_some()) && self.want.is_none())
        {
            return Ok(());
        }
        let mut cached = if (self.settings.privacy.active() && self.want.is_some())
            || self.auth.state != "authenticated"
            || self.retry_at.is_some()
        {
            imaging::cached_ids(&self.store, &self.options())?
        } else {
            Vec::new()
        };
        cached.retain(|id| self.pool.contains(id));
        if self.retry_at.is_some_and(|t| t > Utc::now()) && cached.is_empty() {
            return Ok(());
        }
        let ready: HashSet<_> = self.ready.iter().map(|p| p.asset_id.as_str()).collect();
        let current = self.current.as_ref().map(|p| p.asset_id.as_str());
        let now = Utc::now();
        // This is a scheduling hint, never admission. The existing pipeline and
        // apply path still recheck content, policy, choices and generation.
        let cleared: Vec<_> = if self.settings.privacy.active() && self.want.is_some() {
            cached
                .iter()
                .filter(|id| {
                    self.privacy_eligible_ids.contains(*id)
                        && !ready.contains(id.as_str())
                        && current != Some(id.as_str())
                        && !self.failed.get(*id).is_some_and(|until| *until > now)
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let candidates = if !cleared.is_empty() {
            &cleared
        } else if cached.is_empty() {
            &self.pool
        } else {
            &cached
        };
        let mut selected = None;
        if self.refill_count % 3 != 2
            && let Some(mode) = crate::themes::preference::mode(&self.settings, &self.theme_status)
        {
            let eligible = self.pool.iter().cloned().collect();
            let recent: HashSet<_> = self
                .history
                .iter()
                .rev()
                .take(30)
                .map(|p| p.asset_id.as_str())
                .collect();
            let preferred = crate::themes::preference::cached_candidates(
                &self.store,
                &imaging::config_key(&self.options())?,
                &eligible,
                mode,
            )?;
            selected = preferred
                .into_iter()
                .find(|p| {
                    !ready.contains(p.asset_id.as_str())
                        && current != Some(p.asset_id.as_str())
                        && !recent.contains(p.asset_id.as_str())
                        && !self
                            .failed
                            .get(&p.asset_id)
                            .is_some_and(|until| *until > now)
                        && (cached.is_empty() || candidates.contains(&p.asset_id))
                })
                .map(|p| p.asset_id);
        }
        for _ in 0..if selected.is_some() {
            0
        } else {
            candidates.len()
        } {
            let id = &candidates[self.cursor % candidates.len()];
            self.cursor += 1;
            if ready.contains(id.as_str())
                || current == Some(id.as_str()) && candidates.len() > 1
                || self.failed.get(id).is_some_and(|until| *until > now)
            {
                continue;
            }
            selected = Some(id.clone());
            break;
        }
        let Some(id) = selected else {
            return Ok(());
        };
        self.refill_count = self.refill_count.wrapping_add(1);
        let Some(asset) = self.store.asset(&id)? else {
            return Ok(());
        };
        let mut options = self.options();
        let crop: Option<String> = self
            .store
            .connection()?
            .query_row(
                "SELECT crop FROM feedback WHERE asset_id=?1 AND action='crop'",
                [&id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(crop) = crop {
            options.preferred_crop = serde_json::from_str(&crop).ok();
        }
        let stop = CancellationToken::new();
        self.prep = Some(stop.clone());
        let store = self.store.clone();
        let pipeline = self.pipeline.clone();
        let tx = self.tx.clone();
        let epoch = self.epoch;
        tokio::spawn(async move {
            let result = pipeline.prepare(store, asset, options, stop).await;
            let _ = tx.send(Message::Prepared { epoch, id, result }).await;
        });
        Ok(())
    }
    async fn run(mut self, mut rx: mpsc::Receiver<Message>) {
        loop {
            let due =
                if self.settings.paused || self.effective_pin().is_some() || self.want.is_some() {
                    None
                } else {
                    self.schedule.next_due
                };
            let retry = self.failed.values().min().copied();
            let refill = if self.ready.len() < 3
                && !self.pool.is_empty()
                && !self.settings.paused
                && self.effective_pin().is_none()
            {
                self.refill_after
            } else {
                None
            };
            let deadline = [due, retry, self.retry_at, refill, self.next_sync]
                .into_iter()
                .flatten()
                .min();
            let message = tokio::select! {_ =self.stop.cancelled()=>break,m=rx.recv()=>match m{Some(m)=>m,None=>break},_=crate::platform::deadline(deadline)=>Message::Wake};
            let result: Result<()> = async {
                match message {
                    Message::ModelsProgress{job,value}=>{
                        if self.models_job.as_ref().is_some_and(|(id,_)|id==&job) {self.models_status=value;}
                    },
                    Message::ModelsDone{job,result}=>{
                        if self.models_job.as_ref().is_some_and(|(id,_)|id==&job) {
                            self.models_job=None;
                            self.models_status=match result {Ok(())=>json!({"state":"ready"}),Err(e)=>json!({"state":if privacy::models_present(&self.store){"ready"}else{"unavailable"},"error":if e.to_string()=="privacy_install_cancelled"{"cancelled"}else{"privacy_model_install_failed"}})};
                            if self.settings.privacy.active() {self.arm_protection(true)?;}
                        }
                    },
                    Message::PrivacyApplied{transition_id,generation,cover,result}=>{
                        self.applying=None;
                        self.store.set_meta("applying-v2",&Value::Null)?;
                        if transition_id==self.protection.transition_id && generation==self.protection.generation && self.settings.privacy.active() {
                            match result {
                                Ok(())=>{if cover {self.protection.cover_done=true;}else{self.protection.target_done=true;}},
                                Err(_)=>{self.protection.state="error";self.protection.phase="error";self.protection.reason=Some("privacy_replacement_failed");},
                            }
                            self.persist_protection()?;
                        }
                    },
                    Message::Request(request, reply) => {
                        let result = self
                            .request(request)
                            .await
                            .unwrap_or_else(|e| json!({"error":public_error(&e)}));
                        let _ = reply.send(result);
                    }
                    Message::Geometry => {
                        self.notify_theme();
                        let platform = self.platform.clone();
                        let tx = self.tx.clone();
                        tokio::spawn(async move {
                            let result = async {Ok((platform.geometry().await?,platform.output_names().await?))}.await;
                            let _ = tx.send(Message::GeometryResult(result)).await;
                        });
                    }
                    Message::GeometryResult(result) => {
                        let (geometry,outputs) = result?;
                        if outputs!=self.output_names {self.output_names=outputs;if self.settings.privacy.active(){self.arm_protection(true)?;}}
                        if geometry != self.geometry {
                            self.geometry = geometry;
                            self.cancel(true);
                        }
                    }
                    Message::Helper(info) => {
                        let changed = info["changed"] == true || self.store.settings()?.revision!=self.settings.revision;
                        self.helper = info;
                        if changed {
                            self.cancel(false);
                            self.reload_pool()?;
                            self.prune_ready()?;
                            self.persist_queue()?;
                        }
                    }
                    Message::Theme(info)=>{self.theme_status=info;}
                    Message::SyncProgress(progress)=>self.sync_progress=progress,
                    Message::Wake => {
                        let now = Utc::now();
                        self.failed.retain(|_, until| *until > now);
                        if self.retry_at.is_some_and(|t| t <= now) {
                            self.retry_at = None;
                            self.consecutive_failures = 0;
                        }
                        if self.refill_after.is_some_and(|t| t <= now) {
                            self.refill_after = None;
                        }
                        if self.next_sync.is_some_and(|t| t <= now) {
                            self.next_sync = Some(now + Duration::hours(1));
                            if self.auth.state == "authenticated" {
                                self.store.enqueue("refresh")?;
                            } else if ["offline","session_saved","session_expired"].contains(&self.auth.state.as_str()) && let Some(net) = &self.net {
                                let _ = net.try_send(network::Job::resume());
                            }
                        }
                        if self.schedule.due(
                            now,
                            self.settings.paused,
                            self.effective_pin().is_some(),
                        ) {
                            self.store.enqueue("next")?;
                            self.schedule.advance(now, self.settings.interval_seconds);
                            self.persist_schedule()?;
                        }
                    }
                    Message::Prepared { epoch, id, result } => {
                        self.prep = None;
                        if epoch == self.epoch {
                            let result=result.and_then(|prepared| {
                                let admission=self.admission()?;
                                ensure!(!admission.active || privacy::admit(&self.store,&prepared,&admission)?,"privacy_filtered");Ok(prepared)
                            });
                            match result {
                                Ok(mut prepared) => {
                                    let db=self.store.connection()?;
                                    let preferred:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM feedback WHERE asset_id=?1 AND action='prefer')",[&prepared.asset_id],|r|r.get(0))?;
                                    if preferred{prepared.score+=0.3;}
                                    if self.store.asset(&prepared.asset_id)?.is_some_and(|a|a.favorite){prepared.score+=0.2;}
                                    let mut q=db.prepare("SELECT DISTINCT source_id FROM membership WHERE asset_id=?1 AND kind!='library'")?;
                                    prepared.source_ids=q.query_map([&prepared.asset_id],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
                                    self.consecutive_failures = 0;
                                    self.retry_at = None;
                                    self.analysis_jobs += u64::from(!prepared.analysis_cache_hit);
                                    self.render_hits += u64::from(prepared.cache_hit);
                                    self.ready.push(prepared);
                                    self.persist_queue()?;
                                    self.error = None;
                                    self.refresh_privacy_counts()?;
                                }
                                Err(error) => {
                                    self.refresh_privacy_counts()?;
                                    let code = public_error(&error);
                                    if ["privacy_models_unavailable","privacy_screening_failed"].contains(&code) && self.models_job.is_none() {self.models_status=json!({"state":"unavailable","error":code});}
                                    if ["apple_network_error","apple_download_unavailable","apple_session_expired"].contains(&code) {self.auth.state=if code=="apple_session_expired"{"session_expired"}else{"offline"}.into();self.auth.error=Some(code.into());}
                                    self.error = Some(code.into());
                                    self.consecutive_failures += 1;
                                    if code.starts_with("apple_") || self.consecutive_failures >= 8
                                    {
                                        self.retry_at = Some(Utc::now() + Duration::minutes(5));
                                    }
                                    self.failed.insert(id, Utc::now() + Duration::minutes(5));
                                }
                            }
                        }
                    }
                    Message::Applied {
                        epoch,
                        job,
                        prepared,
                        history_index,
                        elapsed_ms,
                        result,
                    } => {
                        self.applying = None;
                        let result=result.and_then(|()| {
                            let admission=self.admission()?;
                            ensure!(!admission.active || (epoch==self.epoch && privacy::admit(&self.store,&prepared,&admission)?),"privacy_filtered");Ok(())
                        });
                        match result {
                            Ok(()) => {
                                // Record what was actually applied even if cancellation raced an acknowledged apply.
                                let mut prepared = *prepared;
                                prepared.applied_at=Some(Utc::now());
                                self.current = Some(prepared.clone());
                                if let Some(index)=history_index {self.history_index=index;}
                                if self.history.get(self.history_index).is_none_or(|p| {
                                    p.asset_id != prepared.asset_id
                                        || p.revision != prepared.revision
                                }) {
                                    self.history.push(prepared);
                                    if self.history.len() > 200 {
                                        self.history.remove(0);
                                    }
                                    self.history_index = self.history.len() - 1;
                                }
                                self.last_transition_ms = Some(elapsed_ms);
                                self.refill_after =
                                    Some(Utc::now() + Duration::seconds(self.quiet_delay));
                                if epoch==self.epoch{
                                    self.schedule.advance(Utc::now(),self.settings.interval_seconds);
                                    self.error=None;
                                }
                                self.store.write_projection(&[
                                    ("current-v2",serde_json::to_value(&self.current)?),
                                    ("history-v2",serde_json::to_value(&self.history)?),
                                    ("history-index-v2",json!(self.history_index)),
                                    ("schedule-v2",serde_json::to_value(&self.schedule)?),
                                    ("applying-v2",Value::Null)
                                ],Some((&job,json!({"applied":true}))))?;
                                if job.starts_with("privacy-restore:") {self.store.set_meta("privacy-personal-context",&Value::Null)?;}
                                self.notify_theme();
                            }
                            Err(error) => {
                                if error.to_string()!="command_cancelled" {self.error=Some(public_error(&error).into());}
                                self.store.write_projection(&[("applying-v2",Value::Null)],Some((&job,json!({"error":public_error(&error)}))))?;
                            }
                        }
                    }
                    Message::Auth { info, busy,scope } => {
                        if busy {self.sync_progress=Value::Null;}
                        if let Some(scope)=scope&& self.store.meta("base-account-id")?!=json!(scope){self.auth_transition=true;}
                        let authenticated = info.state == "authenticated";
                        self.auth = info;
                        self.auth_busy = busy;
                        if authenticated && !busy && auth_sync_needed(self.last_refresh,self.auth_transition,Utc::now()) {
                            self.store.enqueue("refresh")?;
                        }
                    }
                    Message::Synced { job, result } => match result {
                        Ok(()) => {
                            self.sync_progress=Value::Null;
                            self.cancel(false);
                            self.reload_pool()?;
                            self.prune_ready()?;
                            self.persist_queue()?;
                            self.last_refresh = Some(Utc::now());
                            if !self.auth_busy&&self.auth.state=="authenticated"{self.auth_transition=false;}
                            self.error = None;
                            self.store.finish_command(&job, json!({"synced":true}))?;
                        }
                        Err(error) => {
                            self.sync_progress=Value::Null;
                            let code = public_error(&error);
                            self.error = Some(code.into());
                            if ["apple_session_expired","apple_network_error"].contains(&code) {
                                self.auth.state=if code=="apple_session_expired"{"session_expired"}else{"offline"}.into();
                                self.auth.error=Some(code.into());
                            }
                            self.store.finish_command(&job, json!({"error":code}))?;
                        }
                    },
                    Message::Paired { result } => {
                        self.auth_busy = false;
                        match result {
                            Ok(()) => {
                                if let Ok(Some(config))=crate::enrichment::HelperConfig::load(&self.store){self.helper=json!({"url":config.helper_url,"namespace":config.binding.namespace,"stale":false,"last_refresh":Utc::now()});}
                                self.cancel(false);
                                self.reload_pool()?;
                                self.prune_ready()?;
                                self.persist_queue()?;
                                self.error = None;
                            }
                            Err(e) => self.error = Some(public_error(&e).into()),
                        }
                    }
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                self.error = Some(public_error(&error).into());
            }
            if let Err(error) = self.ensure_protection() {
                self.error = Some(public_error(&error).into());
            }
            if (self.current.is_none()
                || (self.settings.privacy.active()
                    && self
                        .current
                        .as_ref()
                        .is_some_and(|p| privacy::is_neutral(&self.store, p))))
                && (!self.settings.privacy.active() || self.protection.state == "active")
                && !self.ready.is_empty()
                && !self.settings.paused
                && self.effective_pin().is_none()
                && self.want.is_none()
                && self.applying.is_none()
            {
                let _ = self.store.enqueue("next");
            }
            if let Err(e) = self
                .commands()
                .await
                .and_then(|_| self.transition())
                .and_then(|_| self.replenish())
            {
                self.error = Some(public_error(&e).into());
            }
            self.publish();
        }
        self.cancel(false);
        self.publish();
    }
}
use rusqlite::OptionalExtension;
fn auth_sync_needed(
    last: Option<DateTime<Utc>>,
    account_changed: bool,
    now: DateTime<Utc>,
) -> bool {
    account_changed || last.is_none_or(|last| now - last >= Duration::hours(1))
}
fn valid_render(p: &Prepared) -> bool {
    use sha2::{Digest, Sha256};
    crate::storage::read_bounded(&p.render_path, 100 * 1024 * 1024)
        .is_ok_and(|b| hex::encode(Sha256::digest(b)) == p.render_digest)
}
pub fn public_error(error: &anyhow::Error) -> &'static str {
    match error.to_string().as_str() {
        "installation_failed_rolled_back" => "installation_failed_rolled_back",
        "installation_failed_rollback_required" => "installation_failed_rollback_required",
        "unowned_installation_path" => "unowned_installation_path",
        "installation_not_owned" => "installation_not_owned",
        "installation_command_failed" => "installation_command_failed",
        "installation_command_unavailable" => "installation_command_unavailable",
        "catalog_session_mismatch" => "catalog_session_mismatch",
        "catalog_already_exists" => "catalog_already_exists",
        "catalog_seed_unavailable" => "catalog_seed_unavailable",
        "settings_conflict" => "settings_conflict",
        "expected_revision_required" => "expected_revision_required",
        "invalid_settings"
        | "invalid_interval"
        | "invalid_cache_budget"
        | "invalid_fallback"
        | "invalid_source"
        | "unknown_settings_field" => "invalid_settings",
        "invalid_theme_mode"
        | "invalid_theme_style"
        | "invalid_location_mode"
        | "invalid_location"
        | "invalid_solar_offsets"
        | "location_required" => "invalid_settings",
        "no_current_photo" => "no_current_photo",
        "no_eligible_photos" => "no_eligible_photos",
        "invalid_feedback" => "invalid_feedback",
        "invalid_command" => "invalid_command",
        "invalid_operation" => "invalid_operation",
        "command_cancelled" => "command_cancelled",
        "request_id_reused" => "request_id_reused",
        "apple_sign_in_required" => "apple_sign_in_required",
        "apple_sign_in_unavailable" => "apple_sign_in_unavailable",
        "apple_operation_pending" => "apple_operation_pending",
        "apple_session_expired" => "apple_session_expired",
        "apple_network_error" => "apple_network_error",
        "apple_download_unavailable" => "apple_download_unavailable",
        "apple_asset_changed" => "apple_asset_changed",
        "apple_catalog_unavailable" => "apple_catalog_unavailable",
        "apple_catalog_incomplete" => "apple_catalog_incomplete",
        "invalid_apple_catalog_count" => "invalid_apple_catalog_count",
        "invalid_apple_dimensions" => "invalid_apple_dimensions",
        "missing_apple_master" => "missing_apple_master",
        "invalid_apple_record" => "invalid_apple_record",
        "repeated_apple_page" => "repeated_apple_page",
        "repeated_apple_continuation" => "repeated_apple_continuation",
        "invalid_apple_catalog" => "invalid_apple_catalog",
        "invalid_apple_album" => "invalid_apple_album",
        "invalid_apple_zones" => "invalid_apple_zones",
        "apple_rate_limited" => "apple_rate_limited",
        "apple_credentials_rejected" => "apple_credentials_rejected",
        "invalid_mfa_code" => "invalid_mfa_code",
        "helper_offline" => "helper_offline",
        "helper_authentication_failed" => "helper_authentication_failed",
        "helper_unavailable" => "helper_unavailable",
        "invalid_helper_token" => "invalid_helper_token",
        "helper_requires_private_tailscale_https" => "helper_requires_private_tailscale_https",
        "enrichment_namespace_mismatch" | "enrichment_account_or_library_mismatch" => {
            "enrichment_identity_mismatch"
        }
        "no_safe_crop" | "insufficient_image_resolution" => "no_suitable_crop",
        "preparation_cancelled" => "preparation_cancelled",
        "installation_in_progress" => "installation_in_progress",
        "privacy_transition_pending" => "privacy_transition_pending",
        "privacy_deactivation_required" => "privacy_deactivation_required",
        "privacy_evidence_unavailable" => "privacy_evidence_unavailable",
        "privacy_filtered" => "privacy_filtered",
        "privacy_models_unavailable" => "privacy_models_unavailable",
        "privacy_screening_failed" => "privacy_screening_failed",
        "unsupported_hdr_rendition" => "unsupported_hdr_rendition",
        "unsupported_image_format" | "invalid_image_data" => "unsupported_image_format",
        "native_heif_security_update_required" => "native_heif_security_update_required",
        "image_security_limit" => "image_security_limit",
        "download_checksum_mismatch" | "download_size_mismatch" | "invalid_downloaded_media" => {
            "invalid_downloaded_media"
        }
        "display_geometry_unavailable" | "no_active_outputs" => "display_geometry_unavailable",
        "render_unavailable" => "render_unavailable",
        "wallpaper_apply_failed" => "wallpaper_apply_failed",
        _ => "operation_failed",
    }
}

#[cfg(test)]
mod sync_tests {
    use super::*;
    #[test]
    fn fresh_same_account_resume_avoids_an_unnecessary_full_catalog_scan() {
        let now = Utc::now();
        assert!(!auth_sync_needed(
            Some(now - Duration::minutes(10)),
            false,
            now
        ));
        assert!(auth_sync_needed(Some(now - Duration::hours(2)), false, now));
        assert!(auth_sync_needed(Some(now), true, now));
        assert!(auth_sync_needed(None, false, now));
    }
}
