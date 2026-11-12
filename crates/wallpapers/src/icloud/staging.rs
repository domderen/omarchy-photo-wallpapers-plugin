//! Disk-backed raw records: keep only one response page and compact assets in memory.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde_json::Value;
use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt, path::PathBuf};
pub(super) struct Records {
    db: Option<Connection>,
    path: PathBuf,
}
impl Records {
    pub fn new(paths: &crate::storage::Paths) -> Result<Self> {
        let directory = paths.data.join("catalog-staging");
        crate::storage::private_dir(&directory)?;
        let path = directory.join(format!("{}.sqlite", uuid::Uuid::new_v4()));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let db = Connection::open(&path)?;
        let spool = Self { db: Some(db), path };
        spool.db.as_ref().unwrap().execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-2048; CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,master TEXT,data TEXT NOT NULL); CREATE INDEX record_kinds ON records(kind);")?;
        Ok(spool)
    }
    pub fn insert(&mut self, records: &[Value]) -> Result<usize> {
        let tx = self
            .db
            .as_mut()
            .context("catalog_staging_closed")?
            .transaction()?;
        let mut added = 0;
        {
            let mut insert = tx.prepare("INSERT OR IGNORE INTO records VALUES(?1,?2,?3,?4)")?;
            let mut update =
                tx.prepare("UPDATE records SET kind=?2,master=?3,data=?4 WHERE id=?1")?;
            for row in records {
                let id = row["recordName"].as_str().context("invalid_apple_record")?;
                let kind = row["recordType"].as_str().unwrap_or("");
                if !["CPLAsset", "CPLMaster"].contains(&kind) {
                    continue;
                }
                ensure!(id.len() <= 1024, "invalid_apple_record");
                let master = row
                    .pointer("/fields/masterRef/value/recordName")
                    .and_then(Value::as_str);
                let data = serde_json::to_string(row)?;
                let fresh = insert.execute(params![id, kind, master, data])?;
                if fresh == 0 {
                    update.execute(params![id, kind, master, data])?;
                }
                if kind == "CPLAsset" {
                    added += fresh;
                }
            }
        }
        let count: i64 = tx.query_row("SELECT count(*) FROM records", [], |r| r.get(0))?;
        ensure!(count <= 2_000_000, "apple_catalog_size_limit");
        tx.commit()?;
        Ok(added)
    }
    pub fn assets(
        &self,
        account: &str,
        scope: &str,
        zone: &Value,
    ) -> Result<Vec<crate::model::Asset>> {
        let mut query=self.db.as_ref().context("catalog_staging_closed")?.prepare("SELECT a.data,m.data FROM records a LEFT JOIN records m ON m.id=a.master AND m.kind='CPLMaster' WHERE a.kind='CPLAsset' ORDER BY a.id")?;
        let mut output = Vec::new();
        for row in query.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })? {
            let (asset, master) = row?;
            let mut pair = vec![serde_json::from_str(&asset)?];
            if let Some(master) = master {
                pair.push(serde_json::from_str(&master)?);
            }
            output.extend(super::photos::parse_assets(account, scope, zone, &pair)?);
        }
        Ok(output)
    }
    pub fn missing_masters(&self) -> Result<Vec<String>> {
        let mut query=self.db.as_ref().context("catalog_staging_closed")?.prepare("SELECT DISTINCT a.master FROM records a LEFT JOIN records m ON m.id=a.master AND m.kind='CPLMaster' WHERE a.kind='CPLAsset' AND a.master IS NOT NULL AND m.id IS NULL")?;
        Ok(query
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn member_ids(&self) -> Result<Vec<String>> {
        let mut query = self
            .db
            .as_ref()
            .context("catalog_staging_closed")?
            .prepare("SELECT id FROM records WHERE kind='CPLAsset'")?;
        Ok(query
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn asset_count(&self) -> Result<u64> {
        Ok(self
            .db
            .as_ref()
            .context("catalog_staging_closed")?
            .query_row(
                "SELECT count(*) FROM records WHERE kind='CPLAsset'",
                [],
                |r| r.get::<_, i64>(0).map(|n| n as u64),
            )?)
    }
}
impl Drop for Records {
    fn drop(&mut self) {
        self.db.take();
        let _ = std::fs::remove_file(&self.path);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn cross_page_master_join_is_exact_and_private_spool_is_removed() {
        let root = tempfile::tempdir().unwrap();
        let paths = crate::storage::Paths::under(root.path());
        let mut spool = Records::new(&paths).unwrap();
        let path = spool.path.clone();
        assert_eq!(spool.insert(&[json!({"recordName":"asset","recordType":"CPLAsset","fields":{"masterRef":{"value":{"recordName":"master"}}}})]).unwrap(),1);
        assert_eq!(spool.insert(&[json!({"recordName":"asset","recordType":"CPLAsset","fields":{"masterRef":{"value":{"recordName":"master"}}}})]).unwrap(),0);
        spool.insert(&[json!({"recordName":"master","recordType":"CPLMaster","fields":{"resOriginalWidth":{"value":800},"resOriginalHeight":{"value":600},"resOriginalRes":{"value":{"downloadURL":"https://cvws.icloud-content.com/test","size":100,"fileChecksum":"opaque"}}}})]).unwrap();
        let assets = spool
            .assets("123", "private", &json!({"zoneName":"PrimarySync"}))
            .unwrap();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].cloud_master_guid, "master");
        drop(spool);
        assert!(!path.exists());
    }
}
