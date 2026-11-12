//! Read-only Photos CloudKit provider. Network and private URLs stay outside QML.
use super::auth::{ICloudClient, bounded_json};
use crate::{
    model::*,
    storage::{Paths, Store, atomic_bytes, private_dir},
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use reqwest::{Client, Url};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    time::Duration,
};
pub const ALL_QUERY: &str = "CPLAssetAndMasterByAssetDateWithoutHiddenOrDeleted";
const BASE: &str = "/database/1/com.apple.photos.cloud/production";
const MAX_IMAGE: u64 = 150 * 1024 * 1024;
const DESIRED: &[&str] = &[
    "resOriginalWidth",
    "resOriginalHeight",
    "resOriginalFileType",
    "resOriginalRes",
    "resJPEGFullWidth",
    "resJPEGFullHeight",
    "resJPEGFullFileType",
    "resJPEGFullRes",
    "resJPEGLargeWidth",
    "resJPEGLargeHeight",
    "resJPEGLargeFileType",
    "resJPEGLargeRes",
    "filenameEnc",
    "originalOrientation",
    "orientation",
    "masterRef",
    "assetDate",
    "isFavorite",
    "isHidden",
    "isDeleted",
    "isExpunged",
    "itemType",
    "recordChangeTag",
];
#[derive(Clone)]
pub struct PhotosProvider {
    http: Client,
    download_http: Client,
    base: String,
    account: String,
    client_id: String,
    origin: String,
    paths: Paths,
    progress: Option<std::sync::Arc<dyn Fn(Value) + Send + Sync>>,
    local: Option<String>,
}
pub struct CatalogSnapshot {
    pub assets: Vec<Asset>,
    pub sources: Vec<Source>,
    pub memberships: Vec<Membership>,
}
impl CatalogSnapshot {
    pub fn activate(&self, store: &Store) -> Result<()> {
        store.activate_catalog(
            &self.assets,
            &self.sources,
            &self.memberships,
            &uuid::Uuid::new_v4().to_string(),
        )
    }
}
impl PhotosProvider {
    pub fn new(client: &ICloudClient) -> Result<Self> {
        let base = client.photos_url()?;
        let local = client
            .endpoints
            .home
            .starts_with("http://127.0.0.1:")
            .then(|| client.endpoints.home.clone());
        Ok(Self {
            http: client.http.clone(),
            download_http: Client::builder()
                .timeout(Duration::from_secs(90))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base,
            account: client.dsid()?,
            client_id: client.client_id().into(),
            origin: client.endpoints.home.clone(),
            paths: client.paths.clone(),
            progress: None,
            local,
        })
    }
    pub fn with_progress(mut self, progress: impl Fn(Value) + Send + Sync + 'static) -> Self {
        self.progress = Some(std::sync::Arc::new(progress));
        self
    }
    fn progress(&self, value: Value) {
        if let Some(report) = &self.progress {
            report(value);
        }
    }
    async fn post(&self, scope: &str, route: &str, body: &Value) -> Result<Value> {
        ensure!(
            ["private", "shared"].contains(&scope),
            "invalid_cloud_scope"
        );
        for attempt in 0..3 {
            let response = self
                .http
                .post(format!("{}{BASE}/{scope}/{route}", self.base))
                .query(&[
                    ("dsid", self.account.as_str()),
                    ("clientId", self.client_id.as_str()),
                    ("remapEnums", "True"),
                ])
                .header("Content-Type", "text/plain")
                .header("Origin", &self.origin)
                .header("Referer", format!("{}/", self.origin))
                .body(serde_json::to_vec(body)?)
                .send()
                .await;
            match response {
                Ok(response) => {
                    let status = response.status().as_u16();
                    if [401, 403, 421].contains(&status) {
                        bail!("apple_session_expired");
                    }
                    if (status == 429 || status >= 500) && attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))).await;
                        continue;
                    }
                    ensure!((200..300).contains(&status), "apple_catalog_unavailable");
                    let result = bounded_json(response, 16 * 1024 * 1024).await?;
                    ensure!(
                        result.get("serverErrorCode").is_none(),
                        "apple_catalog_unavailable"
                    );
                    return Ok(result);
                }
                Err(_) if attempt < 2 => {
                    tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))).await
                }
                Err(_) => bail!("apple_network_error"),
            }
        }
        bail!("apple_catalog_unavailable")
    }
    async fn folders(&self, scope: &str, zone: &Value) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        let mut parents = std::collections::VecDeque::from([None]);
        let mut visited = HashSet::new();
        while let Some(parent) = parents.pop_front() {
            let mut markers = HashSet::new();
            let mut marker = None;
            for _ in 0..1000 {
                let mut body = json!({"zoneID":zone,"query":{"recordType":"CPLAlbumByPositionLive"},"resultsLimit":200});
                if let Some(parent) = &parent {
                    body["query"]["filterBy"] = json!([{"fieldName":"parentId","comparator":"EQUALS","fieldValue":{"type":"STRING","value":parent}}]);
                }
                if let Some(v) = marker.take() {
                    body["continuationMarker"] = v;
                }
                let page = self.post(scope, "records/query", &body).await?;
                for row in page["records"]
                    .as_array()
                    .context("invalid_apple_catalog")?
                {
                    let id = row["recordName"].as_str().context("invalid_apple_album")?;
                    if ["----Root-Folder----", "----Project-Root-Folder----"].contains(&id)
                        || row["deleted"] == true
                        || truth(field(row, "isDeleted"))
                    {
                        continue;
                    }
                    match field(row, "albumType").as_u64() {
                        Some(0) => rows.push(row.clone()),
                        Some(3) if visited.insert(id.to_owned()) => {
                            parents.push_back(Some(id.to_owned()));
                        }
                        _ => {}
                    }
                }
                ensure!(
                    visited.len() <= 10000 && rows.len() <= 100000,
                    "apple_catalog_size_limit"
                );
                if let Some(next) = page["continuationMarker"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                {
                    ensure!(
                        markers.insert(next.to_owned()),
                        "repeated_apple_continuation"
                    );
                    marker = Some(json!(next));
                } else {
                    break;
                }
            }
            ensure!(marker.is_none(), "apple_catalog_page_limit");
        }
        Ok(rows)
    }
    async fn assets_query(
        &self,
        scope: &str,
        zone: &Value,
        album: Option<&str>,
    ) -> Result<Vec<Asset>> {
        let mut rows = super::staging::Records::new(&self.paths)?;
        let expected = if album.is_none() {
            let count=self.post(scope,"records/query",&json!({"zoneID":zone,"query":{"recordType":"HyperionIndexCountLookup","filterBy":[{"fieldName":"indexCountID","comparator":"IN","fieldValue":{"type":"STRING_LIST","value":["CPLAssetByAssetDateWithoutHiddenOrDeleted"]}}]},"resultsLimit":1})).await?;
            let count = count["records"]
                .as_array()
                .and_then(|r| r.first())
                .and_then(|r| field(r, "itemCount").as_u64())
                .context("invalid_apple_catalog_count")?;
            ensure!(count <= 1_000_000, "apple_catalog_size_limit");
            Some(count)
        } else {
            None
        };
        if let Some(total) = expected {
            self.progress(json!({"phase":"photos","library":if zone["zoneName"]=="PrimarySync"{"personal"}else{"shared"},"completed":0,"total":total}));
        }
        let offset = 0;
        let mut marker = None;
        let mut markers = HashSet::new();
        for _ in 0..10000 {
            let mut filters = vec![
                json!({"fieldName":"startRank","fieldValue":{"type":"INT64","value":offset},"comparator":"EQUALS"}),
                json!({"fieldName":"direction","fieldValue":{"type":"STRING","value":"ASCENDING"},"comparator":"EQUALS"}),
            ];
            let query = if let Some(id) = album {
                filters.push(json!({"fieldName":"parentId","comparator":"EQUALS","fieldValue":{"type":"STRING","value":id}}));
                "CPLContainerRelationLiveByPosition"
            } else {
                ALL_QUERY
            };
            let mut body = json!({"zoneID":zone,"query":{"recordType":query,"filterBy":filters},"resultsLimit":400,"desiredKeys":DESIRED});
            if let Some(value) = marker.take() {
                body["continuationMarker"] = value;
            }
            let page = self.post(scope, "records/query", &body).await?;
            let records = page["records"]
                .as_array()
                .context("invalid_apple_catalog")?;
            if records.is_empty() {
                return self.complete_records(rows, scope, zone, expected).await;
            }
            let added = rows.insert(records)?;
            if let Some(total) = expected {
                self.progress(json!({"phase":"photos","library":if zone["zoneName"]=="PrimarySync"{"personal"}else{"shared"},"completed":rows.asset_count()?,"total":total}));
            }
            ensure!(added > 0, "repeated_apple_page");
            if let Some(next) = page["continuationMarker"]
                .as_str()
                .filter(|s| !s.is_empty())
            {
                ensure!(
                    markers.insert(next.to_owned()),
                    "repeated_apple_continuation"
                );
                marker = Some(json!(next));
            } else {
                return self.complete_records(rows, scope, zone, expected).await;
            }
        }
        bail!("apple_catalog_page_limit")
    }
    async fn complete_records(
        &self,
        mut rows: super::staging::Records,
        scope: &str,
        zone: &Value,
        expected: Option<u64>,
    ) -> Result<Vec<Asset>> {
        if expected.is_none() {
            let library = library_id(&self.account, scope, zone)?;
            return Ok(rows
                .member_ids()?
                .into_iter()
                .map(|id| Asset {
                    id: format!("{library}:{id}"),
                    ..Asset::default()
                })
                .collect());
        }
        ensure!(
            Some(rows.asset_count()?) == expected,
            "apple_catalog_incomplete"
        );
        // Apple can omit a master from query pages even when its asset is present.
        // Resolve by exact record identity before publishing a complete base.
        for batch in rows.missing_masters()?.chunks(100) {
            let records: Vec<_> = batch.iter().map(|id| json!({"recordName":id})).collect();
            let page = self
                .post(
                    scope,
                    "records/lookup",
                    &json!({"zoneID":zone,"records":records,"desiredKeys":DESIRED}),
                )
                .await?;
            rows.insert(
                page["records"]
                    .as_array()
                    .context("invalid_apple_catalog")?,
            )?;
        }
        rows.assets(&self.account, scope, zone)
    }
    pub async fn catalog(&self) -> Result<CatalogSnapshot> {
        let mut output = CatalogSnapshot {
            assets: Vec::new(),
            sources: vec![Source::library()],
            memberships: Vec::new(),
        };
        for scope in ["private", "shared"] {
            let mut marker = None;
            let mut markers = HashSet::new();
            let mut zones = Vec::new();
            for _ in 0..1000 {
                let mut body = json!({});
                if let Some(v) = marker.take() {
                    body["syncToken"] = v;
                }
                let response = self.post(scope, "changes/database", &body).await?;
                zones.extend(
                    response["zones"]
                        .as_array()
                        .context("invalid_apple_zones")?
                        .iter()
                        .cloned(),
                );
                if response["moreComing"] == true {
                    let next = response["syncToken"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .context("invalid_apple_continuation")?;
                    ensure!(
                        markers.insert(next.to_owned()),
                        "repeated_apple_continuation"
                    );
                    marker = Some(json!(next));
                } else {
                    break;
                }
            }
            ensure!(marker.is_none(), "apple_catalog_page_limit");
            for entry in zones {
                let zone = &entry["zoneID"];
                let name = zone["zoneName"].as_str().context("invalid_apple_zone")?;
                if entry["deleted"] == true
                    || !(name == "PrimarySync" || name.starts_with("SharedSync-"))
                {
                    continue;
                }
                let assets = self.assets_query(scope, zone, None).await?;
                let ids: HashSet<_> = assets.iter().map(|a| a.id.clone()).collect();
                output.memberships.extend(ids.iter().map(|id| Membership {
                    asset_id: id.clone(),
                    kind: "library".into(),
                    source_id: "all".into(),
                }));
                output.assets.extend(assets);
                // CloudKit's album tree belongs to the personal primary zone.
                if scope != "private" || name != "PrimarySync" {
                    continue;
                }
                let folders = self.folders(scope, zone).await?;
                for (index, folder) in folders.iter().enumerate() {
                    self.progress(
                        json!({"phase":"albums","completed":index,"total":folders.len()}),
                    );
                    if truth(field(folder, "isDeleted")) {
                        continue;
                    }
                    let id = folder["recordName"]
                        .as_str()
                        .context("invalid_apple_album")?;
                    let source_id = format!("{}:{id}", library_id(&self.account, scope, zone)?);
                    let name = field(folder, "albumNameEnc")
                        .as_str()
                        .and_then(|s| B64.decode(s).ok())
                        .and_then(|b| String::from_utf8(b).ok())
                        .unwrap_or_else(|| "Untitled album".into());
                    let kind = if scope == "shared" {
                        "shared_album"
                    } else {
                        "album"
                    };
                    let members = self.assets_query(scope, zone, Some(id)).await?;
                    output.memberships.extend(
                        members
                            .into_iter()
                            .filter(|a| ids.contains(&a.id))
                            .map(|a| Membership {
                                asset_id: a.id,
                                kind: kind.into(),
                                source_id: source_id.clone(),
                            }),
                    );
                    output.sources.push(Source {
                        id: source_id,
                        kind: kind.into(),
                        name,
                        ..Source::default()
                    });
                }
            }
        }
        self.progress(json!({"phase":"finishing"}));
        Ok(output)
    }
    pub async fn download(&self, asset: &Asset, paths: &Paths) -> Result<PathBuf> {
        let res = asset
            .resources
            .get(&asset.rendition)
            .context("no_downloadable_rendition")?;
        validate_media_url(&res.url, self.local.as_deref())?;
        ensure!(res.bytes > 0 && res.bytes <= MAX_IMAGE, "image_size_limit");
        // Metadata edits do not change image bytes. The scoped ID, rendition and
        // provider fingerprint isolate accounts while avoiding duplicate media.
        let key = hex::encode(Sha256::digest(serde_json::to_vec(&json!([
            asset.id,
            asset.rendition,
            res.checksum,
            res.bytes
        ]))?));
        let parent = paths.data.join("media-v2");
        private_dir(&parent)?;
        let target = parent.join(key);
        let digest_path = target.with_extension("sha256");
        if let (Ok(bytes), Ok(digest)) = (
            crate::storage::read_bounded(&target, res.bytes),
            crate::storage::read_bounded(&digest_path, 64)
                .and_then(|v| String::from_utf8(v).map_err(Into::into)),
        ) && validate_bytes(&bytes, res).is_ok()
            && hex::encode(Sha256::digest(&bytes)) == digest
        {
            return Ok(target);
        }
        let mut response = self
            .download_http
            .get(&res.url)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("apple_download_unavailable"))?;
        if [401, 403, 404, 410].contains(&response.status().as_u16()) {
            // Signed CDN URLs expire independently of the catalog/session. Renew
            // just this photo's resources; never rescan the library to change it.
            let fresh = self.refresh_resource(asset).await?;
            validate_media_url(&fresh, self.local.as_deref())?;
            response = self
                .download_http
                .get(fresh)
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("apple_download_unavailable"))?;
        }
        ensure!(response.status().is_success(), "apple_download_unavailable");
        ensure!(
            response.content_length().is_none_or(|n| n == res.bytes),
            "download_size_mismatch"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("apple_download_unavailable"))?
        {
            ensure!(
                bytes.len() as u64 + chunk.len() as u64 <= res.bytes,
                "image_size_limit"
            );
            bytes.extend(chunk);
        }
        validate_bytes(&bytes, res)?;
        // Apple fileChecksum is an opaque MMCS compound signature, not a content hash.
        // TLS + exact length + media signature protect receipt; our SHA256 protects reuse.
        atomic_bytes(&target, &bytes)?;
        atomic_bytes(&digest_path, hex::encode(Sha256::digest(&bytes)).as_bytes())?;
        Ok(target)
    }
    async fn refresh_resource(&self, asset: &Asset) -> Result<String> {
        let parts: Vec<_> = asset.library_id.splitn(5, ':').collect();
        ensure!(
            parts.len() == 5
                && parts[0] == "icloud"
                && parts[1] == self.account
                && !asset.cloud_guid.is_empty()
                && !asset.cloud_master_guid.is_empty(),
            "apple_asset_changed"
        );
        let zone = json!({"zoneName":parts[3],"ownerRecordName":parts[4]});
        let body = json!({"zoneID":zone,"records":[{"recordName":asset.cloud_guid},{"recordName":asset.cloud_master_guid}],"desiredKeys":DESIRED});
        let page = self.post(parts[2], "records/lookup", &body).await?;
        let latest = parse_assets(
            &self.account,
            parts[2],
            &zone,
            page["records"]
                .as_array()
                .context("invalid_apple_catalog")?,
        )?;
        let latest = latest
            .iter()
            .find(|a| a.id == asset.id)
            .context("apple_asset_changed")?;
        ensure!(latest.rendition == asset.rendition, "apple_asset_changed");
        let old = asset
            .resources
            .get(&asset.rendition)
            .context("apple_asset_changed")?;
        let fresh = latest
            .resources
            .get(&asset.rendition)
            .context("apple_asset_changed")?;
        ensure!(
            old.checksum == fresh.checksum && old.bytes == fresh.bytes,
            "apple_asset_changed"
        );
        Ok(fresh.url.clone())
    }
}
fn validate_bytes(bytes: &[u8], res: &Resource) -> Result<()> {
    ensure!(bytes.len() as u64 == res.bytes, "download_size_mismatch");
    ensure!(!res.checksum.is_empty(), "missing_apple_fingerprint");
    let media = bytes.starts_with(&[0xff, 0xd8, 0xff])
        || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"II*\0")
        || bytes.starts_with(b"MM\0*")
        || bytes.get(4..8) == Some(b"ftyp")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"));
    ensure!(media, "invalid_downloaded_media");
    Ok(())
}
pub fn validate_media_url(raw: &str, local: Option<&str>) -> Result<()> {
    let url = Url::parse(raw).map_err(|_| anyhow::anyhow!("invalid_media_url"))?;
    let allowed = [
        "icloud.com",
        "icloud.com.cn",
        "icloud-content.com",
        "icloud-content.com.cn",
        "cdn-apple.com",
        "apple.com",
        "apple.com.cn",
    ];
    let trusted = url.scheme() == "https"
        && url.host_str().is_some_and(|h| {
            allowed
                .iter()
                .any(|suffix| h == *suffix || h.ends_with(&format!(".{suffix}")))
        });
    let test = local.is_some_and(|base| Url::parse(base).is_ok_and(|b| b.origin() == url.origin()));
    ensure!(
        url.username().is_empty() && url.password().is_none() && (trusted || test),
        "untrusted_media_url"
    );
    Ok(())
}
fn field<'a>(row: &'a Value, key: &str) -> &'a Value {
    &row["fields"][key]["value"]
}
fn truth(v: &Value) -> bool {
    v == &json!(true) || v.as_i64().is_some_and(|n| n != 0)
}
fn library_id(account: &str, scope: &str, zone: &Value) -> Result<String> {
    Ok(format!(
        "icloud:{account}:{scope}:{}:{}",
        zone["zoneName"].as_str().context("invalid_apple_zone")?,
        zone["ownerRecordName"].as_str().unwrap_or("_defaultOwner")
    ))
}
pub fn parse_assets(
    account: &str,
    scope: &str,
    zone: &Value,
    records: &[Value],
) -> Result<Vec<Asset>> {
    let library = library_id(account, scope, zone)?;
    let masters: HashMap<_, _> = records
        .iter()
        .filter(|v| v["recordType"] == "CPLMaster")
        .filter_map(|r| r["recordName"].as_str().map(|id| (id, r)))
        .collect();
    let mut output = Vec::new();
    for record in records.iter().filter(|v| v["recordType"] == "CPLAsset") {
        if record["deleted"] == true
            || ["isHidden", "isDeleted", "isExpunged"]
                .iter()
                .any(|k| truth(field(record, k)))
        {
            continue;
        }
        let id = record["recordName"]
            .as_str()
            .context("invalid_apple_asset")?;
        let master = masters
            .get(
                field(record, "masterRef")["recordName"]
                    .as_str()
                    .context("missing_apple_master")?,
            )
            .context("missing_apple_master")?;
        let filename = field(master, "filenameEnc")
            .as_str()
            .and_then(|s| B64.decode(s).ok())
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default();
        let mut resources = BTreeMap::new();
        let mut dimensions = HashMap::new();
        for (label, prefix) in [
            ("edited", "resJPEGFull"),
            ("jpeg_fallback", "resJPEGLarge"),
            ("original_fallback", "resOriginal"),
        ] {
            let owner = if field(record, &format!("{prefix}Res")).is_object() {
                record
            } else {
                *master
            };
            let res = field(owner, &format!("{prefix}Res"));
            let file_type = field(owner, &format!("{prefix}FileType"))
                .as_str()
                .unwrap_or("");
            if file_type.contains("movie")
                || file_type.contains("video")
                || file_type.contains("mpeg")
            {
                continue;
            }
            if let (Some(url), Some(bytes), Some(checksum)) = (
                res["downloadURL"].as_str(),
                res["size"].as_u64(),
                res["fileChecksum"].as_str(),
            ) {
                let width = field(owner, &format!("{prefix}Width"))
                    .as_u64()
                    .unwrap_or(0);
                let height = field(owner, &format!("{prefix}Height"))
                    .as_u64()
                    .unwrap_or(0);
                if width == 0 || height == 0 || width > 100000 || height > 100000 {
                    continue;
                }
                resources.insert(
                    label.into(),
                    Resource {
                        url: url.into(),
                        bytes,
                        checksum: checksum.into(),
                        content_type: file_type.into(),
                    },
                );
                dimensions.insert(label, (width as u32, height as u32));
            }
        }
        let Some(label) = ["edited", "jpeg_fallback", "original_fallback"]
            .into_iter()
            .find(|k| resources.contains_key(*k))
        else {
            continue;
        };
        let (width, height) = dimensions[label];
        ensure!(
            width > 0 && height > 0 && width <= 100000 && height <= 100000,
            "invalid_apple_dimensions"
        );
        let captured_at = field(record, "assetDate")
            .as_i64()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|d| d.to_rfc3339());
        let revision = hex::encode(Sha256::digest(serde_json::to_vec(&json!([
            record["recordChangeTag"],
            master["recordChangeTag"],
            resources
                .iter()
                .map(|(k, r)| (k, &r.checksum))
                .collect::<Vec<_>>()
        ]))?));
        output.push(Asset {
            id: format!("{library}:{id}"),
            cloud_guid: id.into(),
            cloud_master_guid: master["recordName"].as_str().unwrap_or("").into(),
            library_id: library.clone(),
            revision,
            filename,
            width,
            height,
            orientation: field(record, "orientation")
                .as_u64()
                .or_else(|| field(master, "originalOrientation").as_u64())
                .unwrap_or(1) as u32,
            captured_at,
            favorite: truth(field(record, "isFavorite")),
            rendition: label.into(),
            resources,
            ..Asset::default()
        });
    }
    Ok(output)
}
