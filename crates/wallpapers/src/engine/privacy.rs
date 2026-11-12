//! Admission is independent of render/cache creation and is rechecked at apply.
use crate::{
    imaging::Prepared,
    model::Asset,
    privacy::{
        policy::{PrivacyChoice, ScreeningState},
        store::{AssetScope, DecisionKey, PrivacyStore},
    },
    storage::{Store, read_bounded},
};
use anyhow::Result;
use rusqlite::params;
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct AdmissionContext {
    pub generation: u64,
    pub policy_key: Option<DecisionKey>,
    pub active: bool,
}
impl AdmissionContext {
    pub fn from_store(store: &Store, policy_key: Option<DecisionKey>) -> Result<Self> {
        Ok(Self {
            generation: store.meta("privacy-generation")?.as_u64().unwrap_or(0),
            policy_key,
            active: store.settings()?.privacy.active(),
        })
    }
}
pub fn pixel_revision(asset: &Asset) -> Result<String> {
    let checksum = asset
        .resources
        .get(&asset.rendition)
        .map(|r| r.checksum.as_str())
        .filter(|s| !s.is_empty());
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            asset.rendition.as_str(),
            asset.width,
            asset.height,
            asset.orientation,
            checksum,
            checksum.is_none().then_some(asset.revision.as_str())
        ))?)
    ))
}
pub fn admit(store: &Store, prepared: &Prepared, context: &AdmissionContext) -> Result<bool> {
    if store.meta("privacy-generation")?.as_u64().unwrap_or(0) != context.generation {
        return Ok(false);
    }
    if is_neutral(store, prepared) {
        return Ok(true);
    }
    let Some(asset) = store.asset(&prepared.asset_id)? else {
        return Ok(false);
    };
    if asset.hidden || asset.deleted {
        return Ok(false);
    }
    let settings = store.settings()?;
    let all = settings
        .sources
        .iter()
        .any(|s| s.kind == "library" && s.id == "all");
    let allowed:bool=store.connection()?.query_row("SELECT NOT EXISTS(SELECT 1 FROM feedback WHERE asset_id=?1 AND action='never') AND (?2 OR EXISTS(SELECT 1 FROM membership m JOIN json_each(?3) s ON m.kind=json_extract(s.value,'$.kind') AND m.source_id=json_extract(s.value,'$.id') WHERE m.asset_id=?1))",params![prepared.asset_id,all,serde_json::to_string(&settings.sources)?],|r|r.get(0))?;
    if !allowed {
        return Ok(false);
    }
    if !context.active {
        return Ok(true);
    }
    let Some(account) = store.meta("base-account-id")?.as_str().map(str::to_owned) else {
        return Ok(false);
    };
    let scope = AssetScope {
        account,
        library: asset.library_id.clone(),
        asset: asset.id.clone(),
    };
    let privacy = PrivacyStore::new(store.clone());
    let Some(content) = privacy.content_for_render(&prepared.render_digest, &scope)? else {
        return Ok(false);
    };
    if content.rendition != asset.rendition
        || content.rendition_revision != pixel_revision(&asset)?
        || !privacy.has_evidence(&content)
    {
        return Ok(false);
    }
    if !read_bounded(&prepared.render_path, 100 * 1024 * 1024)
        .is_ok_and(|b| format!("{:x}", Sha256::digest(&b)) == prepared.render_digest)
    {
        return Ok(false);
    }
    match privacy.choice_for(&content)? {
        PrivacyChoice::PersonalOnly => Ok(false),
        PrivacyChoice::Allow => Ok(true),
        PrivacyChoice::Auto => match &context.policy_key {
            Some(key) => Ok(privacy
                .lookup(&content, key)?
                .is_some_and(|d| d.state == ScreeningState::Clear)),
            None => Ok(false),
        },
    }
}

pub fn policy_key(store: &Store) -> Option<DecisionKey> {
    let key = serde_json::from_value::<DecisionKey>(store.meta("privacy-policy-v1").ok()?).ok()?;
    key.validate().ok()?;
    Some(key)
}
pub fn screening_worker(
    store: &Store,
    key: &DecisionKey,
) -> Result<crate::privacy::worker::ScreeningWorker> {
    use anyhow::{Context, ensure};
    let parent = store.paths.data.join("privacy/models");
    let active = parent.join("active.json");
    crate::storage::validate_private_parents(&active)?;
    let manifest: crate::privacy::models::ModelManifest = serde_json::from_slice(
        &read_bounded(&active, 512 * 1024).context("privacy_models_unavailable")?,
    )?;
    manifest.validate()?;
    ensure!(
        manifest.qualified
            && manifest.identity()? == key.modelset_sha256
            && manifest.preprocessing_version == key.preprocessing_version
            && manifest.policy_version == key.policy_version,
        "privacy_models_unavailable"
    );
    let root = parent.join(&key.modelset_sha256);
    // A cheap presence check is not artifact verification. The worker still
    // checks every byte/hash before loading; idle status never reads 300 MB.
    for artifact in
        std::iter::once(&manifest.runtime).chain(manifest.models.iter().map(|m| &m.artifact))
    {
        let path = root.join(&artifact.path);
        crate::storage::validate_private_parents(&path)?;
        let file = crate::storage::private_file(&path, rustix::fs::OFlags::RDONLY)?;
        ensure!(
            file.metadata()?.len() == artifact.bytes,
            "privacy_models_unavailable"
        );
    }
    Ok(crate::privacy::worker::ScreeningWorker::new(
        std::env::current_exe()?,
        root,
        manifest,
        std::time::Duration::from_secs(30),
    ))
}
pub fn full_frame_preview(
    store: &Store,
    asset: &Asset,
    path: &std::path::Path,
) -> Result<(
    crate::privacy::store::ContentKey,
    image::RgbImage,
    crate::privacy::worker::PreviewJob,
)> {
    use crate::privacy::{store::ContentKey, worker::PreviewJob};
    use anyhow::Context;
    let bytes = read_bounded(path, 150 * 1024 * 1024)?;
    let media_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let rgb = crate::imaging::decode::decode(&bytes, asset.orientation)?;
    let thumb = image::DynamicImage::ImageRgb8(image::imageops::thumbnail(&rgb, 512, 512));
    let mut output = std::io::Cursor::new(Vec::new());
    thumb.write_to(&mut output, image::ImageFormat::Png)?;
    let png = output.into_inner();
    let privacy = PrivacyStore::new(store.clone());
    let preview_sha256 = privacy.save_preview(&png)?;
    let scope = AssetScope {
        account: store
            .meta("base-account-id")?
            .as_str()
            .context("privacy_account_unavailable")?
            .into(),
        library: asset.library_id.clone(),
        asset: asset.id.clone(),
    };
    let content = ContentKey {
        scope,
        rendition: asset.rendition.clone(),
        rendition_revision: pixel_revision(asset)?,
        media_sha256,
        preview_sha256,
    };
    content.validate()?;
    Ok((content, rgb, PreviewJob::from_png(png)))
}

fn neutral_bytes(mode: &str) -> Result<Vec<u8>> {
    anyhow::ensure!(matches!(mode, "light" | "dark"), "invalid_neutral_mode");
    let color = if mode == "light" {
        [227, 233, 239]
    } else {
        [30, 37, 45]
    };
    let mut output = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(16, 16, image::Rgb(color))
        .write_to(&mut output, image::ImageFormat::Png)?;
    Ok(output.into_inner())
}
pub fn neutral(store: &Store, generation: u64, mode: &str) -> Result<Prepared> {
    let bytes = neutral_bytes(mode)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let path = store
        .paths
        .data
        .join(format!("privacy/neutral/neutral-{generation}-{mode}.png"));
    crate::storage::atomic_bytes(&path, &bytes)?;
    Ok(Prepared {
        asset_id: format!("builtin:meeting-neutral:{generation}:{mode}"),
        revision: "neutral-v1".into(),
        render_path: path,
        render_digest: hash,
        config_key: "meeting-neutral-v1".into(),
        rect: crate::imaging::Rect {
            x: 0,
            y: 0,
            width: 16,
            height: 16,
        },
        mode: "fit".into(),
        fingerprint: 0,
        score: 0.,
        source_ids: vec![],
        source_width: 16,
        source_height: 16,
        width: 16,
        height: 16,
        rendition: mode.into(),
        provenance: "meeting-neutral".into(),
        applied_at: None,
        decode_count: 0,
        cache_hit: true,
        analysis_cache_hit: true,
    })
}
pub fn is_neutral(store: &Store, prepared: &Prepared) -> bool {
    let Some(rest) = prepared.asset_id.strip_prefix("builtin:meeting-neutral:") else {
        return false;
    };
    let Some((generation, mode)) = rest.split_once(':') else {
        return false;
    };
    let Ok(generation) = generation.parse::<u64>() else {
        return false;
    };
    let Ok(bytes) = neutral_bytes(mode) else {
        return false;
    };
    let expected = store
        .paths
        .data
        .join(format!("privacy/neutral/neutral-{generation}-{mode}.png"));
    prepared.provenance == "meeting-neutral"
        && prepared.render_path == expected
        && prepared.render_digest == format!("{:x}", Sha256::digest(&bytes))
        && read_bounded(&expected, 16 * 1024).is_ok_and(|data| data == bytes)
}

#[derive(Default)]
pub(crate) struct Session {
    pub generation: u64,
    pub transition_id: String,
    pub state: &'static str,
    pub phase: &'static str,
    pub reason: Option<&'static str>,
    pub target: Option<Prepared>,
    pub cover: Option<Prepared>,
    pub outputs: Vec<String>,
    pub preloaded: std::collections::HashSet<String>,
    pub frames: std::collections::HashSet<String>,
    pub cover_started: bool,
    pub cover_done: bool,
    pub target_started: bool,
    pub target_done: bool,
}
impl Session {
    pub fn off() -> Self {
        Self {
            state: "off",
            phase: "off",
            ..Default::default()
        }
    }
    pub fn arm(generation: u64, target: Prepared, cover: Prepared, outputs: Vec<String>) -> Self {
        Self {
            generation,
            transition_id: uuid::Uuid::new_v4().to_string(),
            state: "arming",
            phase: "preload",
            reason: Some("waiting_for_desktop"),
            target: Some(target),
            cover: Some(cover),
            outputs,
            ..Default::default()
        }
    }
    pub fn status(&self, store: &Store) -> serde_json::Value {
        let settings = store.settings().ok();
        serde_json::json!({"requested":settings.as_ref().is_some_and(|s|s.privacy.meeting),"protected":settings.as_ref().is_some_and(|s|s.privacy.active()),"filter_personal":settings.as_ref().is_some_and(|s|s.privacy.filter_personal),"state":self.state,"phase":self.phase,"generation":self.generation,"transition_id":self.transition_id,"reason":self.reason,"pending_path":self.target.as_ref().map(|p|&p.render_path),"pending_digest":self.target.as_ref().map(|p|&p.render_digest),"outputs":self.outputs,"preloaded_outputs":self.preloaded,"framed_outputs":self.frames,"model_state":if policy_key(store).is_some(){"ready"}else{"unavailable"}})
    }
    pub fn acknowledge(&mut self, input: &serde_json::Value, frame: bool) -> Result<()> {
        use anyhow::{Context, ensure};
        ensure!(
            self.state != "off"
                && input["generation"].as_u64() == Some(self.generation)
                && input["transition_id"].as_str() == Some(self.transition_id.as_str()),
            "stale_privacy_confirmation"
        );
        let output = input["output"]
            .as_str()
            .context("invalid_privacy_confirmation")?;
        ensure!(
            self.outputs.iter().any(|o| o == output),
            "unknown_privacy_output"
        );
        if frame {
            ensure!(
                self.target_done && self.phase == "frame",
                "privacy_frame_not_ready"
            );
            self.frames.insert(output.into());
        } else {
            ensure!(self.state != "error", "privacy_replacement_failed");
            self.preloaded.insert(output.into());
        }
        Ok(())
    }
    pub fn all_preloaded(&self) -> bool {
        !self.outputs.is_empty() && self.outputs.iter().all(|o| self.preloaded.contains(o))
    }
    pub fn all_framed(&self) -> bool {
        !self.outputs.is_empty() && self.outputs.iter().all(|o| self.frames.contains(o))
    }
}

pub fn current_choice(store: &Store, prepared: &Prepared) -> Result<(PrivacyChoice, bool)> {
    let Some(asset) = store.asset(&prepared.asset_id)? else {
        return Ok((PrivacyChoice::Auto, false));
    };
    let account = store
        .meta("base-account-id")?
        .as_str()
        .unwrap_or("")
        .to_owned();
    if account.is_empty() {
        return Ok((PrivacyChoice::Auto, false));
    }
    let scope = AssetScope {
        account,
        library: asset.library_id.clone(),
        asset: asset.id.clone(),
    };
    let privacy = PrivacyStore::new(store.clone());
    let content = privacy
        .content_for_render(&prepared.render_digest, &scope)?
        .filter(|c| {
            c.rendition_revision == pixel_revision(&asset).unwrap_or_default()
                && privacy.has_evidence(c)
        })
        .or(privacy.content_for_asset(&scope, &pixel_revision(&asset)?)?);
    if let Some(content) = content {
        Ok((privacy.choice_for(&content)?, true))
    } else {
        Ok((privacy.scope_choice(&scope)?, false))
    }
}

pub fn neutral_mode<'a>(
    settings: &crate::model::Settings,
    status: &'a serde_json::Value,
) -> &'a str {
    match settings.theme.mode.as_str() {
        "light" if settings.match_colors => "light",
        "dark" if settings.match_colors => "dark",
        _ => status["mode"]
            .as_str()
            .filter(|mode| matches!(*mode, "light" | "dark"))
            .unwrap_or("dark"),
    }
}

pub fn models_present(store: &Store) -> bool {
    policy_key(store).is_some_and(|key| screening_worker(store, &key).is_ok())
}
