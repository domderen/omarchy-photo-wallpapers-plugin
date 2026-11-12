use omarchy_photo_wallpapers::installer::shortcuts;
use serde_json::json;
#[test]
fn shortcuts_preserve_user_bindings_and_are_idempotent_and_owned() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join(".config/hypr/bindings.lua");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let original = "-- my bindings\no.bind(\"SUPER + R\", \"My app\", \"my-app\")\n";
    std::fs::write(&file, original).unwrap();
    let binary = root.path().join(".local/bin/omarchy-photo-wallpapers");
    assert!(shortcuts::install(root.path(), &binary, &json!([])).unwrap());
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with(original));
    assert!(text.contains("meeting on") && text.contains("meeting off"));
    assert!(!text.contains("toggle"));
    assert!(shortcuts::install(root.path(), &binary, &json!([])).unwrap());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    shortcuts::remove(root.path(), &binary).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
}
#[test]
fn conflicting_live_bindings_or_substituted_paths_are_never_replaced() {
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("app");
    assert!(
        !shortcuts::install(
            root.path(),
            &binary,
            &json!([{"modmask":68,"key":"M","description":"Other application"}])
        )
        .unwrap()
    );
    assert!(!root.path().join(".config/hypr/bindings.lua").exists());
    let external = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".config")).unwrap();
    std::os::unix::fs::symlink(external.path(), root.path().join(".config/hypr")).unwrap();
    assert!(shortcuts::install(root.path(), &binary, &json!([])).is_err());
    assert_eq!(std::fs::read_dir(external.path()).unwrap().count(), 0);
}
