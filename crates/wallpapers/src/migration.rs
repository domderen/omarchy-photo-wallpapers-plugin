//! Preserve legacy state without assuming local UUIDs identify cloud assets.
use crate::{
    imaging::{Prepared, Rect},
    model::Settings,
    storage::Store,
};
use anyhow::{Result, ensure};
use rusqlite::{OpenFlags, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
#[derive(Default, Serialize, serde::Deserialize)]
pub struct MigrationReport {
    pub imported: bool,
    pub mapped_assets: usize,
    pub remaining_assets: usize,
    pub pending_crops: usize,
    pub unavailable_sources: usize,
}
fn read(path: &Path, limit: u64) -> Result<Value> {
    ensure!(
        std::fs::metadata(path)?.len() <= limit,
        "legacy_state_too_large"
    );
    Ok(serde_json::from_slice(&crate::storage::read_bounded(
        path, limit,
    )?)?)
}
fn meta(tx: &rusqlite::Transaction<'_>, key: &str) -> Result<Value> {
    let raw: Option<String> = tx
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
        .optional()?;
    Ok(raw
        .map(|v| serde_json::from_str(&v))
        .transpose()?
        .unwrap_or(Value::Null))
}
fn put(tx: &rusqlite::Transaction<'_>, key: &str, value: Value) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO meta VALUES(?1,?2)",
        params![key, value.to_string()],
    )?;
    Ok(())
}
/// Reuse an explicitly selected, completed native sign-in without replacing any
/// catalog, preferences or feedback. Both daemons must be stopped. The receiving
/// daemon validates the session with Apple and builds its own catalog on startup.
pub fn adopt_session(
    source: &crate::storage::Paths,
    target: &crate::storage::Paths,
) -> Result<Value> {
    let _locks = lock_profiles(source, target)?;
    ensure!(
        !target.config.join("apple-session.json").exists(),
        "apple_session_already_exists"
    );
    let path = source.config.join("apple-session.json");
    let session = read(&path, 2 * 1024 * 1024)?;
    ensure!(
        session["stage"] == "authenticated",
        "apple_sign_in_incomplete"
    );
    let client = crate::icloud::auth::ICloudClient::new(source)?;
    ensure!(
        client.info().state == "session_saved",
        "apple_sign_in_incomplete"
    );
    client.dsid()?;
    client.photos_url()?;
    crate::storage::atomic_json(&target.config.join("apple-session.json"), &session)?;
    Ok(json!({"adopted":true,"catalog_resync_required":true,"settings_preserved":true}))
}
fn lock_profiles(
    source: &crate::storage::Paths,
    target: &crate::storage::Paths,
) -> Result<(std::fs::File, Option<std::fs::File>)> {
    let source_lock = crate::platform::lock(source)?;
    let target_lock = if source.runtime.canonicalize().ok() == target.runtime.canonicalize().ok() {
        None
    } else {
        Some(crate::platform::lock(target)?)
    };
    Ok((source_lock, target_lock))
}
/// Copy only a completed same-account cloud base into an empty receiving base.
/// Receiving preferences, feedback, history and media remain under its ownership.
pub fn adopt_catalog(
    source: &crate::storage::Paths,
    target: &crate::storage::Paths,
) -> Result<Value> {
    let _locks = lock_profiles(source, target)?;
    ensure!(
        read(&source.config.join("apple-session.json"), 2 * 1024 * 1024)?["stage"]
            == "authenticated"
            && read(&target.config.join("apple-session.json"), 2 * 1024 * 1024)?["stage"]
                == "authenticated",
        "catalog_session_mismatch"
    );
    let account = crate::icloud::auth::ICloudClient::new(source)?
        .dsid()
        .map_err(|_| anyhow::anyhow!("catalog_session_mismatch"))?;
    ensure!(
        crate::icloud::auth::ICloudClient::new(target)?
            .dsid()
            .ok()
            .as_ref()
            == Some(&account),
        "catalog_session_mismatch"
    );
    let store = Store::open(target)?;
    ensure!(
        store.meta("base-account-id")?.is_null(),
        "catalog_already_exists"
    );
    discover_legacy(&store)?;
    let mut db = store.connection()?;
    let mut uri = reqwest::Url::from_file_path(source.database().canonicalize()?)
        .map_err(|_| anyhow::anyhow!("invalid_profile_path"))?;
    uri.set_query(Some("mode=ro"));
    db.execute("ATTACH DATABASE ?1 AS candidate", [uri.as_str()])?;
    let integrity: String = db.query_row("PRAGMA candidate.quick_check", [], |r| r.get(0))?;
    let version: i64 = db.query_row("PRAGMA candidate.user_version", [], |r| r.get(0))?;
    ensure!(
        integrity == "ok" && matches!(version, 2 | 3),
        "catalog_seed_unavailable"
    );
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let base: String = tx.query_row(
        "SELECT value FROM candidate.meta WHERE key='base-account-id'",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        serde_json::from_str::<String>(&base)? == account,
        "catalog_session_mismatch"
    );
    let prefix = format!("icloud:{account}:");
    let foreign: i64 = tx.query_row(
        "SELECT count(*) FROM candidate.assets WHERE substr(library_id,1,length(?1))!=?1",
        [prefix],
        |r| r.get(0),
    )?;
    let current: i64 = tx.query_row("SELECT count(*) FROM assets", [], |r| r.get(0))?;
    ensure!(foreign == 0 && current == 0, "catalog_seed_unavailable");
    let complete:i64=tx.query_row("SELECT count(*) FROM candidate.meta WHERE key IN ('catalog_generation','last_sync') AND value!='null'",[],|r|r.get(0))?;
    ensure!(complete == 2, "catalog_seed_unavailable");
    let assets = tx.execute("INSERT INTO assets SELECT * FROM candidate.assets", [])?;
    ensure!(assets <= 1_000_000, "catalog_seed_unavailable");
    tx.execute(
        "INSERT OR REPLACE INTO sources SELECT * FROM candidate.sources WHERE provider='icloud'",
        [],
    )?;
    tx.execute("INSERT OR REPLACE INTO membership SELECT * FROM candidate.membership WHERE provider='icloud' AND asset_id IN (SELECT id FROM assets)",[])?;
    tx.execute("INSERT OR REPLACE INTO meta SELECT * FROM candidate.meta WHERE key IN ('base-account-id','catalog_generation','last_sync')",[])?;
    put(&tx, "validated_catalog_handoff", json!(true))?;
    tx.commit()?;
    Ok(json!({"adopted":true,"assets":assets,"settings_preserved":true,"feedback_preserved":true}))
}
pub fn discover_legacy(store: &Store) -> Result<MigrationReport> {
    if store.meta("legacy_imported")? == true || !store.paths.config.join("settings.json").exists()
    {
        return Ok(MigrationReport::default());
    }
    let settings: Settings =
        serde_json::from_value(read(&store.paths.config.join("settings.json"), 65536)?)?;
    settings.validate()?;
    let engine_path = store.paths.data.join("engine.json");
    let engine = if engine_path.exists() {
        read(&engine_path, 8 * 1024 * 1024)?
    } else {
        json!({})
    };
    let mut feedback = Vec::new();
    let path = store.paths.data.join("feedback.sqlite");
    if path.exists() {
        let db = rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut query = db.prepare("SELECT asset_id,action,crop FROM feedback LIMIT 100001")?;
        for row in query.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })? {
            let (id, action, crop) = row?;
            ensure!(
                ["never", "prefer", "crop"].contains(&action.as_str()),
                "invalid_legacy_feedback"
            );
            feedback.push(json!({"asset_id":id,"action":action,"crop":crop.and_then(|v|serde_json::from_str::<Value>(&v).ok())}));
        }
        ensure!(feedback.len() <= 100000, "legacy_feedback_too_large");
    }
    let catalogs = store.paths.data.join("catalogs");
    let namespace = (|| -> Result<Value> {
        let active = read(&catalogs.join("active.json"), 65536)?;
        let generation = active["generation"].as_str().unwrap_or("");
        ensure!(
            generation.len() == 64 && generation.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid_legacy_catalog"
        );
        let db = rusqlite::Connection::open_with_flags(
            catalogs.join(format!("{generation}.sqlite")),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let value: String = db.query_row(
            "SELECT value FROM catalog_metadata WHERE key='namespace'",
            [],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&value)?)
    })()
    .unwrap_or(Value::Null);
    let schedule_path = store.paths.data.join("schedule.json");
    let schedule = if schedule_path.exists() {
        read(&schedule_path, 65536).ok()
    } else {
        None
    };
    let mut db = store.connection()?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if meta(&tx, "legacy_imported")? == true {
        return Ok(MigrationReport::default());
    }
    for (key, value) in [
        ("settings", serde_json::to_value(&settings)?),
        ("legacy_engine", engine),
        ("legacy_namespace", namespace),
        ("legacy_pending_feedback", json!(feedback)),
        ("legacy_pending_crops", json!([])),
        ("legacy_mapped_ids", json!({})),
        ("legacy_imported", json!(true)),
    ] {
        put(&tx, key, value)?;
    }
    if let Some(schedule) = schedule
        && schedule["next_due"]
            .as_str()
            .is_some_and(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok())
    {
        put(&tx, "schedule-v2", json!({"next_due":schedule["next_due"]}))?;
    }
    tx.commit()?;
    Ok(MigrationReport {
        imported: true,
        remaining_assets: feedback.len(),
        ..MigrationReport::default()
    })
}
pub fn resolve_legacy(store: &Store) -> Result<MigrationReport> {
    if store.meta("legacy_imported")? != true {
        return Ok(MigrationReport::default());
    }
    let mut db = store.connection()?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let namespace = meta(&tx, "legacy_namespace")?;
    let Some(namespace) = namespace.as_str() else {
        return Ok(MigrationReport::default());
    };
    let provider = format!("mac:{namespace}");
    let raw: Option<String> = tx
        .query_row(
            "SELECT data FROM enrichment WHERE provider=?1",
            [&provider],
            |r| r.get(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(MigrationReport::default());
    };
    let overlay: Value = serde_json::from_str(&raw)?;
    let version = json!([
        meta(&tx, &format!("enrichment-key:{provider}"))?,
        meta(&tx, "catalog_generation")?
    ]);
    if meta(&tx, "legacy_resolution_version")? == version {
        let report: MigrationReport = serde_json::from_value(meta(&tx, "migration_report")?)?;
        return Ok(report);
    }
    if overlay["binding"]["account_id"] != meta(&tx, "base-account-id")? {
        return Ok(MigrationReport::default());
    }
    let mut mappings: HashMap<String, String> = serde_json::from_value(overlay["matches"].clone())?;
    let existing: HashSet<String> = tx
        .prepare("SELECT id FROM assets")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    mappings.retain(|_, v| existing.contains(v));
    let mut settings: Settings = serde_json::from_value(meta(&tx, "settings")?)?;
    let previous = serde_json::to_value(&settings)?;
    let mut available_sources = HashSet::new();
    for row in tx
        .prepare("SELECT kind,id FROM sources WHERE provider=?1")?
        .query_map([&provider], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
    {
        available_sources.insert(row?);
    }
    let mut unavailable = 0;
    for source in &mut settings.sources {
        if source.id.starts_with("mac:")
            || source.id.starts_with("icloud:")
            || source.kind == "library"
        {
            continue;
        }
        let id = if source.kind == "person" {
            format!("{provider}:{}", source.id)
        } else {
            format!("{provider}:collection:{}", source.id)
        };
        if available_sources.contains(&(source.kind.clone(), id.clone())) {
            source.id = id;
        } else {
            unavailable += 1;
        }
    }
    if let Some(pin) = &settings.pinned
        && let Some(mapped) = mappings.get(pin)
    {
        settings.pinned = Some(mapped.clone());
    }
    if serde_json::to_value(&settings)? != previous {
        settings.revision += 1;
        settings.validate()?;
        put(&tx, "settings", serde_json::to_value(&settings)?)?;
    }
    let pending: Vec<Value> =
        serde_json::from_value(meta(&tx, "legacy_pending_feedback")?).unwrap_or_default();
    let mut remaining = Vec::new();
    let mut crops: Vec<Value> =
        serde_json::from_value(meta(&tx, "legacy_pending_crops")?).unwrap_or_default();
    for entry in pending {
        let Some(id) = entry["asset_id"].as_str().and_then(|id| mappings.get(id)) else {
            remaining.push(entry);
            continue;
        };
        if entry["action"] == "crop" {
            let mut crop = entry;
            crop["cloud_asset_id"] = json!(id);
            crop["reason"] = json!("legacy_rendition_geometry_unverified");
            crops.push(crop);
        } else {
            tx.execute(
                "INSERT OR IGNORE INTO feedback VALUES(?1,?2,NULL)",
                params![id, entry["action"].as_str().unwrap_or("")],
            )?;
        }
    }
    let mut mapped: HashMap<String, String> =
        serde_json::from_value(meta(&tx, "legacy_mapped_ids")?).unwrap_or_default();
    mapped.extend(mappings.clone());
    put(&tx, "legacy_mapped_ids", json!(mapped))?;
    put(&tx, "legacy_pending_feedback", json!(remaining))?;
    put(&tx, "legacy_pending_crops", json!(crops))?;
    let engine = meta(&tx, "legacy_engine")?;
    let mut history: Vec<Prepared> =
        serde_json::from_value(meta(&tx, "history-v2")?).unwrap_or_default();
    for item in engine["history"].as_array().into_iter().flatten() {
        if let Some(prepared) = legacy_render(store, item, &mappings)?
            && !history.iter().any(|p| {
                p.asset_id == prepared.asset_id && p.render_digest == prepared.render_digest
            })
        {
            history.push(prepared);
        }
    }
    history.sort_by_key(|p| p.applied_at);
    if history.len() > 200 {
        history.drain(..history.len() - 200);
    }
    if let Some(current) = legacy_render(store, &engine["current"], &mappings)?
        && meta(&tx, "legacy_current_migrated")? != true
    {
        if meta(&tx, "current-v2")?.is_null() {
            let index = history
                .iter()
                .position(|p| {
                    p.asset_id == current.asset_id && p.render_digest == current.render_digest
                })
                .unwrap_or_else(|| {
                    history.push(current.clone());
                    history.len() - 1
                });
            put(&tx, "current-v2", serde_json::to_value(&current)?)?;
            put(&tx, "history-index-v2", json!(index))?;
        }
        put(&tx, "legacy_current_migrated", json!(true))?;
    }
    if !history.is_empty() {
        put(&tx, "history-v2", serde_json::to_value(&history)?)?;
    }
    put(&tx, "legacy_resolution_version", version)?;
    let report = MigrationReport {
        imported: false,
        mapped_assets: mappings.len(),
        remaining_assets: remaining.len(),
        pending_crops: crops.len(),
        unavailable_sources: unavailable,
    };
    put(&tx, "migration_report", serde_json::to_value(&report)?)?;
    tx.commit()?;
    Ok(report)
}
fn legacy_render(
    store: &Store,
    value: &Value,
    mapping: &HashMap<String, String>,
) -> Result<Option<Prepared>> {
    let Some(id) = value["asset_id"].as_str().and_then(|id| mapping.get(id)) else {
        return Ok(None);
    };
    let Some(path) = value["render_path"].as_str() else {
        return Ok(None);
    };
    let path = std::path::PathBuf::from(path);
    let Ok(canonical) = path.canonicalize() else {
        return Ok(None);
    };
    if !canonical.starts_with(store.paths.data.canonicalize()?) {
        return Ok(None);
    }
    let Ok((width, height)) = image::image_dimensions(&canonical) else {
        return Ok(None);
    };
    ensure!(
        std::fs::metadata(&canonical)?.is_file()
            && std::fs::metadata(&canonical)?.len() <= 100 * 1024 * 1024,
        "legacy_render_too_large"
    );
    let bytes = crate::storage::read_bounded(&canonical, 100 * 1024 * 1024)?;
    use sha2::{Digest, Sha256};
    let source_ids = value["source_ids"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(Prepared {
        asset_id: id.clone(),
        revision: value["revision"]
            .as_str()
            .unwrap_or("unknown-legacy-revision")
            .into(),
        render_path: canonical,
        render_digest: hex::encode(Sha256::digest(bytes)),
        config_key: "legacy-history-only".into(),
        rect: Rect {
            x: 0,
            y: 0,
            width,
            height,
        },
        mode: "fit".into(),
        fingerprint: value["fingerprint"]
            .as_str()
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .unwrap_or(0),
        score: 0.,
        source_width: width,
        source_height: height,
        width,
        height,
        rendition: "legacy_cached".into(),
        provenance: "legacy-mac-render".into(),
        source_ids,
        applied_at: value["applied_at"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&chrono::Utc)),
        decode_count: 0,
        cache_hit: true,
        analysis_cache_hit: true,
    }))
}
