use crate::model::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use rustix::fs::{Mode, OFlags};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Paths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub runtime: PathBuf,
}
impl Paths {
    pub fn under(root: &Path) -> Self {
        Self {
            config: root.join("config"),
            data: root.join("data"),
            runtime: root.join("run"),
        }
    }
    pub fn system() -> Result<Self> {
        let home = dirs::home_dir().context("home_unavailable")?;
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| data.join("omarchy-photo-wallpapers/run"));
        Ok(Self {
            config: config.join("omarchy-photo-wallpapers"),
            data: data.join("omarchy-photo-wallpapers"),
            runtime: runtime.join("omarchy-photo-wallpapers"),
        })
    }
    pub fn database(&self) -> PathBuf {
        self.data.join("state-v2.sqlite")
    }
    pub fn socket(&self) -> PathBuf {
        self.runtime.join("engine.sock")
    }
}

pub fn private_dir(path: &Path) -> Result<()> {
    validate_private_parents(path)?;
    fs::create_dir_all(path)?;
    let directory = fs::File::from(rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let metadata = directory.metadata()?;
    ensure!(
        metadata.uid() == rustix::process::geteuid().as_raw(),
        "private_path_owner_mismatch"
    );
    if metadata.permissions().mode() & 0o777 != 0o700 {
        directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn validate_parent(metadata: &fs::Metadata) -> Result<()> {
    let uid = metadata.uid();
    let mode = metadata.permissions().mode();
    ensure!(metadata.is_dir(), "private_parent_not_directory");
    ensure!(
        uid == 0 || uid == rustix::process::geteuid().as_raw(),
        "private_parent_owner_mismatch"
    );
    // Root-owned sticky temporary directories protect existing owned children.
    // Other shared writable ancestors permit replacement before a private leaf.
    ensure!(
        mode & 0o022 == 0 || (uid == 0 && mode & 0o1000 != 0),
        "private_parent_writable"
    );
    Ok(())
}
pub(crate) fn validate_private_parents(path: &Path) -> Result<()> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut nearest = None;
    for parent in absolute.ancestors().skip(1) {
        match fs::symlink_metadata(parent) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    // Permit immutable OS aliases (/var, /tmp on macOS), never
                    // a user-planted link inside application storage.
                    ensure!(metadata.uid() == 0, "private_parent_symlink");
                    validate_parent(&fs::metadata(parent)?)?;
                } else {
                    validate_parent(&metadata)?;
                }
                if nearest.is_none() {
                    nearest = Some(parent.to_owned());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    // Check the physical ancestry behind any permitted root-owned alias too.
    if let Some(parent) = nearest {
        let canonical = parent.canonicalize()?;
        for parent in canonical.ancestors() {
            validate_parent(&fs::metadata(parent)?)?;
        }
    }
    Ok(())
}
/// Open the object itself, never a symlink. Validate the descriptor before use;
/// NONBLOCK also prevents a substituted FIFO/device from blocking the daemon.
pub(crate) fn private_file(path: &Path, flags: OFlags) -> Result<fs::File> {
    let file = fs::File::from(rustix::fs::open(
        path,
        flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::from_raw_mode(0o600),
    )?);
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "private_path_not_regular_file");
    ensure!(
        metadata.uid() == rustix::process::geteuid().as_raw(),
        "private_path_owner_mismatch"
    );
    Ok(file)
}
pub fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("missing_parent")?;
    private_dir(parent)?;
    let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}
pub fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    atomic_bytes(path, &serde_json::to_vec(value)?)
}
pub fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = private_file(path, OFlags::RDONLY)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit,
        "private_file_size_limit"
    );
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "private_file_size_limit");
    Ok(bytes)
}

#[derive(Clone)]
pub struct Store {
    pub paths: Paths,
    _keeper: Arc<Mutex<Connection>>,
}
impl Store {
    pub fn open(paths: &Paths) -> Result<Self> {
        for path in [&paths.config, &paths.data, &paths.runtime] {
            private_dir(path)?;
        }
        let store = Self {
            paths: paths.clone(),
            _keeper: Arc::new(Mutex::new(Self::open_database(paths)?)),
        };
        let db = store.connection()?;
        let version: u32 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(version <= 3, "unsupported_storage_version");
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
          CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS assets(id TEXT PRIMARY KEY,cloud_guid TEXT NOT NULL,library_id TEXT NOT NULL,revision TEXT NOT NULL,data TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS cloud_identity ON assets(cloud_guid,library_id);
          CREATE TABLE IF NOT EXISTS sources(kind TEXT,id TEXT,data TEXT NOT NULL,provider TEXT NOT NULL,PRIMARY KEY(kind,id));
          CREATE TABLE IF NOT EXISTS membership(asset_id TEXT,kind TEXT,source_id TEXT,provider TEXT,PRIMARY KEY(asset_id,kind,source_id,provider));
          CREATE INDEX IF NOT EXISTS source_members ON membership(kind,source_id,asset_id);
          CREATE TABLE IF NOT EXISTS feedback(asset_id TEXT,action TEXT,crop TEXT,PRIMARY KEY(asset_id,action));
          CREATE TABLE IF NOT EXISTS command_requests(request_id TEXT PRIMARY KEY,action TEXT,job_id TEXT);
          CREATE TABLE IF NOT EXISTS commands(id TEXT PRIMARY KEY,action TEXT,state TEXT,result TEXT,created REAL);
          CREATE TABLE IF NOT EXISTS analysis(key TEXT PRIMARY KEY,data TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS renders(key TEXT PRIMARY KEY,asset_id TEXT,revision TEXT,config_key TEXT,data TEXT NOT NULL,used REAL);
          CREATE INDEX IF NOT EXISTS renders_config_used ON renders(config_key,used DESC);
          CREATE TABLE IF NOT EXISTS enrichment(provider TEXT PRIMARY KEY,data TEXT NOT NULL,updated TEXT);
          BEGIN IMMEDIATE;
          CREATE TABLE IF NOT EXISTS privacy_decisions(key TEXT PRIMARY KEY,data TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS privacy_choices(scope TEXT PRIMARY KEY,content TEXT NOT NULL,data TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS privacy_render_content(render_sha256 TEXT NOT NULL,scope TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(render_sha256,scope));
          PRAGMA user_version=3;
          COMMIT;")?;
        db.execute(
            "INSERT OR IGNORE INTO meta VALUES('settings',?1)",
            [serde_json::to_string(&Settings::default())?],
        )?;
        // Retain a participating connection so SQLite does not checkpoint, fsync and
        // recreate WAL/SHM files after every short-lived operation connection.
        store
            ._keeper
            .lock()
            .map_err(|_| anyhow::anyhow!("storage_lock_failed"))?
            .query_row("SELECT count(*) FROM meta", [], |r| r.get::<_, i64>(0))?;
        Ok(store)
    }
    /// Lifecycle cleanup can read/write existing ownership metadata without
    /// upgrading a database that will remain for a previous runtime.
    pub(crate) fn existing_without_migration(paths: &Paths) -> Result<Self> {
        ensure!(paths.database().exists(), "database_unavailable");
        let db = Self::open_database(paths)?;
        let version: u32 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(version <= 3, "unsupported_storage_version");
        Ok(Self {
            paths: paths.clone(),
            _keeper: Arc::new(Mutex::new(db)),
        })
    }
    pub fn connection(&self) -> Result<Connection> {
        let db = Self::open_database(&self.paths)?;
        db.busy_timeout(Duration::from_secs(5))?;
        Ok(db)
    }
    fn open_database(paths: &Paths) -> Result<Connection> {
        // SQLite's NOFOLLOW also rejects OS aliases such as macOS /var -> /private/var.
        // Resolve the already-validated private directory, retaining NOFOLLOW on
        // the database object itself and on SQLite's canonical path.
        let database = paths.data.canonicalize()?.join("state-v2.sqlite");
        let file = private_file(&database, OFlags::CREATE | OFlags::RDWR)?;
        if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        Ok(Connection::open_with_flags(
            database,
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?)
    }
    pub fn meta(&self, key: &str) -> Result<Value> {
        let raw: Option<String> = self
            .connection()?
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()?;
        Ok(match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Value::Null,
        })
    }
    pub fn set_meta(&self, key: &str, value: &Value) -> Result<()> {
        self.connection()?.execute(
            "INSERT OR REPLACE INTO meta VALUES(?1,?2)",
            params![key, value.to_string()],
        )?;
        Ok(())
    }
    pub fn settings(&self) -> Result<Settings> {
        Ok(serde_json::from_value(self.meta("settings")?)?)
    }
    pub fn update_settings(&self, expected: u64, changes: Value) -> Result<Settings> {
        let _lifecycle = crate::platform::lifecycle_lock(&self.paths, false)?;
        let object = changes.as_object().context("invalid_settings")?;
        let allowed = [
            "sources",
            "interval_seconds",
            "paused",
            "pinned",
            "enhancements",
            "cache_bytes",
            "fallback",
            "match_colors",
            "theme",
            "privacy",
        ];
        ensure!(
            object.keys().all(|k| allowed.contains(&k.as_str())),
            "unknown_setting"
        );
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let raw: String = tx.query_row("SELECT value FROM meta WHERE key='settings'", [], |r| {
            r.get(0)
        })?;
        let mut value: Value = serde_json::from_str(&raw)?;
        ensure!(
            value["revision"].as_u64() == Some(expected),
            "settings_conflict"
        );
        let previous: Settings = serde_json::from_value(value.clone())?;
        for (key, item) in object {
            if key == "privacy" {
                let privacy = item.as_object().context("invalid_privacy_settings")?;
                ensure!(
                    privacy.keys().all(|k| k == "filter_personal"),
                    "meeting_requires_immediate_command"
                );
                let mut next = serde_json::to_value(&previous.privacy)?;
                for (field, enabled) in privacy {
                    next[field] = enabled.clone();
                }
                value[key] = next;
            } else {
                value[key] = item.clone();
            }
        }
        value["revision"] = json!(expected + 1);
        let mut settings: Settings = serde_json::from_value(value).context("invalid_settings")?;
        settings.validate()?;
        let mut known = HashSet::new();
        settings.sources.retain(|s| known.insert(s.clone()));
        privacy_transition(&tx, &previous, &settings)?;
        if previous.privacy != settings.privacy && settings.privacy.active() {
            self.privacy_shadow(&settings)?;
        }
        tx.execute(
            "UPDATE meta SET value=?1 WHERE key='settings'",
            [serde_json::to_string(&settings)?],
        )?;
        tx.commit()?;
        if previous.privacy != settings.privacy && !settings.privacy.active() {
            self.privacy_shadow(&settings)?;
        }
        Ok(settings)
    }
    fn privacy_shadow(&self, settings: &Settings) -> Result<()> {
        let path = self.paths.data.join("privacy-mode.json");
        let value = json!({"schema_version":1,"protected":settings.privacy.active(),"meeting":settings.privacy.meeting});
        if read_bounded(&path, 1024)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            == Some(value.clone())
        {
            return Ok(());
        }
        atomic_json(&path, &value)
    }
    pub fn advance_privacy_generation(&self) -> Result<u64> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let raw: Option<String> = tx
            .query_row(
                "SELECT value FROM meta WHERE key='privacy-generation'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let old = match raw {
            Some(raw) => serde_json::from_str::<u64>(&raw).context("invalid_privacy_state")?,
            None => 0,
        };
        let next = old.checked_add(1).context("privacy_generation_exhausted")?;
        tx.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES('privacy-generation',?1)",
            [json!(next).to_string()],
        )?;
        tx.commit()?;
        Ok(next)
    }
    pub fn set_meeting(&self, enabled: bool) -> Result<Settings> {
        let _lifecycle = crate::platform::lifecycle_lock(&self.paths, false)?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let raw: String = tx.query_row("SELECT value FROM meta WHERE key='settings'", [], |r| {
            r.get(0)
        })?;
        let previous: Settings = serde_json::from_str(&raw)?;
        if previous.privacy.meeting == enabled {
            self.privacy_shadow(&previous)?;
            return Ok(previous);
        }
        let mut settings = previous.clone();
        settings.privacy.meeting = enabled;
        settings.revision = settings
            .revision
            .checked_add(1)
            .context("settings_revision_exhausted")?;
        privacy_transition(&tx, &previous, &settings)?;
        if settings.privacy.active() {
            self.privacy_shadow(&settings)?;
        }
        tx.execute(
            "UPDATE meta SET value=?1 WHERE key='settings'",
            [serde_json::to_string(&settings)?],
        )?;
        tx.commit()?;
        if !settings.privacy.active() {
            self.privacy_shadow(&settings)?;
        }
        Ok(settings)
    }
    pub fn enqueue(&self, action: &str) -> Result<String> {
        self.enqueue_once(action, &uuid::Uuid::new_v4().to_string())
    }
    pub fn enqueue_once(&self, action: &str, request_id: &str) -> Result<String> {
        ensure!(
            ["next", "previous", "refresh"].contains(&action),
            "invalid_action"
        );
        ensure!(
            !request_id.is_empty() && request_id.len() <= 128,
            "invalid_request_id"
        );
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some((previous, id)) = tx
            .query_row(
                "SELECT action,job_id FROM command_requests WHERE request_id=?1",
                [request_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        {
            ensure!(previous == action, "request_id_reused");
            return Ok(id);
        }
        let pending=tx.query_row("SELECT id FROM commands WHERE action=?1 AND state IN ('queued','running') ORDER BY created LIMIT 1",[action],|r|r.get::<_,String>(0)).optional()?;
        let id = if let Some(id) = pending {
            id
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO commands VALUES(?1,?2,'queued',NULL,?3)",
                params![id, action, chrono::Utc::now().timestamp_millis()],
            )?;
            id
        };
        tx.execute(
            "INSERT INTO command_requests VALUES(?1,?2,?3)",
            params![request_id, action, id],
        )?;
        tx.commit()?;
        Ok(id)
    }
    pub fn eligible_ids(&self, selected: &[SourceRef]) -> Result<Vec<String>> {
        let all = selected
            .iter()
            .any(|s| s.kind == "library" && s.id == "all");
        let db = self.connection()?;
        let mut q=db.prepare("SELECT id FROM assets a WHERE NOT EXISTS(SELECT 1 FROM feedback f WHERE f.asset_id=a.id AND f.action='never') AND (?1 OR EXISTS(SELECT 1 FROM membership m JOIN json_each(?2) s ON m.kind=json_extract(s.value,'$.kind') AND m.source_id=json_extract(s.value,'$.id') WHERE m.asset_id=a.id)) ORDER BY id")?;
        Ok(
            q.query_map(params![all, serde_json::to_string(selected)?], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?,
        )
    }
    pub fn claim_command(&self) -> Result<Option<Command>> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let result = tx
            .query_row(
                "SELECT id,action FROM commands WHERE state='queued' ORDER BY created LIMIT 1",
                [],
                |r| {
                    Ok(Command {
                        id: r.get(0)?,
                        action: r.get(1)?,
                    })
                },
            )
            .optional()?;
        if let Some(c) = &result {
            tx.execute("UPDATE commands SET state='running' WHERE id=?1", [&c.id])?;
        }
        tx.commit()?;
        Ok(result)
    }
    pub fn finish_command(&self, id: &str, result: Value) -> Result<()> {
        self.write_projection(&[], Some((id, result)))
    }
    pub fn write_projection(
        &self,
        values: &[(&str, Value)],
        command: Option<(&str, Value)>,
    ) -> Result<()> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for (key, value) in values {
            tx.execute(
                "INSERT OR REPLACE INTO meta VALUES(?1,?2)",
                params![key, value.to_string()],
            )?;
        }
        if let Some((id, result)) = command {
            tx.execute(
                "UPDATE commands SET state='done',result=?2 WHERE id=?1",
                params![id, result.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn recover_commands(&self) -> Result<()> {
        self.connection()?.execute(
            "UPDATE commands SET state='queued' WHERE state='running'",
            [],
        )?;
        Ok(())
    }
    pub fn pending_actions(&self) -> Result<u64> {
        Ok(self.connection()?.query_row(
            "SELECT count(*) FROM commands WHERE state IN ('queued','running')",
            [],
            |r| r.get::<_, i64>(0).map(|n| n as u64),
        )?)
    }
    pub fn activate_catalog(
        &self,
        assets: &[Asset],
        sources: &[Source],
        members: &[Membership],
        generation: &str,
    ) -> Result<()> {
        self.activate_catalog_checked(assets, sources, members, generation, None)
    }
    pub fn activate_catalog_checked(
        &self,
        assets: &[Asset],
        sources: &[Source],
        members: &[Membership],
        generation: &str,
        session: Option<(u64, String)>,
    ) -> Result<()> {
        let mut ids = HashSet::new();
        for asset in assets {
            ensure!(
                !asset.id.is_empty()
                    && !asset.revision.is_empty()
                    && asset.width > 0
                    && asset.height > 0
                    && ids.insert(&asset.id),
                "invalid_asset"
            );
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some((epoch, account)) = session {
            let raw: Option<String> = tx
                .query_row(
                    "SELECT value FROM meta WHERE key='sync_authorization_epoch'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            let current: u64 = raw
                .map(|s| serde_json::from_str(&s))
                .transpose()?
                .unwrap_or(0);
            ensure!(epoch == current, "command_cancelled");
            tx.execute(
                "INSERT OR REPLACE INTO meta VALUES('base-account-id',?1)",
                [json!(account).to_string()],
            )?;
        }
        tx.execute("DELETE FROM assets", [])?;
        tx.execute("DELETE FROM sources WHERE provider='icloud'", [])?;
        tx.execute("DELETE FROM membership WHERE provider='icloud'", [])?;
        for asset in assets.iter().filter(|a| !a.hidden && !a.deleted) {
            tx.execute(
                "INSERT INTO assets VALUES(?1,?2,?3,?4,?5)",
                params![
                    asset.id,
                    asset.cloud_guid,
                    asset.library_id,
                    asset.revision,
                    serde_json::to_string(asset)?
                ],
            )?;
        }
        for source in sources {
            tx.execute(
                "INSERT OR REPLACE INTO sources VALUES(?1,?2,?3,'icloud')",
                params![source.kind, source.id, serde_json::to_string(source)?],
            )?;
        }
        for member in members {
            tx.execute(
                "INSERT OR IGNORE INTO membership VALUES(?1,?2,?3,'icloud')",
                params![member.asset_id, member.kind, member.source_id],
            )?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO meta VALUES('catalog_generation',?1)",
            [json!(generation).to_string()],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO meta VALUES('last_sync',?1)",
            [json!(chrono::Utc::now().to_rfc3339()).to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn assets(&self) -> Result<Vec<Asset>> {
        let db = self.connection()?;
        let mut query = db.prepare("SELECT data FROM assets ORDER BY id")?;
        let raw = query
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        raw.into_iter()
            .map(|r| Ok(serde_json::from_str(&r)?))
            .collect()
    }
    pub fn asset(&self, id: &str) -> Result<Option<Asset>> {
        let raw: Option<String> = self
            .connection()?
            .query_row("SELECT data FROM assets WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        raw.map(|r| Ok(serde_json::from_str(&r)?)).transpose()
    }
    pub fn eligible(&self, selected: &[SourceRef], include_excluded: bool) -> Result<Vec<Asset>> {
        let all = selected
            .iter()
            .any(|s| s.kind == "library" && s.id == "all");
        let db = self.connection()?;
        let mut q=db.prepare("SELECT data FROM assets a WHERE (?1 OR NOT EXISTS(SELECT 1 FROM feedback f WHERE f.asset_id=a.id AND f.action='never')) AND (?2 OR EXISTS(SELECT 1 FROM membership m JOIN json_each(?3) s ON m.kind=json_extract(s.value,'$.kind') AND m.source_id=json_extract(s.value,'$.id') WHERE m.asset_id=a.id)) ORDER BY a.id")?;
        let raw = q
            .query_map(
                params![include_excluded, all, serde_json::to_string(selected)?],
                |r| r.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        raw.into_iter()
            .map(|r| Ok(serde_json::from_str(&r)?))
            .collect()
    }
    pub fn sources(&self) -> Result<Vec<Source>> {
        let selected = self.settings()?.sources;
        let db = self.connection()?;
        let mut q = db.prepare("SELECT data FROM sources ORDER BY kind,id")?;
        let mut sources = Vec::new();
        for raw in q.query_map([], |r| r.get::<_, String>(0))? {
            let mut s: Source = serde_json::from_str(&raw?)?;
            s.selected = selected.iter().any(|r| r.id == s.id && r.kind == s.kind);
            s.count = if s.kind == "library" {
                db.query_row("SELECT count(*) FROM assets", [], |r| {
                    r.get::<_, i64>(0).map(|n| n as u64)
                })?
            } else {
                db.query_row("SELECT count(DISTINCT a.id) FROM assets a JOIN membership m ON a.id=m.asset_id WHERE m.kind=?1 AND m.source_id=?2",params![s.kind,s.id],|r|r.get::<_,i64>(0).map(|n|n as u64))?
            };
            sources.push(s);
        }
        for s in &selected {
            if !sources.iter().any(|r| r.kind == s.kind && r.id == s.id) {
                sources.push(Source {
                    id: s.id.clone(),
                    kind: s.kind.clone(),
                    name: "Unavailable source".into(),
                    available: false,
                    selected: true,
                    ..Source::default()
                });
            }
        }
        Ok(sources)
    }
    pub fn feedback(&self, asset: &str, action: &str, crop: Option<Value>) -> Result<()> {
        ensure!(
            ["never", "prefer", "crop", "restore", "period"].contains(&action),
            "invalid_feedback"
        );
        let db = self.connection()?;
        if action == "period" {
            let period = crop
                .as_ref()
                .and_then(|v| v["period"].as_str())
                .filter(|s| ["auto", "day", "night", "either"].contains(s))
                .ok_or_else(|| anyhow::anyhow!("invalid_feedback"))?;
            if period == "auto" {
                db.execute(
                    "DELETE FROM feedback WHERE asset_id=?1 AND action='period'",
                    [asset],
                )?;
                return Ok(());
            }
        }
        if action == "restore" {
            db.execute("DELETE FROM feedback WHERE asset_id=?1", [asset])?;
        } else {
            db.execute(
                "INSERT OR REPLACE INTO feedback VALUES(?1,?2,?3)",
                params![asset, action, crop.map(|c| c.to_string())],
            )?;
        }
        Ok(())
    }
    pub fn import_legacy(&self, settings: Value, excluded: &[String], engine: Value) -> Result<()> {
        if self.meta("legacy_imported")? == json!(true) {
            return Ok(());
        }
        let value: Settings = serde_json::from_value(settings)?;
        value.validate()?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM meta WHERE key='legacy_imported')",
            [],
            |r| r.get(0),
        )?;
        if existing {
            return Ok(());
        }
        for (key, value) in [
            ("settings", serde_json::to_value(value)?),
            ("migration_pending_ids", json!(excluded)),
            ("legacy_engine", engine),
            ("legacy_imported", json!(true)),
        ] {
            tx.execute(
                "INSERT OR REPLACE INTO meta VALUES(?1,?2)",
                params![key, value.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

fn privacy_transition(
    tx: &rusqlite::Transaction<'_>,
    previous: &Settings,
    next: &Settings,
) -> Result<()> {
    if previous.privacy == next.privacy {
        return Ok(());
    }
    let read = |key: &str| -> Result<Value> {
        let raw: Option<String> = tx
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()?;
        Ok(match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Value::Null,
        })
    };
    let old = read("privacy-generation")?;
    let generation = if old.is_null() {
        0
    } else {
        old.as_u64().context("invalid_privacy_state")?
    }
    .checked_add(1)
    .context("privacy_generation_exhausted")?;
    let mut entries = vec![
        ("privacy-generation", json!(generation)),
        (
            "privacy-state-v1",
            json!({"generation":generation,"state":if next.privacy.active(){"arming"}else{"off"}}),
        ),
    ];
    if !previous.privacy.active()
        && next.privacy.active()
        && read("privacy-personal-context")?.is_null()
    {
        entries.push(("privacy-personal-context",json!({"current":read("current-v2")?,"history_index":read("history-index-v2")?,"account":read("base-account-id")?})));
    }
    if previous.privacy.meeting != next.privacy.meeting {
        entries.push(("privacy-meeting-pin", Value::Null));
        entries.push(("privacy-meeting-pin-set", json!(false)));
    }
    for (key, value) in entries {
        tx.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
            params![key, value.to_string()],
        )?;
    }
    Ok(())
}
