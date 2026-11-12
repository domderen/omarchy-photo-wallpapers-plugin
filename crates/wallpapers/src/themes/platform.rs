//! Own one user theme and use only Omarchy's public preserve-background command.
use super::palette::Palette;
use crate::storage::{Store, atomic_bytes, private_dir, read_bounded};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
pub const MANAGED_THEME: &str = "photo-wallpapers";
const MARKER: &[u8] = b"domderen.photo-wallpapers/theme-v1\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    Unchanged,
    Restored,
    Suspended,
}
#[async_trait]
pub trait Runner: Send + Sync {
    async fn apply(&self, theme: &str) -> Result<()>;
}
pub struct SystemRunner;
#[async_trait]
impl Runner for SystemRunner {
    async fn apply(&self, theme: &str) -> Result<()> {
        valid_name(theme)?;
        let status = tokio::time::timeout(
            Duration::from_secs(70),
            tokio::process::Command::new("omarchy-theme-set")
                .arg(theme)
                .env("OMARCHY_THEME_SKIP_BACKGROUND", "1")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("theme_apply_timed_out"))?
        .map_err(|_| anyhow::anyhow!("theme_apply_failed"))?;
        ensure!(status.success(), "theme_apply_failed");
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Ownership {
    previous: String,
    applied: bool,
    digest: Option<String>,
    pending: Option<String>,
    suspended: bool,
}
pub struct Manager {
    store: Store,
    home: PathBuf,
    runner: Arc<dyn Runner>,
}
impl Manager {
    pub fn new(store: Store, home: PathBuf, runner: Arc<dyn Runner>) -> Self {
        Self {
            store,
            home,
            runner,
        }
    }
    pub fn suspended(&self) -> Result<bool> {
        let owner: Option<Ownership> =
            serde_json::from_value(self.store.meta("theme-ownership-v1")?).ok();
        if let Some(mut owner) = owner {
            if owner.suspended {
                return Ok(true);
            }
            if owner.applied && !matches_owner(&read_active(&self.home)?, &owner) {
                owner.suspended = true;
                self.save(&owner)?;
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub async fn apply(&self, palette: &Palette) -> Result<Outcome> {
        let text = palette.toml()?;
        let digest = hash(text.as_bytes());
        let active = read_active(&self.home)?;
        let previous: Option<Ownership> =
            serde_json::from_value(self.store.meta("theme-ownership-v1")?).ok();
        if let Some(mut owner) = previous.clone() {
            if owner.suspended {
                return Ok(Outcome::Suspended);
            }
            if (owner.applied && !matches_owner(&active, &owner))
                || (!owner.applied && active.name != owner.previous && active.name != MANAGED_THEME)
            {
                owner.suspended = true;
                self.save(&owner)?;
                return Ok(Outcome::Suspended);
            }
            if active.name == MANAGED_THEME
                && active.digest.as_ref() == Some(&digest)
                && owner.digest.as_ref() == Some(&digest)
            {
                return Ok(Outcome::Unchanged);
            }
        } else {
            ensure!(active.name != MANAGED_THEME, "theme_ownership_lost");
        }
        let folder = self.folder();
        claim(&folder)?;
        let mut owner = previous.unwrap_or(Ownership {
            previous: active.name.clone(),
            applied: false,
            digest: None,
            pending: None,
            suspended: false,
        });
        owner.pending = Some(digest.clone());
        self.save(&owner)?;
        atomic_bytes(&folder.join("colors.toml"), text.as_bytes())?;
        if palette.mode == "light" {
            atomic_bytes(&folder.join("light.mode"), b"")?;
        } else if folder.join("light.mode").exists() {
            std::fs::remove_file(folder.join("light.mode"))?;
        }
        let result = self.runner.apply(MANAGED_THEME).await;
        let after = read_active(&self.home)?;
        if after.name == MANAGED_THEME && after.digest.as_ref() == Some(&digest) {
            owner.applied = true;
            owner.digest = Some(digest);
            owner.pending = None;
            self.save(&owner)?;
            result?;
            return Ok(Outcome::Applied);
        }
        owner.pending = None;
        if after.name != active.name {
            owner.suspended = true;
        }
        self.save(&owner)?;
        result?;
        anyhow::bail!("theme_apply_failed")
    }
    pub async fn restore(&self) -> Result<Outcome> {
        if let Some(previous) = pending_restore(&self.store, &self.home)? {
            self.runner.apply(&previous).await?;
            ensure!(
                read_active(&self.home)?.name == previous,
                "theme_restore_failed"
            );
            clear_ownership(&self.store)?;
            Ok(Outcome::Restored)
        } else {
            clear_ownership(&self.store)?;
            Ok(Outcome::Unchanged)
        }
    }
    fn save(&self, owner: &Ownership) -> Result<()> {
        self.store
            .set_meta("theme-ownership-v1", &serde_json::to_value(owner)?)
    }
    fn folder(&self) -> PathBuf {
        self.home.join(".config/omarchy/themes").join(MANAGED_THEME)
    }
}
struct Active {
    name: String,
    digest: Option<String>,
}
fn read_active(home: &Path) -> Result<Active> {
    let current = home.join(".local/state/omarchy/current");
    let name = String::from_utf8(read_bounded(&current.join("theme.name"), 512)?)
        .map_err(|_| anyhow::anyhow!("theme_environment_unavailable"))?
        .trim()
        .to_string();
    valid_name(&name)?;
    let digest = read_bounded(&current.join("theme/colors.toml"), 64 * 1024)
        .ok()
        .map(|v| hash(&v));
    Ok(Active { name, digest })
}
fn matches_owner(active: &Active, owner: &Ownership) -> bool {
    active.name == MANAGED_THEME
        && active.digest.is_some()
        && (active.digest == owner.digest || active.digest == owner.pending)
}
fn valid_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 160
            && !name.starts_with('.')
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_'),
        "invalid_theme_name"
    );
    Ok(())
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn claim(folder: &Path) -> Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(folder) {
        ensure!(
            meta.is_dir()
                && !meta.file_type().is_symlink()
                && read_bounded(&folder.join(".photo-wallpapers-owner"), 128)
                    .ok()
                    .as_deref()
                    == Some(MARKER),
            "theme_destination_not_owned"
        );
    } else {
        private_dir(folder)?;
        atomic_bytes(&folder.join(".photo-wallpapers-owner"), MARKER)?;
    }
    Ok(())
}
pub fn pending_restore(store: &Store, home: &Path) -> Result<Option<String>> {
    let owner: Option<Ownership> = serde_json::from_value(store.meta("theme-ownership-v1")?).ok();
    if let Some(owner) = owner {
        valid_name(&owner.previous)?;
        if !owner.suspended && matches_owner(&read_active(home)?, &owner) {
            return Ok(Some(owner.previous));
        }
    }
    Ok(None)
}
pub fn clear_ownership(store: &Store) -> Result<()> {
    if !store.meta("theme-ownership-v1")?.is_null() {
        store.set_meta("theme-ownership-v1", &Value::Null)?;
    }
    Ok(())
}
pub fn remove_owned_theme(home: &Path) -> Result<()> {
    let folder = home.join(".config/omarchy/themes").join(MANAGED_THEME);
    let owned = std::fs::symlink_metadata(&folder)
        .ok()
        .is_some_and(|m| m.is_dir() && !m.file_type().is_symlink())
        && read_bounded(&folder.join(".photo-wallpapers-owner"), 128)
            .ok()
            .as_deref()
            == Some(MARKER);
    if !owned
        || read_active(home)
            .map(|v| v.name == MANAGED_THEME)
            .unwrap_or(true)
    {
        return Ok(());
    }
    // Delete only files emitted by this application; retain any user additions.
    for name in ["colors.toml", "light.mode", ".photo-wallpapers-owner"] {
        let path = folder.join(name);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() || m.file_type().is_symlink())
        {
            std::fs::remove_file(path)?;
        }
    }
    if std::fs::read_dir(&folder)?.next().is_none() {
        std::fs::remove_dir(folder)?;
    }
    Ok(())
}
