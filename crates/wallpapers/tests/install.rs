use anyhow::Result;
use omarchy_photo_wallpapers::{
    installer::*,
    storage::{Paths, Store},
};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct FakeRunner {
    calls: Mutex<Vec<Vec<String>>>,
}
struct ThemeDesktop {
    home: std::path::PathBuf,
}
impl ThemeDesktop {
    fn activate_theme(&self, name: &str) -> Result<()> {
        let current = self.home.join(".local/state/omarchy/current");
        std::fs::create_dir_all(current.join("theme"))?;
        std::fs::write(current.join("theme.name"), name)?;
        std::fs::copy(
            self.home
                .join(".config/omarchy/themes")
                .join(name)
                .join("colors.toml"),
            current.join("theme/colors.toml"),
        )?;
        Ok(())
    }
}
impl Runner for ThemeDesktop {
    fn run(&self, args: &[String]) -> Result<()> {
        if args.first().is_some_and(|v| v == "env")
            && args.iter().any(|v| v == "OMARCHY_THEME_SKIP_BACKGROUND=1")
        {
            self.activate_theme(args.last().unwrap())?;
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl omarchy_photo_wallpapers::themes::platform::Runner for ThemeDesktop {
    async fn apply(&self, name: &str) -> Result<()> {
        self.activate_theme(name)
    }
}
impl Runner for FakeRunner {
    fn run(&self, args: &[String]) -> Result<()> {
        self.calls.lock().unwrap().push(args.to_vec());
        Ok(())
    }
}
fn setup() -> (
    tempfile::TempDir,
    Store,
    std::path::PathBuf,
    Arc<FakeRunner>,
) {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths {
        config: root.path().join(".config/omarchy-photo-wallpapers"),
        data: root.path().join(".local/share/omarchy-photo-wallpapers"),
        runtime: root.path().join("run"),
    };
    let store = Store::open(&paths).unwrap();
    let binary = root.path().join("fixture-rust-binary");
    std::fs::write(&binary, b"synthetic Rust executable").unwrap();
    (root, store, binary, Arc::new(FakeRunner::default()))
}
#[test]
fn failed_update_restores_a_previously_disabled_service() {
    use std::sync::atomic::{AtomicBool, Ordering};
    #[derive(Default)]
    struct StateRunner {
        enabled: AtomicBool,
        fail_health: AtomicBool,
    }
    impl Runner for StateRunner {
        fn run(&self, args: &[String]) -> Result<()> {
            if args.iter().any(|s| s == "is-enabled") {
                anyhow::ensure!(self.enabled.load(Ordering::SeqCst), "disabled");
            }
            if args.iter().any(|s| s == "enable") {
                self.enabled.store(true, Ordering::SeqCst);
            }
            if args.iter().any(|s| s == "disable") {
                self.enabled.store(false, Ordering::SeqCst);
            }
            if args.last().is_some_and(|s| s == "health") && self.fail_health.load(Ordering::SeqCst)
            {
                anyhow::bail!("not_ready");
            }
            Ok(())
        }
    }
    let (root, store, binary, _) = setup();
    let runner = Arc::new(StateRunner::default());
    let installer = Installer::new(root.path(), &store.paths, runner.clone());
    installer
        .activate_backend(&installer.stage(&binary).unwrap())
        .unwrap();
    runner.enabled.store(false, Ordering::SeqCst);
    runner.fail_health.store(true, Ordering::SeqCst);
    assert!(
        installer
            .activate_backend(&installer.stage(&binary).unwrap())
            .is_err()
    );
    assert!(!runner.enabled.load(Ordering::SeqCst));
}

#[test]
fn installation_ownership_symlink_cannot_authorize_an_update() {
    let (root, store, binary, runner) = setup();
    let installer = Installer::new(root.path(), &store.paths, runner.clone());
    installer
        .activate_backend(&installer.stage(&binary).unwrap())
        .unwrap();
    let owner = store.paths.config.join("install-v2.json");
    let outside = root.path().join("unrelated-owner.json");
    std::fs::rename(&owner, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &owner).unwrap();
    std::fs::write(&binary, b"replacement executable").unwrap();
    let stage = installer.stage(&binary).unwrap();
    runner.calls.lock().unwrap().clear();
    assert!(installer.activate_backend(&stage).is_err());
    assert_eq!(
        std::fs::read(root.path().join(".local/bin/omarchy-photo-wallpapers")).unwrap(),
        b"synthetic Rust executable"
    );
    assert!(
        runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|args| args[0] != "systemctl")
    );
}

#[test]
fn backend_install_rejects_symlinked_destination_parents_before_service_mutation() {
    for relative in [".local/bin", ".config/systemd/user"] {
        let (root, store, binary, runner) = setup();
        let outside = tempfile::tempdir().unwrap();
        let parent = root.path().join(relative);
        std::fs::create_dir_all(parent.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(outside.path(), &parent).unwrap();
        let installer = Installer::new(root.path(), &store.paths, runner.clone());
        let stage = installer.stage(&binary).unwrap();
        runner.calls.lock().unwrap().clear();
        assert!(installer.activate_backend(&stage).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        assert!(
            runner
                .calls
                .lock()
                .unwrap()
                .iter()
                .all(|args| args[0] != "systemctl")
        );
    }
}

#[test]
fn staged_and_activated_install_is_idempotent_and_preserves_unrelated_services() {
    let (root, store, binary, runner) = setup();
    let unrelated = root.path().join(".config/systemd/user/unrelated.service");
    std::fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
    std::fs::write(&unrelated, "unrelated").unwrap();
    let installer = Installer::new(root.path(), &store.paths, runner.clone());
    let stage = installer.stage(&binary).unwrap();
    assert!(stage.join("plugin/manifest.json").exists());
    let staged_manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(stage.join("plugin/manifest.json")).unwrap())
            .unwrap();
    for entry in staged_manifest["entryPoints"].as_object().unwrap().values() {
        let entry = entry.as_str().unwrap();
        assert!(
            stage.join("plugin").join(entry).is_file(),
            "staged manifest must resolve its bundled native entrypoint: {entry}"
        );
    }
    assert!(stage.join("plugin/components/ThemeEditor.qml").exists());
    assert!(stage.join("licenses/sunrise-MIT.txt").exists());
    assert!(
        !root
            .path()
            .join(".local/bin/omarchy-photo-wallpapers")
            .exists()
    );
    installer.activate(&stage).unwrap();
    installer.activate(&stage).unwrap();
    let unit = std::fs::read_to_string(
        root.path()
            .join(".config/systemd/user/omarchy-photo-wallpapers.service"),
    )
    .unwrap();
    assert!(!unit.contains("python") && !unit.contains(".venv"));
    assert!(unit.contains("CPUQuota=40%"));
    assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "unrelated");
    assert_eq!(
        std::fs::read(root.path().join(".local/bin/omarchy-photo-wallpapers")).unwrap(),
        b"synthetic Rust executable"
    );
    assert!(
        root.path()
            .join(".config/omarchy/plugins/domderen.photo-wallpapers/components/AuthEditor.qml")
            .exists()
    );
    store
        .set_meta(
            "retain-private-history",
            &serde_json::json!({"synthetic":true}),
        )
        .unwrap();
    installer.remove(false).unwrap();
    assert!(
        !root
            .path()
            .join(".local/bin/omarchy-photo-wallpapers")
            .exists()
    );
    assert!(store.paths.database().exists());
    assert!(unrelated.exists());
}
#[test]
fn unowned_plugin_is_rejected_before_any_live_mutation() {
    let (root, store, binary, runner) = setup();
    let plugin = root
        .path()
        .join(".config/omarchy/plugins/domderen.photo-wallpapers");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("unknown"), "leave me").unwrap();
    let installer = Installer::new(root.path(), &store.paths, runner);
    let stage = installer.stage(&binary).unwrap();
    assert!(installer.activate(&stage).is_err());
    assert_eq!(
        std::fs::read_to_string(plugin.join("unknown")).unwrap(),
        "leave me"
    );
    assert!(
        !root
            .path()
            .join(".config/systemd/user/omarchy-photo-wallpapers.service")
            .exists()
    );
}
#[tokio::test]
async fn uninstall_restores_owned_theme_before_removal_and_keeps_a_manual_theme_choice() {
    use omarchy_photo_wallpapers::themes::{
        palette::Palette,
        platform::{MANAGED_THEME, Manager, Outcome},
    };
    for manual in [false, true] {
        let (root, store, binary, _) = setup();
        let runner = Arc::new(ThemeDesktop {
            home: root.path().into(),
        });
        let mut colors = std::collections::BTreeMap::new();
        for (key, color) in [
            ("background", "#111111"),
            ("foreground", "#eeeeee"),
            ("accent", "#82aaff"),
            ("selection", "#333333"),
            ("muted", "#aaaaaa"),
            ("red", "#ffb4ab"),
            ("green", "#81c784"),
            ("yellow", "#e5c07b"),
            ("blue", "#82aaff"),
            ("magenta", "#c792ea"),
            ("cyan", "#89ddff"),
            ("color0", "#111111"),
            ("color15", "#eeeeee"),
        ] {
            colors.insert(key.into(), color.into());
        }
        let palette = Palette {
            mode: "dark".into(),
            colors,
        };
        let base = root.path().join(".config/omarchy/themes/tokyo-night");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("colors.toml"), palette.toml().unwrap()).unwrap();
        runner.activate_theme("tokyo-night").unwrap();
        let installer = Installer::new(root.path(), &store.paths, runner.clone());
        let stage = installer.stage(&binary).unwrap();
        installer.activate(&stage).unwrap();
        let manager = Manager::new(store.clone(), root.path().into(), runner.clone());
        manager.apply(&palette).await.unwrap();
        if manual {
            runner.activate_theme("tokyo-night").unwrap();
            assert_eq!(manager.apply(&palette).await.unwrap(), Outcome::Suspended);
            runner.activate_theme(MANAGED_THEME).unwrap();
        }
        installer.remove(false).unwrap();
        let active =
            std::fs::read_to_string(root.path().join(".local/state/omarchy/current/theme.name"))
                .unwrap();
        assert_eq!(active, if manual { MANAGED_THEME } else { "tokyo-night" });
        assert_eq!(
            root.path()
                .join(".config/omarchy/themes")
                .join(MANAGED_THEME)
                .exists(),
            manual
        );
        assert!(store.paths.database().exists());
    }
}
#[test]
fn owned_legacy_symlink_can_be_replaced_without_overwriting_its_target() {
    let (root, store, binary, runner) = setup();
    let command = root.path().join(".local/bin/omarchy-photo-wallpapers");
    std::fs::create_dir_all(command.parent().unwrap()).unwrap();
    let old = root.path().join("old-python-command");
    std::fs::write(&old, "preserve legacy rollback").unwrap();
    std::os::unix::fs::symlink(&old, &command).unwrap();
    omarchy_photo_wallpapers::storage::atomic_json(
        &store.paths.config.join("install.json"),
        &serde_json::json!({"command":command}),
    )
    .unwrap();
    let installer = Installer::new(root.path(), &store.paths, runner);
    let stage = installer.stage(&binary).unwrap();
    installer.activate(&stage).unwrap();
    assert!(!command.is_symlink());
    assert_eq!(
        std::fs::read_to_string(old).unwrap(),
        "preserve legacy rollback"
    );
}

#[derive(Default)]
struct FailStart {
    failed: std::sync::atomic::AtomicBool,
}
impl Runner for FailStart {
    fn run(&self, args: &[String]) -> Result<()> {
        if args.iter().any(|a| a == "enable")
            && !self.failed.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            anyhow::bail!("synthetic service startup failure");
        }
        Ok(())
    }
}
#[test]
fn activation_failure_restores_the_previous_owned_installation() {
    let (root, store, binary, _) = setup();
    let plugin = root
        .path()
        .join(".config/omarchy/plugins/domderen.photo-wallpapers");
    let unit = root
        .path()
        .join(".config/systemd/user/omarchy-photo-wallpapers.service");
    let command = root.path().join(".local/bin/omarchy-photo-wallpapers");
    for path in [
        &plugin,
        &unit.parent().unwrap().to_path_buf(),
        &command.parent().unwrap().to_path_buf(),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(plugin.join("old.qml"), "old native UI").unwrap();
    std::fs::write(&unit, "old service").unwrap();
    std::fs::write(&command, "old executable").unwrap();
    omarchy_photo_wallpapers::storage::atomic_json(
        &store.paths.config.join("install.json"),
        &serde_json::json!({"plugin":plugin,"unit":unit,"command":command}),
    )
    .unwrap();
    let installer = Installer::new(root.path(), &store.paths, Arc::new(FailStart::default()));
    let stage = installer.stage(&binary).unwrap();
    assert!(installer.activate(&stage).is_err());
    assert_eq!(std::fs::read_to_string(unit).unwrap(), "old service");
    assert_eq!(std::fs::read_to_string(command).unwrap(), "old executable");
    assert!(plugin.join("old.qml").exists());
    assert!(!plugin.join("components/AuthEditor.qml").exists());
}

#[test]
fn backend_install_update_and_remove_preserve_a_marketplace_checkout() {
    let (root, store, binary, runner) = setup();
    let plugin = root
        .path()
        .join(".config/omarchy/plugins/domderen.photo-wallpapers");
    std::fs::create_dir_all(plugin.join(".git")).unwrap();
    std::fs::write(plugin.join("manifest.json"), "git-owned manifest").unwrap();
    std::fs::write(plugin.join("local-draft.qml"), "user's unpublished change").unwrap();
    let installer = Installer::new(root.path(), &store.paths, runner.clone());
    let stage = installer.stage(&binary).unwrap();
    installer.activate_backend(&stage).unwrap();
    std::fs::write(&binary, "updated backend").unwrap();
    installer
        .activate_backend(&installer.stage(&binary).unwrap())
        .unwrap();
    assert_eq!(
        std::fs::read(root.path().join(".local/bin/omarchy-photo-wallpapers")).unwrap(),
        b"updated backend"
    );
    installer.remove(false).unwrap();
    assert_eq!(
        std::fs::read_to_string(plugin.join("manifest.json")).unwrap(),
        "git-owned manifest"
    );
    assert_eq!(
        std::fs::read_to_string(plugin.join("local-draft.qml")).unwrap(),
        "user's unpublished change"
    );
    assert!(plugin.join(".git").is_dir());
    assert!(store.paths.database().exists());
    assert!(!runner.calls.lock().unwrap().iter().any(|call| {
        call.first().is_some_and(|a| a == "omarchy-shell")
            || (call.first().is_some_and(|a| a == "omarchy")
                && call.get(2).is_none_or(|a| a != "validate"))
    }));
}

#[test]
fn backend_install_rejects_unowned_binary_without_touching_checkout_or_service() {
    let (root, store, binary, runner) = setup();
    let command = root.path().join(".local/bin/omarchy-photo-wallpapers");
    std::fs::create_dir_all(command.parent().unwrap()).unwrap();
    std::fs::write(&command, "another application").unwrap();
    let installer = Installer::new(root.path(), &store.paths, runner.clone());
    assert!(
        installer
            .activate_backend(&installer.stage(&binary).unwrap())
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(command).unwrap(),
        "another application"
    );
    assert!(
        runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|call| call.iter().all(|a| a != "systemctl"))
    );
}

#[test]
fn failed_backend_update_restores_code_and_ownership_but_never_mutates_checkout() {
    let (root, store, binary, runner) = setup();
    let installer = Installer::new(root.path(), &store.paths, runner);
    installer
        .activate_backend(&installer.stage(&binary).unwrap())
        .unwrap();
    let plugin = root
        .path()
        .join(".config/omarchy/plugins/domderen.photo-wallpapers");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("manifest.json"), "independent Git checkout").unwrap();
    let owner = std::fs::read(store.paths.config.join("install-v2.json")).unwrap();
    let command = root.path().join(".local/bin/omarchy-photo-wallpapers");
    let previous = std::fs::read(&command).unwrap();
    std::fs::write(&binary, "failed update").unwrap();
    let failing = Installer::new(root.path(), &store.paths, Arc::new(FailStart::default()));
    assert!(
        failing
            .activate_backend(&failing.stage(&binary).unwrap())
            .is_err()
    );
    assert_eq!(std::fs::read(command).unwrap(), previous);
    assert_eq!(
        std::fs::read(store.paths.config.join("install-v2.json")).unwrap(),
        owner
    );
    assert_eq!(
        std::fs::read_to_string(plugin.join("manifest.json")).unwrap(),
        "independent Git checkout"
    );
}

struct FailHealth;
impl Runner for FailHealth {
    fn run(&self, args: &[String]) -> Result<()> {
        anyhow::ensure!(args.last().is_none_or(|a| a != "health"), "not_ready");
        Ok(())
    }
}
#[test]
fn successful_systemctl_without_daemon_readiness_rolls_back_update() {
    let (root, store, binary, runner) = setup();
    let installer = Installer::new(root.path(), &store.paths, runner);
    installer
        .activate_backend(&installer.stage(&binary).unwrap())
        .unwrap();
    let command = root.path().join(".local/bin/omarchy-photo-wallpapers");
    let previous = std::fs::read(&command).unwrap();
    let owner = std::fs::read(store.paths.config.join("install-v2.json")).unwrap();
    std::fs::write(&binary, "never became ready").unwrap();
    let update = Installer::new(root.path(), &store.paths, Arc::new(FailHealth));
    assert!(
        update
            .activate_backend(&update.stage(&binary).unwrap())
            .is_err()
    );
    assert_eq!(std::fs::read(command).unwrap(), previous);
    assert_eq!(
        std::fs::read(store.paths.config.join("install-v2.json")).unwrap(),
        owner
    );
}

#[test]
fn removal_and_unsupported_downgrade_require_explicit_privacy_deactivation() {
    let (root, store, binary, runner) = setup();
    let installer = Installer::new(root.path(), &store.paths, runner.clone());
    let stage = installer.stage(&binary).unwrap();
    installer.activate(&stage).unwrap();
    store.set_meeting(true).unwrap();
    runner.calls.lock().unwrap().clear();
    let err = installer.remove(false).unwrap_err().to_string();
    assert_eq!(err, "privacy_deactivation_required");
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(
        root.path()
            .join(".local/bin/omarchy-photo-wallpapers")
            .is_file()
    );
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(stage.join("stage.json")).unwrap()).unwrap();
    manifest["version"] = serde_json::json!("0.4.0");
    std::fs::write(stage.join("stage.json"), manifest.to_string()).unwrap();
    assert_eq!(
        installer.activate(&stage).unwrap_err().to_string(),
        "privacy_deactivation_required"
    );
}

fn failed_privacy_entry_retains_requested_shadow(store: &Store, personal: bool) {
    store
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_privacy_settings BEFORE UPDATE ON meta
         WHEN NEW.key='settings' BEGIN SELECT RAISE(ABORT,'synthetic_write_failure'); END;",
        )
        .unwrap();
    let result = if personal {
        store.update_settings(
            store.settings().unwrap().revision,
            serde_json::json!({"privacy":{"filter_personal":true}}),
        )
    } else {
        store.set_meeting(true)
    };
    assert!(result.is_err());
    assert!(!store.settings().unwrap().privacy.active());
    let shadow: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.paths.data.join("privacy-mode.json")).unwrap())
            .unwrap();
    assert_eq!(shadow["protected"], true);
}

#[test]
fn removal_respects_durable_protection_after_a_failed_settings_commit() {
    for personal in [false, true] {
        let (root, store, binary, runner) = setup();
        let installer = Installer::new(root.path(), &store.paths, runner.clone());
        let stage = installer.stage(&binary).unwrap();
        installer.activate_backend(&stage).unwrap();
        failed_privacy_entry_retains_requested_shadow(&store, personal);
        runner.calls.lock().unwrap().clear();
        assert_eq!(
            installer.remove(false).unwrap_err().to_string(),
            "privacy_deactivation_required"
        );
        assert!(runner.calls.lock().unwrap().is_empty());
        assert!(
            root.path()
                .join(".local/bin/omarchy-photo-wallpapers")
                .is_file()
        );
    }
}

#[test]
fn upgrade_respects_durable_protection_after_a_failed_settings_commit() {
    for personal in [false, true] {
        let (root, store, binary, runner) = setup();
        let installer = Installer::new(root.path(), &store.paths, runner.clone());
        let stage = installer.stage(&binary).unwrap();
        installer.activate_backend(&stage).unwrap();
        failed_privacy_entry_retains_requested_shadow(&store, personal);
        runner.calls.lock().unwrap().clear();
        assert_eq!(
            installer.activate_backend(&stage).unwrap_err().to_string(),
            "privacy_deactivation_required"
        );
        assert!(runner.calls.lock().unwrap().is_empty());
    }
}

#[test]
fn explicit_off_repairs_a_failed_request_before_removal() {
    let (root, store, binary, runner) = setup();
    let installer = Installer::new(root.path(), &store.paths, runner);
    let stage = installer.stage(&binary).unwrap();
    installer.activate_backend(&stage).unwrap();
    failed_privacy_entry_retains_requested_shadow(&store, true);
    store
        .connection()
        .unwrap()
        .execute_batch("DROP TRIGGER fail_privacy_settings")
        .unwrap();
    store.set_meeting(false).unwrap();
    assert_eq!(installer.remove(false).unwrap()["removed"], true);
}

#[test]
fn invalid_durable_protection_fails_closed_without_lifecycle_side_effects() {
    for bytes in [
        b"{".to_vec(),
        br#"{"schema_version":2,"protected":false}"#.to_vec(),
        br#"{"schema_version":1,"protected":"false"}"#.to_vec(),
        vec![b' '; 1025],
    ] {
        let (root, store, binary, runner) = setup();
        let installer = Installer::new(root.path(), &store.paths, runner.clone());
        let stage = installer.stage(&binary).unwrap();
        installer.activate_backend(&stage).unwrap();
        std::fs::write(store.paths.data.join("privacy-mode.json"), bytes).unwrap();
        runner.calls.lock().unwrap().clear();
        assert_eq!(
            installer.remove(false).unwrap_err().to_string(),
            "privacy_deactivation_required"
        );
        assert_eq!(
            installer.activate_backend(&stage).unwrap_err().to_string(),
            "privacy_deactivation_required"
        );
        assert!(runner.calls.lock().unwrap().is_empty());
        store.set_meeting(false).unwrap();
        assert_eq!(installer.remove(false).unwrap()["removed"], true);
    }
}

#[test]
fn linked_durable_protection_is_not_followed_or_overwritten() {
    let (root, store, binary, runner) = setup();
    let installer = Installer::new(root.path(), &store.paths, runner.clone());
    let stage = installer.stage(&binary).unwrap();
    installer.activate_backend(&stage).unwrap();
    let victim = root.path().join("unrelated.json");
    let original = br#"{"schema_version":1,"protected":false}"#;
    std::fs::write(&victim, original).unwrap();
    let shadow = store.paths.data.join("privacy-mode.json");
    let _ = std::fs::remove_file(&shadow);
    std::os::unix::fs::symlink(&victim, &shadow).unwrap();
    runner.calls.lock().unwrap().clear();
    assert_eq!(
        installer.remove(false).unwrap_err().to_string(),
        "privacy_deactivation_required"
    );
    assert_eq!(
        installer.activate_backend(&stage).unwrap_err().to_string(),
        "privacy_deactivation_required"
    );
    assert!(runner.calls.lock().unwrap().is_empty());
    store.set_meeting(false).unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), original);
    assert!(
        !std::fs::symlink_metadata(&shadow)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(installer.remove(false).unwrap()["removed"], true);
}

#[test]
fn maintenance_excludes_concurrent_entry_and_requires_a_confirmed_surface_for_active_upgrade() {
    let (root, store, binary, runner) = setup();
    let installer = Installer::new(root.path(), &store.paths, runner.clone());
    let stage = installer.stage(&binary).unwrap();
    let guard = omarchy_photo_wallpapers::platform::lifecycle_lock(&store.paths, true).unwrap();
    assert_eq!(
        store.set_meeting(true).unwrap_err().to_string(),
        "installation_in_progress"
    );
    assert!(!store.settings().unwrap().privacy.meeting);
    drop(guard);
    store.set_meeting(true).unwrap();
    runner.calls.lock().unwrap().clear();
    assert_eq!(
        installer.activate(&stage).unwrap_err().to_string(),
        "privacy_transition_pending"
    );
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[test]
fn a_rejected_upgrade_never_migrates_the_running_old_database() {
    let (root, store, binary, runner) = setup();
    let paths = store.paths.clone();
    store
        .connection()
        .unwrap()
        .execute_batch("PRAGMA user_version=2")
        .unwrap();
    drop(store);
    let installer = Installer::new(root.path(), &paths, runner);
    let stage = installer.stage(&binary).unwrap();
    let command = root.path().join(".local/bin/omarchy-photo-wallpapers");
    std::fs::create_dir_all(command.parent().unwrap()).unwrap();
    std::fs::write(command, b"not-owned").unwrap();
    assert!(installer.activate(&stage).is_err());
    let db = rusqlite::Connection::open(paths.database()).unwrap();
    let version: u32 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);
}
#[test]
fn failed_format_upgrade_restores_a_database_the_old_backend_can_open() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct FailEnable(AtomicBool);
    impl Runner for FailEnable {
        fn run(&self, args: &[String]) -> Result<()> {
            if args.iter().any(|v| v == "enable") && self.0.swap(false, Ordering::SeqCst) {
                anyhow::bail!("synthetic_activation_failure");
            }
            Ok(())
        }
    }
    let (root, store, binary, runner) = setup();
    let paths = store.paths.clone();
    let first = Installer::new(root.path(), &paths, runner);
    let stage = first.stage(&binary).unwrap();
    first.activate(&stage).unwrap();
    store
        .connection()
        .unwrap()
        .execute_batch("PRAGMA user_version=2")
        .unwrap();
    drop(store);
    let installer = Installer::new(
        root.path(),
        &paths,
        Arc::new(FailEnable(AtomicBool::new(true))),
    );
    assert_eq!(
        installer.activate(&stage).unwrap_err().to_string(),
        "installation_failed_rolled_back"
    );
    let db = rusqlite::Connection::open(paths.database()).unwrap();
    let version: u32 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        version, 2,
        "old runtime must not be restarted against schema3"
    );
}

#[test]
fn removal_preserves_the_retained_database_format() {
    let (root, store, binary, runner) = setup();
    let paths = store.paths.clone();
    let installer = Installer::new(root.path(), &paths, runner);
    let stage = installer.stage(&binary).unwrap();
    installer.activate(&stage).unwrap();
    store
        .connection()
        .unwrap()
        .execute_batch("PRAGMA user_version=2")
        .unwrap();
    drop(store);
    installer.remove(false).unwrap();
    let db = rusqlite::Connection::open(paths.database()).unwrap();
    let version: u32 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);
}

#[test]
fn managed_code_updates_refresh_the_native_shell_and_report_a_deferred_refresh() {
    struct LockedShell;
    impl Runner for LockedShell {
        fn run(&self, args: &[String]) -> Result<()> {
            if args.first().is_some_and(|v| v == "omarchy-restart-shell") {
                anyhow::bail!("session_locked");
            }
            Ok(())
        }
    }
    let (root, store, binary, runner) = setup();
    let installer = Installer::new(root.path(), &store.paths, runner.clone());
    let stage = installer.stage(&binary).unwrap();
    let result = installer.activate(&stage).unwrap();
    assert_eq!(result["shell_restart_required"], false);
    assert!(
        runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|args| args.first().is_some_and(|v| v == "omarchy-restart-shell"))
    );
    let installer = Installer::new(root.path(), &store.paths, Arc::new(LockedShell));
    let result = installer.activate(&stage).unwrap();
    assert_eq!(result["shell_restart_required"], true);
    assert_eq!(result["installed"], true);
}
