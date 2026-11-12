use async_trait::async_trait;
use omarchy_photo_wallpapers::{
    storage::{Paths, Store},
    themes::{
        palette::Palette,
        platform::{MANAGED_THEME, Manager, Outcome, Runner},
    },
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn palette(mode: &str) -> Palette {
    let (bg, fg, accent) = if mode == "light" {
        ("#fff8f5", "#201a17", "#885020")
    } else {
        ("#171310", "#ece0d8", "#ffb77a")
    };
    let mut colors = BTreeMap::new();
    for (key, value) in [
        ("background", bg),
        ("foreground", fg),
        ("accent", accent),
        ("selection", bg),
        ("muted", fg),
        ("red", "#ba1a1a"),
        ("green", "#256d32"),
        ("yellow", "#795500"),
        ("blue", "#305da8"),
        ("magenta", "#8e4585"),
        ("cyan", "#006b73"),
        ("color0", bg),
        ("color15", fg),
    ] {
        colors.insert(key.into(), value.into());
    }
    Palette {
        mode: mode.into(),
        colors,
    }
}
struct Desktop {
    home: PathBuf,
    fail: AtomicBool,
}
#[async_trait]
impl Runner for Desktop {
    async fn apply(&self, theme: &str) -> anyhow::Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            anyhow::bail!("theme_apply_failed");
        }
        let current = self.home.join(".local/state/omarchy/current");
        std::fs::create_dir_all(current.join("theme"))?;
        std::fs::write(current.join("theme.name"), theme)?;
        std::fs::copy(
            self.home
                .join(".config/omarchy/themes")
                .join(theme)
                .join("colors.toml"),
            current.join("theme/colors.toml"),
        )?;
        let log = self.home.join("applied");
        let mut text = std::fs::read_to_string(&log).unwrap_or_default();
        text.push_str(theme);
        text.push('\n');
        std::fs::write(log, text)?;
        Ok(())
    }
}
fn setup() -> (tempfile::TempDir, Store, Arc<Desktop>) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let theme = home.join(".config/omarchy/themes/tokyo-night");
    std::fs::create_dir_all(&theme).unwrap();
    std::fs::write(theme.join("colors.toml"), palette("dark").toml().unwrap()).unwrap();
    let current = home.join(".local/state/omarchy/current/theme");
    std::fs::create_dir_all(&current).unwrap();
    std::fs::copy(theme.join("colors.toml"), current.join("colors.toml")).unwrap();
    std::fs::write(current.parent().unwrap().join("theme.name"), "tokyo-night").unwrap();
    let store = Store::open(&Paths::under(&root.path().join("app"))).unwrap();
    let desktop = Arc::new(Desktop {
        home,
        fail: AtomicBool::new(false),
    });
    (root, store, desktop)
}
fn active(home: &Path) -> String {
    std::fs::read_to_string(home.join(".local/state/omarchy/current/theme.name")).unwrap()
}
#[tokio::test]
async fn theme_activation_deduplicates_and_disable_restores_previous_theme_after_restart() {
    let (_root, store, desktop) = setup();
    let manager = Manager::new(store.clone(), desktop.home.clone(), desktop.clone());
    assert_eq!(
        manager.apply(&palette("light")).await.unwrap(),
        Outcome::Applied
    );
    assert_eq!(active(&desktop.home), MANAGED_THEME);
    assert!(
        desktop
            .home
            .join(".config/omarchy/themes")
            .join(MANAGED_THEME)
            .join("light.mode")
            .exists()
    );
    assert_eq!(
        manager.apply(&palette("light")).await.unwrap(),
        Outcome::Unchanged
    );
    assert_eq!(
        std::fs::read_to_string(desktop.home.join("applied"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let restarted = Manager::new(store, desktop.home.clone(), desktop.clone());
    assert_eq!(restarted.restore().await.unwrap(), Outcome::Restored);
    assert_eq!(active(&desktop.home), "tokyo-night");
}
#[tokio::test]
async fn manual_theme_selection_is_preserved_on_further_updates_and_disable() {
    let (_root, store, desktop) = setup();
    let manager = Manager::new(store, desktop.home.clone(), desktop.clone());
    manager.apply(&palette("dark")).await.unwrap();
    desktop.apply("tokyo-night").await.unwrap();
    assert_eq!(
        manager.apply(&palette("light")).await.unwrap(),
        Outcome::Suspended
    );
    assert_eq!(active(&desktop.home), "tokyo-night");
    assert_eq!(manager.restore().await.unwrap(), Outcome::Unchanged);
    assert_eq!(active(&desktop.home), "tokyo-night");
}
#[tokio::test]
async fn manual_edits_and_unowned_theme_directory_are_not_overwritten() {
    let (_root, store, desktop) = setup();
    let folder = desktop
        .home
        .join(".config/omarchy/themes")
        .join(MANAGED_THEME);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("notes.txt"), "user file").unwrap();
    let manager = Manager::new(store, desktop.home.clone(), desktop.clone());
    assert_eq!(
        manager
            .apply(&palette("light"))
            .await
            .err()
            .unwrap()
            .to_string(),
        "theme_destination_not_owned"
    );
    assert_eq!(
        std::fs::read_to_string(folder.join("notes.txt")).unwrap(),
        "user file"
    );
    assert_eq!(active(&desktop.home), "tokyo-night");
    std::fs::remove_dir_all(folder).unwrap();
    manager.apply(&palette("dark")).await.unwrap();
    let active_path = desktop
        .home
        .join(".local/state/omarchy/current/theme/colors.toml");
    std::fs::write(&active_path, palette("light").toml().unwrap()).unwrap();
    assert_eq!(
        manager.apply(&palette("dark")).await.unwrap(),
        Outcome::Suspended
    );
    assert_eq!(
        std::fs::read_to_string(active_path).unwrap(),
        palette("light").toml().unwrap()
    );
}
#[tokio::test]
async fn failed_activation_keeps_original_theme_and_can_be_retried() {
    let (_root, store, desktop) = setup();
    let manager = Manager::new(store, desktop.home.clone(), desktop.clone());
    desktop.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        manager
            .apply(&palette("light"))
            .await
            .err()
            .unwrap()
            .to_string(),
        "theme_apply_failed"
    );
    assert_eq!(active(&desktop.home), "tokyo-night");
    desktop.fail.store(false, Ordering::SeqCst);
    assert_eq!(
        manager.apply(&palette("light")).await.unwrap(),
        Outcome::Applied
    );
    manager.restore().await.unwrap();
    assert_eq!(active(&desktop.home), "tokyo-night");
}
#[tokio::test]
async fn a_suspended_manual_choice_of_the_generated_theme_is_also_preserved() {
    let (_root, store, desktop) = setup();
    let manager = Manager::new(store, desktop.home.clone(), desktop.clone());
    manager.apply(&palette("dark")).await.unwrap();
    desktop.apply("tokyo-night").await.unwrap();
    assert_eq!(
        manager.apply(&palette("light")).await.unwrap(),
        Outcome::Suspended
    );
    desktop.apply(MANAGED_THEME).await.unwrap();
    assert_eq!(manager.restore().await.unwrap(), Outcome::Unchanged);
    assert_eq!(active(&desktop.home), MANAGED_THEME);
}
#[tokio::test]
async fn restoring_when_nothing_is_owned_does_not_write_the_database() {
    let (_root, store, desktop) = setup();
    let manager = Manager::new(store.clone(), desktop.home.clone(), desktop);
    let wal = store.paths.database().with_extension("sqlite-wal");
    let before = std::fs::read(&wal).unwrap();
    assert_eq!(manager.restore().await.unwrap(), Outcome::Unchanged);
    assert!(
        std::fs::read(wal).unwrap() == before,
        "Idle ownership check changed the WAL"
    );
}
