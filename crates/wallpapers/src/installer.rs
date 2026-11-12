//! Self-contained Rust user installation. Staging never changes the running installation.
use crate::{
    migration,
    storage::{Paths, Store, atomic_json, private_dir, read_bounded, validate_private_parents},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
mod database;
pub mod shortcuts;
const ID: &str = "domderen.photo-wallpapers";
macro_rules! bundle {($($name:literal),*$(,)?)=>{&[("manifest.json",include_str!("../../../manifest.json")),$(($name,include_str!(concat!("../../../plugin/",$name)))),*]};}
const FILES: &[(&str, &str)] = bundle!(
    "BarWidget.qml",
    "Service.qml",
    "Settings.qml",
    "components/AuthEditor.qml",
    "components/ConnectionEditor.qml",
    "components/Controls.qml",
    "components/EngineBridge.qml",
    "components/IntervalEditor.qml",
    "components/PhotoPreview.qml",
    "components/PrivacyEditor.qml",
    "components/PrivacyOverlay.qml",
    "components/SourcePicker.qml",
    "components/StatusSummary.qml",
    "components/ThemeEditor.qml"
);
pub trait Runner: Send + Sync {
    fn run(&self, args: &[String]) -> Result<()>;
    fn active_bindings(&self) -> Result<Value> {
        anyhow::bail!("shortcut_snapshot_unavailable")
    }
}
pub struct SystemRunner;
impl Runner for SystemRunner {
    fn active_bindings(&self) -> Result<Value> {
        let result = std::process::Command::new("hyprctl")
            .args(["-j", "binds"])
            .output()?;
        ensure!(
            result.status.success() && result.stdout.len() <= 4 * 1024 * 1024,
            "shortcut_snapshot_unavailable"
        );
        Ok(serde_json::from_slice(&result.stdout)?)
    }

    fn run(&self, args: &[String]) -> Result<()> {
        ensure!(!args.is_empty(), "invalid_install_command");
        let result = std::process::Command::new(&args[0])
            .args(&args[1..])
            .output()
            .context("installation_command_unavailable")?;
        ensure!(result.status.success(), "installation_command_failed");
        Ok(())
    }
}
pub struct Installer {
    home: PathBuf,
    paths: Paths,
    runner: Arc<dyn Runner>,
}
impl Installer {
    pub fn new(home: &Path, paths: &Paths, runner: Arc<dyn Runner>) -> Self {
        Self {
            home: home.into(),
            paths: paths.clone(),
            runner,
        }
    }
    fn plugin(&self) -> PathBuf {
        self.home.join(".config/omarchy/plugins").join(ID)
    }
    fn unit(&self) -> PathBuf {
        self.home
            .join(".config/systemd/user/omarchy-photo-wallpapers.service")
    }
    fn command(&self) -> PathBuf {
        self.home.join(".local/bin/omarchy-photo-wallpapers")
    }
    fn owner(&self) -> PathBuf {
        self.paths.config.join("install-v2.json")
    }
    fn run(&self, args: &[&str]) -> Result<()> {
        self.runner
            .run(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }
    fn validate(&self, path: &Path) -> Result<()> {
        self.runner.run(&[
            "omarchy".into(),
            "plugin".into(),
            "validate".into(),
            path.to_str().context("invalid_install_path")?.into(),
        ])
    }
    pub fn stage(&self, binary: &Path) -> Result<PathBuf> {
        ensure!(binary.is_file(), "rust_binary_required");
        let stage = self
            .paths
            .data
            .join("install-stage")
            .join(uuid::Uuid::new_v4().to_string());
        private_dir(&stage)?;
        for (name, text) in FILES {
            let content = if *name == "manifest.json" {
                let mut manifest: Value = serde_json::from_str(text)?;
                for entry in manifest["entryPoints"]
                    .as_object_mut()
                    .context("invalid_bundled_manifest")?
                    .values_mut()
                {
                    *entry = Value::String(
                        entry
                            .as_str()
                            .and_then(|s| s.strip_prefix("plugin/"))
                            .context("invalid_bundled_entrypoint")?
                            .to_owned(),
                    );
                }
                serde_json::to_vec_pretty(&manifest)?
            } else {
                text.as_bytes().to_vec()
            };
            write_file(&stage.join("plugin").join(name), &content, 0o600)?;
        }
        write_file(
            &stage.join("omarchy-photo-wallpapers"),
            &fs::read(binary)?,
            0o755,
        )?;
        write_file(
            &stage.join("omarchy-photo-wallpapers.service"),
            self.unit_text().as_bytes(),
            0o600,
        )?;
        write_file(
            &stage.join("licenses/kei-MIT.txt"),
            include_bytes!("../../../third_party/kei-MIT.txt"),
            0o600,
        )?;
        write_file(
            &stage.join("licenses/SeetaFace-BSD-2-Clause.txt"),
            include_bytes!("../../../third_party/SeetaFace-BSD-2-Clause.txt"),
            0o600,
        )?;
        write_file(
            &stage.join("licenses/sunrise-MIT.txt"),
            include_bytes!("../../../third_party/sunrise-MIT.txt"),
            0o600,
        )?;
        atomic_json(
            &stage.join("stage.json"),
            &json!({"application":ID,"version":env!("CARGO_PKG_VERSION"),"home":self.home}),
        )?;
        self.validate(&stage.join("plugin"))?;
        Ok(stage)
    }
    fn ownership(&self) -> Result<Value> {
        if self.owner().exists() {
            let value: Value = serde_json::from_slice(&read_bounded(&self.owner(), 65536)?)?;
            ensure!(
                value["application"] == ID && value["home"] == json!(self.home),
                "invalid_install_ownership"
            );
            let mut owner = json!({"unit":self.unit(),"command":self.command()});
            if value["plugin_managed"].as_bool().unwrap_or(true) {
                owner["plugin"] = json!(self.plugin());
            }
            return Ok(owner);
        }
        let path = self.paths.config.join("install.json");
        if path.exists() {
            return Ok(serde_json::from_slice(&read_bounded(&path, 65536)?)?);
        }
        Ok(json!({}))
    }
    fn verify_owned(&self, include_plugin: bool) -> Result<()> {
        let owner = self.ownership()?;
        for (key, path) in [
            ("plugin", self.plugin()),
            ("unit", self.unit()),
            ("command", self.command()),
        ] {
            if key == "plugin" && !include_plugin {
                continue;
            }
            validate_private_parents(&path)?;
            if key == "plugin" {
                ensure!(
                    !path.join(".git").exists(),
                    "git_managed_plugin_requires_backend_only"
                );
            }
            if fs::symlink_metadata(&path).is_ok() {
                ensure!(owner[key] == json!(path), "unowned_installation_path");
            }
        }
        Ok(())
    }
    pub fn activate(&self, stage: &Path) -> Result<Value> {
        self.activate_kind(stage, true)
    }
    /// Omarchy owns its checkout. This mode never writes, enables, disables or removes it.
    pub fn activate_backend(&self, stage: &Path) -> Result<Value> {
        self.activate_kind(stage, false)
    }
    fn activate_kind(&self, stage: &Path, plugin_managed: bool) -> Result<Value> {
        let _lifecycle = crate::platform::lifecycle_lock(&self.paths, true)?;
        let manifest: Value =
            serde_json::from_slice(&read_bounded(&stage.join("stage.json"), 65536)?)?;
        ensure!(
            manifest["application"] == ID && manifest["home"] == json!(self.home),
            "invalid_install_stage"
        );
        let (database_version, settings, privacy_state) = database::preflight(&self.paths)?;
        ensure!(
            !database::durable_protection_requested(&self.paths)? || settings.privacy.active(),
            "privacy_deactivation_required"
        );
        if settings.privacy.active() {
            let version = manifest["version"].as_str().unwrap_or("");
            let parts: Vec<_> = version
                .split('.')
                .filter_map(|n| n.parse::<u64>().ok())
                .collect();
            ensure!(
                parts.len() == 3 && (parts[0], parts[1], parts[2]) >= (0, 5, 0),
                "privacy_deactivation_required"
            );
            ensure!(
                privacy_state["state"] == "active",
                "privacy_transition_pending"
            );
        }
        self.validate(&stage.join("plugin"))?;
        self.verify_owned(plugin_managed)?;
        let backup = self
            .paths
            .data
            .join("migration-backups")
            .join(uuid::Uuid::new_v4().to_string());
        private_dir(&backup)?;
        let managed: Vec<_> = [
            ("plugin", self.plugin()),
            ("unit", self.unit()),
            ("command", self.command()),
            ("install.json", self.paths.config.join("install.json")),
            ("install-v2.json", self.owner()),
        ]
        .into_iter()
        .filter(|(name, _)| plugin_managed || *name != "plugin")
        .collect();
        let was_active = self.unit().exists()
            && self
                .run(&[
                    "systemctl",
                    "--user",
                    "is-active",
                    "--quiet",
                    "omarchy-photo-wallpapers.service",
                ])
                .is_ok();
        let was_enabled = self.unit().exists()
            && self
                .run(&[
                    "systemctl",
                    "--user",
                    "is-enabled",
                    "--quiet",
                    "omarchy-photo-wallpapers.service",
                ])
                .is_ok();
        for (name, path) in &managed {
            if fs::symlink_metadata(path).is_ok() {
                copy_owned(path, &backup.join(name))?;
            }
        }
        let database_backup = backup.join("pre-migration.sqlite");
        if database_version > 0 && database_version < 3 {
            database::snapshot(&self.paths, &database_backup)?;
        }
        let result = (|| -> Result<Value> {
            if self.unit().exists() {
                self.run(&[
                    "systemctl",
                    "--user",
                    "stop",
                    "omarchy-photo-wallpapers.service",
                ])?;
            }
            let store = Store::open(&self.paths)?;
            let migration = migration::discover_legacy(&store)?;
            if plugin_managed {
                for (name, _) in FILES {
                    let source = stage.join("plugin").join(name);
                    write_file(&self.plugin().join(name), &fs::read(source)?, 0o600)?;
                }
            }
            write_file(
                &self.command(),
                &fs::read(stage.join("omarchy-photo-wallpapers"))?,
                0o755,
            )?;
            write_file(
                &self.unit(),
                &fs::read(stage.join("omarchy-photo-wallpapers.service"))?,
                0o600,
            )?;
            copy_owned(&stage.join("licenses"), &self.paths.data.join("licenses"))?;
            if plugin_managed {
                self.validate(&self.plugin())?;
            }
            atomic_json(
                &self.owner(),
                &json!({"application":ID,"home":self.home,"version":env!("CARGO_PKG_VERSION"),"plugin_managed":plugin_managed,"rollback":backup}),
            )?;
            self.run(&["systemctl", "--user", "daemon-reload"])?;
            self.run(&[
                "systemctl",
                "--user",
                "enable",
                "--now",
                "omarchy-photo-wallpapers.service",
            ])?;
            let mut ready = false;
            for _ in 0..20 {
                if self
                    .runner
                    .run(&[
                        self.command().to_string_lossy().into_owned(),
                        "health".into(),
                    ])
                    .is_ok()
                {
                    ready = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            ensure!(ready, "backend_not_ready");
            if plugin_managed {
                self.run(&["omarchy-shell", "shell", "rescanPlugins"])?;
                self.run(&["omarchy", "plugin", "enable", ID])?;
            }
            // Rescan alone can retain Qt's old imported component types. Use
            // Omarchy's public, lock-aware restart instead of touching packages.
            let shell_restart_required =
                plugin_managed && self.run(&["omarchy-restart-shell"]).is_err();
            let shortcuts = match self
                .runner
                .active_bindings()
                .and_then(|bindings| shortcuts::install(&self.home, &self.command(), &bindings))
            {
                Ok(true) => {
                    if self.run(&["hyprctl", "reload"]).is_ok() {
                        "installed"
                    } else {
                        "reload_required"
                    }
                }
                Ok(false) => "conflict",
                Err(_) => "unavailable",
            };
            Ok(
                json!({"shell_restart_required":shell_restart_required,"meeting_shortcuts":shortcuts,"installed":true,"backend":"rust","plugin_managed":plugin_managed,"legacy_imported":migration.imported,"rollback_preserved":true}),
            )
        })();
        if result.is_err() {
            ensure!(
                self.run(&[
                    "systemctl",
                    "--user",
                    "stop",
                    "omarchy-photo-wallpapers.service",
                ])
                .is_ok(),
                "installation_failed_rollback_required"
            );
            let restored = (|| -> Result<()> {
                if !was_enabled && self.unit().exists() {
                    // Undo enable --now before removing/restoring the unit;
                    // otherwise failure changes the previous startup policy.
                    self.run(&[
                        "systemctl",
                        "--user",
                        "disable",
                        "omarchy-photo-wallpapers.service",
                    ])?;
                }
                for (name, path) in &managed {
                    if let Ok(metadata) = fs::symlink_metadata(path) {
                        if metadata.is_dir() {
                            fs::remove_dir_all(path)?;
                        } else {
                            fs::remove_file(path)?;
                        }
                    }
                    if fs::symlink_metadata(backup.join(name)).is_ok() {
                        copy_owned(&backup.join(name), path)?;
                    }
                }
                if database_backup.exists() {
                    database::restore(&self.paths, &database_backup)?;
                }
                self.run(&["systemctl", "--user", "daemon-reload"])?;
                if was_active {
                    self.run(&[
                        "systemctl",
                        "--user",
                        "start",
                        "omarchy-photo-wallpapers.service",
                    ])?;
                }
                if plugin_managed {
                    let _ = self.run(&["omarchy-shell", "shell", "rescanPlugins"]);
                }
                Ok(())
            })();
            ensure!(restored.is_ok(), "installation_failed_rollback_required");
            anyhow::bail!("installation_failed_rolled_back");
        }
        result
    }
    pub fn remove(&self, purge: bool) -> Result<Value> {
        let _lifecycle = crate::platform::lifecycle_lock(&self.paths, true)?;
        ensure!(
            !database::preflight(&self.paths)?.1.privacy.active()
                && !database::durable_protection_requested(&self.paths)?,
            "privacy_deactivation_required"
        );
        let owner = self.ownership()?;
        self.verify_owned(owner.get("plugin").is_some())?;
        ensure!(
            owner.is_object() && !owner.as_object().unwrap().is_empty(),
            "installation_not_owned"
        );
        let shortcut_removed = shortcuts::remove(&self.home, &self.command()).is_ok();
        if shortcut_removed {
            let _ = self.run(&["hyprctl", "reload"]);
        }
        if owner.get("unit").is_some() {
            self.run(&[
                "systemctl",
                "--user",
                "disable",
                "--now",
                "omarchy-photo-wallpapers.service",
            ])?;
        }
        if self.paths.database().exists() {
            let store = Store::existing_without_migration(&self.paths)?;
            if let Some(previous) = crate::themes::platform::pending_restore(&store, &self.home)? {
                self.run(&[
                    "env",
                    "OMARCHY_THEME_SKIP_BACKGROUND=1",
                    "omarchy-theme-set",
                    &previous,
                ])?;
            }
            crate::themes::platform::clear_ownership(&store)?;
            crate::themes::platform::remove_owned_theme(&self.home)?;
        }
        if owner.get("unit").is_some() {
            if self.unit().exists() {
                fs::remove_file(self.unit())?;
            }
            self.run(&["systemctl", "--user", "daemon-reload"])?;
        }
        if owner.get("plugin").is_some() {
            self.run(&["omarchy", "plugin", "disable", ID])?;
            if self.plugin().exists() {
                fs::remove_dir_all(self.plugin())?;
            }
            self.run(&["omarchy-shell", "shell", "rescanPlugins"])?;
        }
        if fs::symlink_metadata(self.command()).is_ok() {
            fs::remove_file(self.command())?;
        }
        if purge {
            if self.paths.config.exists() {
                fs::remove_dir_all(&self.paths.config)?;
            }
            if self.paths.data.exists() {
                fs::remove_dir_all(&self.paths.data)?;
            }
        } else {
            let _ = fs::remove_file(self.owner());
            let _ = fs::remove_file(self.paths.config.join("install.json"));
        }
        Ok(
            json!({"removed":true,"private_data_preserved":!purge,"shortcuts_removed":shortcut_removed}),
        )
    }
    fn unit_text(&self) -> String {
        let command = self
            .command()
            .to_string_lossy()
            .replace('%', "%%")
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        format!(
            "[Unit]\nDescription=Independent iCloud Photo Wallpapers (Rust)\nAfter=graphical-session.target\nPartOf=graphical-session.target\n\n[Service]\nType=exec\nExecStart=\"{command}\" daemon\nEnvironment=PATH=%h/.local/bin:/usr/share/omarchy/bin:/usr/bin:/bin\nEnvironment=OMARCHY_PATH=/usr/share/omarchy\nEnvironment=DBUS_SESSION_BUS_ADDRESS=unix:path=%t/bus\nUMask=0077\nCPUQuota=40%\nMemoryMax=1G\nNice=10\nIOSchedulingClass=idle\nRestart=on-failure\nRestartSec=10\nTimeoutStopSec=75\n\n[Install]\nWantedBy=graphical-session.target\n"
        )
    }
}
fn write_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    validate_private_parents(path)?;
    let parent = path.parent().context("invalid_install_path")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{}.install", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(mode)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}
fn copy_owned(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        let link = fs::read_link(source)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::symlink_metadata(target).is_ok() {
            fs::remove_file(target)?;
        }
        std::os::unix::fs::symlink(link, target)?;
    } else if metadata.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_owned(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else {
        write_file(
            target,
            &fs::read(source)?,
            metadata.permissions().mode() & 0o777,
        )?;
    }
    Ok(())
}
