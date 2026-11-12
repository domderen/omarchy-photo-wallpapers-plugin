//! Read-only preflight and a consistent pre-migration rollback snapshot.
use crate::{
    model::Settings,
    storage::{Paths, private_dir, private_file, read_bounded, validate_private_parents},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension,
    backup::{Backup, StepResult},
};
use rustix::fs::OFlags;
use serde_json::Value;
use std::{
    fs::OpenOptions,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};
fn sidecars(path: &Path) -> Vec<std::path::PathBuf> {
    ["-wal", "-shm", "-journal"]
        .iter()
        .map(|suffix| {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            name.into()
        })
        .collect()
}
fn open(path: &Path, write: bool) -> Result<Connection> {
    validate_private_parents(path)?;
    for family in sidecars(path) {
        if std::fs::symlink_metadata(&family).is_ok() {
            let _ = private_file(&family, OFlags::RDONLY)?;
        }
    }
    let _guard = private_file(path, if write { OFlags::RDWR } else { OFlags::RDONLY })?;
    let db = Connection::open_with_flags(
        path.parent()
            .unwrap()
            .canonicalize()?
            .join(path.file_name().unwrap()),
        OpenFlags::SQLITE_OPEN_NOFOLLOW
            | if write {
                OpenFlags::SQLITE_OPEN_READ_WRITE
            } else {
                OpenFlags::SQLITE_OPEN_READ_ONLY
            },
    )?;
    db.busy_timeout(Duration::from_secs(5))?;
    Ok(db)
}
pub fn preflight(paths: &Paths) -> Result<(u32, Settings, Value)> {
    if !paths.database().exists() {
        return Ok((0, Settings::default(), Value::Null));
    }
    let db = open(&paths.database(), false)?;
    let version = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    ensure!(version <= 3, "unsupported_storage_version");
    let settings: String =
        db.query_row("SELECT value FROM meta WHERE key='settings'", [], |r| {
            r.get(0)
        })?;
    let state: Option<String> = db
        .query_row(
            "SELECT value FROM meta WHERE key='privacy-state-v1'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok((
        version,
        serde_json::from_str(&settings)?,
        state
            .map(|v| serde_json::from_str(&v))
            .transpose()?
            .unwrap_or(Value::Null),
    ))
}
/// A requested cover survives a failed settings transaction. Lifecycle operations
/// must not remove it merely because the database still contains the old flags.
/// Missing is the normal pre-0.5/default state; unreadable or invalid is protected.
pub fn durable_protection_requested(paths: &Paths) -> Result<bool> {
    let read = || -> Result<bool> {
        let path = paths.data.join("privacy-mode.json");
        validate_private_parents(&path)?;
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            result => {
                result?;
            }
        }
        let value: Value = serde_json::from_slice(&read_bounded(&path, 1024)?)?;
        ensure!(value["schema_version"] == 1, "invalid_privacy_shadow");
        value["protected"]
            .as_bool()
            .context("invalid_privacy_shadow")
    };
    read().context("privacy_deactivation_required")
}
pub fn snapshot(paths: &Paths, target: &Path) -> Result<()> {
    private_dir(target.parent().unwrap())?;
    let _new = private_file(target, OFlags::CREATE | OFlags::EXCL | OFlags::RDWR)?;
    let source = open(&paths.database(), false)?;
    let mut destination = open(target, true)?;
    {
        let backup = Backup::new(&source, &mut destination)?;
        let started = Instant::now();
        loop {
            ensure!(
                started.elapsed() < Duration::from_secs(30),
                "database_snapshot_timeout"
            );
            match backup.step(256)? {
                StepResult::Done => break,
                StepResult::More | StepResult::Busy | StepResult::Locked => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => anyhow::bail!("database_snapshot_failed"),
            }
        }
    }
    let check: String = destination.query_row("PRAGMA quick_check(1)", [], |r| r.get(0))?;
    ensure!(check == "ok", "database_snapshot_invalid");
    destination.close().map_err(|(_, e)| e)?;
    std::fs::File::open(target)?.sync_all()?;
    Ok(())
}
/// Caller must have stopped the daemon and hold the exclusive lifecycle lock.
pub fn restore(paths: &Paths, snapshot: &Path) -> Result<()> {
    let _ = open(snapshot, false)?;
    for path in std::iter::once(paths.database()).chain(sidecars(&paths.database())) {
        validate_private_parents(&path)?;
        if std::fs::symlink_metadata(&path).is_ok() {
            let _file = private_file(&path, OFlags::RDONLY)?;
        }
    }
    let target = paths.database();
    let temporary = paths
        .data
        .join(format!(".rollback-{}.sqlite", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut source = private_file(snapshot, OFlags::RDONLY)?;
        let mut copy = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        std::io::copy(&mut source, &mut copy)?;
        copy.sync_all()?;
        drop(copy);
        for path in sidecars(&target) {
            if std::fs::symlink_metadata(&path).is_ok() {
                std::fs::remove_file(path)?;
            }
        }
        std::fs::rename(&temporary, &target)?;
        std::fs::File::open(&paths.data)?.sync_all()?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temporary);
    result
}
