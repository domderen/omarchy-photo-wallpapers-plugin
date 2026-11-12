//! Optional Mac metadata overlay. No media delivery or filename-based identity joins.
use crate::{model::Source, storage::Store};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
#[derive(Clone, Deserialize, Serialize)]
pub struct Person {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Mapping {
    pub local_id: String,
    pub cloud_master_guid: Option<String>,
    pub owner_hash: Option<String>,
    pub person_ids: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub collection_ids: Vec<String>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct MetadataCollection {
    pub id: String,
    pub kind: String,
    pub name: String,
    #[serde(default = "yes")]
    pub available: bool,
}
fn yes() -> bool {
    true
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Enrichment {
    pub schema_version: u32,
    pub provider_namespace: String,
    pub cloud_id_type: String,
    pub created_at: String,
    pub people: Vec<Person>,
    #[serde(default)]
    pub collections: Vec<MetadataCollection>,
    pub assets: Vec<Mapping>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Binding {
    pub namespace: String,
    pub account_id: String,
    pub library_id: String,
    #[serde(default)]
    pub owned_library_ids: Vec<String>,
}
fn owned_library(account: &str, library: &str) -> bool {
    let parts: Vec<_> = library.splitn(5, ':').collect();
    parts.len() == 5
        && parts[0] == "icloud"
        && parts[1] == account
        && parts[2] == "private"
        && (parts[3] == "PrimarySync" || parts[3].starts_with("SharedSync-"))
}
impl Binding {
    pub fn for_account(store: &Store, account: &str, namespace: &str) -> Result<Self> {
        let prefix = format!("icloud:{account}:private:");
        let db = store.connection()?;
        let mut query = db.prepare(
            "SELECT DISTINCT library_id FROM assets WHERE substr(library_id,1,length(?1))=?1 ORDER BY library_id",
        )?;
        let libraries = query
            .query_map([&prefix], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let libraries: Vec<_> = libraries
            .into_iter()
            .filter(|id| owned_library(account, id))
            .collect();
        let anchor = libraries
            .iter()
            .find(|id| id.starts_with(&format!("{prefix}PrimarySync:")))
            .or_else(|| libraries.first())
            .context("icloud_primary_library_unavailable")?;
        Ok(Self {
            namespace: namespace.into(),
            account_id: account.into(),
            library_id: anchor.clone(),
            owned_library_ids: libraries,
        })
    }
}
#[derive(Serialize, Deserialize)]
pub struct ImportReport {
    pub changed: bool,
    pub matched: usize,
    pub unmatched: usize,
    pub ambiguous: usize,
}
pub fn import(store: &Store, doc: &Enrichment, binding: &Binding) -> Result<ImportReport> {
    ensure!(
        doc.schema_version == 1 && doc.cloud_id_type == "CPLMaster.recordName",
        "unsupported_enrichment_schema"
    );
    ensure!(
        !doc.provider_namespace.is_empty()
            && doc.provider_namespace.len() <= 256
            && doc.provider_namespace == binding.namespace,
        "enrichment_namespace_mismatch"
    );
    // Only zones returned in the account's private scope are owned by that user.
    // Join unique master IDs across those explicitly bound zones, never other owners.
    // Shared-owner hashes have no verified CloudKit owner mapping yet and are excluded.
    ensure!(
        owned_library(&binding.account_id, &binding.library_id)
            && binding.owned_library_ids.len() <= 1000
            && binding
                .owned_library_ids
                .iter()
                .all(|id| owned_library(&binding.account_id, id)),
        "enrichment_account_or_library_mismatch"
    );
    ensure!(
        doc.assets.len() <= 1_000_000
            && doc.people.len() <= 100_000
            && doc.collections.len() <= 100_000,
        "enrichment_size_limit"
    );
    chrono::DateTime::parse_from_rfc3339(&doc.created_at)
        .context("invalid_enrichment_timestamp")?;
    let mut people = HashSet::new();
    for person in &doc.people {
        ensure!(
            !person.id.is_empty()
                && person.id.len() <= 256
                && person.name.len() <= 1024
                && people.insert(person.id.clone()),
            "invalid_enrichment_people"
        );
    }
    let mut collections = HashMap::new();
    for c in &doc.collections {
        ensure!(
            ["album", "shared_album"].contains(&c.kind.as_str())
                && !c.id.is_empty()
                && c.id.len() <= 256
                && c.name.len() <= 1024
                && collections.insert(c.id.as_str(), c).is_none(),
            "invalid_enrichment_collections"
        );
    }
    let mut db = store.connection()?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let account: Option<String> = tx
        .query_row(
            "SELECT value FROM meta WHERE key='base-account-id'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(account) = account {
        ensure!(
            serde_json::from_str::<String>(&account)? == binding.account_id,
            "enrichment_account_or_library_mismatch"
        );
    }
    let provider = format!("mac:{}", doc.provider_namespace);
    let mut canonical = serde_json::to_value(doc)?;
    canonical.as_object_mut().unwrap().remove("created_at");
    let base: Option<String> = tx
        .query_row(
            "SELECT value FROM meta WHERE key='catalog_generation'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    use sha2::{Digest, Sha256};
    let content_key = serde_json::to_string(&hex::encode(Sha256::digest(serde_json::to_vec(&(
        canonical, binding, base,
    ))?)))?;
    let key_name = format!("enrichment-key:{provider}");
    let report_name = format!("enrichment-report:{provider}");
    let previous: Option<String> = tx
        .query_row("SELECT value FROM meta WHERE key=?1", [&key_name], |r| {
            r.get(0)
        })
        .optional()?;
    if previous.as_deref() == Some(content_key.as_str()) {
        let raw: String =
            tx.query_row("SELECT value FROM meta WHERE key=?1", [&report_name], |r| {
                r.get(0)
            })?;
        let mut report: ImportReport = serde_json::from_str(&raw)?;
        report.changed = false;
        tx.execute(
            "UPDATE enrichment SET updated=?1 WHERE provider=?2",
            params![doc.created_at, provider],
        )?;
        tx.execute("UPDATE sources SET data=json_set(data,'$.stale',json('false')) WHERE provider=?1 AND json_extract(data,'$.stale')=1",[&provider])?;
        tx.commit()?;
        return Ok(report);
    }
    let mut libraries = binding.owned_library_ids.clone();
    libraries.push(binding.library_id.clone());
    libraries.sort();
    libraries.dedup();
    let mut statement = tx.prepare(
        "SELECT id,json_extract(data,'$.cloud_master_guid') FROM assets WHERE library_id IN (SELECT value FROM json_each(?1))",
    )?;
    let base = statement
        .query_map([serde_json::to_string(&libraries)?], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let mut by_master: HashMap<String, Vec<String>> = HashMap::new();
    for (id, master) in base {
        if !master.is_empty() {
            by_master.entry(master).or_default().push(id);
        }
    }
    let mut local_counts = HashMap::new();
    let mut local_ids = HashSet::new();
    for item in &doc.assets {
        ensure!(
            local_ids.insert(&item.local_id) && item.local_id.len() <= 256,
            "duplicate_local_enrichment_id"
        );
        if let Some(guid) = &item.cloud_master_guid {
            ensure!(guid.len() <= 512, "invalid_cloud_master_guid");
            *local_counts.entry(guid.as_str()).or_insert(0) += 1;
        }
    }
    tx.execute("DELETE FROM sources WHERE provider=?1", [&provider])?;
    tx.execute("DELETE FROM membership WHERE provider=?1", [&provider])?;
    for person in &doc.people {
        let row = Source {
            id: format!("{provider}:{}", person.id),
            kind: "person".into(),
            name: person.name.clone(),
            provider: provider.clone(),
            ..Source::default()
        };
        tx.execute(
            "INSERT INTO sources VALUES(?1,?2,?3,?4)",
            params![row.kind, row.id, serde_json::to_string(&row)?, provider],
        )?;
    }
    for collection in &doc.collections {
        let source = Source {
            id: format!("{provider}:collection:{}", collection.id),
            kind: collection.kind.clone(),
            name: collection.name.clone(),
            available: collection.available,
            provider: provider.clone(),
            ..Source::default()
        };
        tx.execute(
            "INSERT INTO sources VALUES(?1,?2,?3,?4)",
            params![
                source.kind,
                source.id,
                serde_json::to_string(&source)?,
                provider
            ],
        )?;
    }
    let mut report = ImportReport {
        changed: true,
        matched: 0,
        unmatched: 0,
        ambiguous: 0,
    };
    let mut matches = HashMap::new();
    for item in &doc.assets {
        let Some(guid) = &item.cloud_master_guid else {
            report.unmatched += 1;
            continue;
        };
        if item.owner_hash.as_ref().is_some_and(|s| !s.is_empty()) {
            report.unmatched += 1;
            continue;
        }
        let Some(assets) = by_master.get(guid) else {
            report.unmatched += 1;
            continue;
        };
        if assets.len() != 1 || local_counts.get(guid.as_str()) != Some(&1) {
            report.ambiguous += 1;
            continue;
        }
        let asset_id = &assets[0];
        report.matched += 1;
        matches.insert(item.local_id.clone(), asset_id.clone());
        for person in item.person_ids.iter().filter(|p| people.contains(*p)) {
            tx.execute(
                "INSERT OR IGNORE INTO membership VALUES(?1,'person',?2,?3)",
                params![asset_id, format!("{provider}:{person}"), provider],
            )?;
        }
        for collection in item.collection_ids.iter().filter_map(|id| {
            collections
                .get(id.as_str())
                .copied()
                .filter(|c| c.available)
        }) {
            tx.execute(
                "INSERT OR IGNORE INTO membership VALUES(?1,?2,?3,?4)",
                params![
                    asset_id,
                    collection.kind,
                    format!("{provider}:collection:{}", collection.id),
                    provider
                ],
            )?;
        }
    }
    let saved = serde_json::json!({"document":doc,"binding":binding,"matches":matches});
    tx.execute("INSERT INTO enrichment VALUES(?1,?2,?3) ON CONFLICT(provider) DO UPDATE SET data=excluded.data,updated=excluded.updated",params![provider,serde_json::to_string(&saved)?,doc.created_at])?;
    tx.execute(
        "INSERT OR REPLACE INTO meta VALUES(?1,?2)",
        params![key_name, content_key],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO meta VALUES(?1,?2)",
        params![report_name, serde_json::to_string(&report)?],
    )?;
    tx.commit()?;
    Ok(report)
}
pub fn mark_stale(store: &Store, namespace: &str) -> Result<()> {
    let mut db = store.connection()?;
    let tx = db.transaction()?;
    let provider = format!("mac:{namespace}");
    tx.execute("UPDATE sources SET data=json_set(data,'$.stale',json('true')) WHERE provider=?1 AND json_extract(data,'$.stale') != 1",[&provider])?;
    tx.commit()?;
    Ok(())
}

// Pairing tokens are never serialized into public status or settings.
#[derive(Clone, Deserialize, Serialize)]
pub struct HelperConfig {
    pub helper_url: String,
    pub token: String,
    pub binding: Binding,
}
impl HelperConfig {
    pub fn validate(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.helper_url)
            .map_err(|_| anyhow::anyhow!("invalid_helper_url"))?;
        ensure!(
            url.scheme() == "https"
                && url.host_str().is_some_and(|h| h.ends_with(".ts.net"))
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && (url.path() == "/" || url.path().is_empty()),
            "helper_requires_private_tailscale_https"
        );
        ensure!(
            (32..=1024).contains(&self.token.len())
                && self
                    .token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "invalid_helper_token"
        );
        Ok(())
    }
    pub async fn fetch(&self) -> Result<Enrichment> {
        self.validate()?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let response = client
            .get(format!(
                "{}/v2/enrichment",
                self.helper_url.trim_end_matches('/')
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("helper_offline"))?;
        ensure!(
            response.status().is_success(),
            if response.status().as_u16() == 401 {
                "helper_authentication_failed"
            } else {
                "helper_unavailable"
            }
        );
        let value = crate::icloud::auth::bounded_json(response, 64 * 1024 * 1024).await?;
        serde_json::from_value(value).map_err(|_| anyhow::anyhow!("invalid_helper_enrichment"))
    }
    pub fn save(&self, store: &Store) -> Result<()> {
        self.validate()?;
        crate::storage::atomic_json(
            &store.paths.config.join("helper-v2.json"),
            &serde_json::to_value(self)?,
        )
    }
    pub fn load(store: &Store) -> Result<Option<Self>> {
        let path = store.paths.config.join("helper-v2.json");
        if !path.exists() {
            return Ok(None);
        }
        let config: Self = serde_json::from_slice(&crate::storage::read_bounded(&path, 65536)?)
            .context("invalid_helper_configuration")?;
        config.validate()?;
        Ok(Some(config))
    }
}
