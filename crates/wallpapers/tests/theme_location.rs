use omarchy_photo_wallpapers::{
    storage::{Paths, Store},
    themes::{Location, ThemeSettings, location::Resolver},
};
use serde_json::json;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

#[tokio::test]
async fn current_location_uses_native_coordinates_and_follows_edits_offline() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(&root.path().join("app"))).unwrap();
    let dir = root.path().join(".local/state/omarchy/settings");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("weather.json");
    std::fs::write(
        &path,
        json!({"name":"Fixture","latitude":52.2,"longitude":21.0}).to_string(),
    )
    .unwrap();
    let resolver = Resolver::new().unwrap();
    let one = resolver
        .resolve(&store, &ThemeSettings::default(), root.path(), false)
        .await
        .unwrap();
    assert_eq!(one.location.latitude, 52.2);
    assert!(!one.stale);
    assert_eq!(one.provider, "omarchy-weather");
    std::fs::write(
        &path,
        json!({"name":"Changed","latitude":40.7,"longitude":-74.0}).to_string(),
    )
    .unwrap();
    let two = resolver
        .resolve(&store, &ThemeSettings::default(), root.path(), false)
        .await
        .unwrap();
    assert_eq!(two.location.longitude, -74.0);
    std::fs::remove_file(path).unwrap();
    let cached = resolver
        .resolve(&store, &ThemeSettings::default(), root.path(), false)
        .await
        .unwrap();
    assert!(cached.stale);
    assert_eq!(cached.location.longitude, -74.0);
}

#[tokio::test]
async fn current_provider_is_bounded_validated_and_retains_last_known_on_outage() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(root.path())).unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"nearest_area":[{"latitude":"51.5","longitude":"-0.12","areaName":[{"value":"Fixture"}]}]}))).mount(&server).await;
    let resolver = Resolver::with_endpoints(&server.uri(), &server.uri()).unwrap();
    let one = resolver
        .resolve(&store, &ThemeSettings::default(), root.path(), true)
        .await
        .unwrap();
    assert_eq!(one.location.latitude, 51.5);
    assert!(!one.stale);
    server.reset().await;
    let cached = resolver
        .resolve(&store, &ThemeSettings::default(), root.path(), true)
        .await
        .unwrap();
    assert!(cached.stale);
    assert_eq!(cached.location.longitude, -0.12);
    let empty = Store::open(&Paths::under(&root.path().join("empty"))).unwrap();
    assert_eq!(
        resolver
            .resolve(&empty, &ThemeSettings::default(), root.path(), false)
            .await
            .err()
            .unwrap()
            .to_string(),
        "location_unavailable"
    );
}

#[tokio::test]
async fn fixed_location_needs_no_network_or_mac_and_debug_omits_coordinates() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(root.path())).unwrap();
    let settings = ThemeSettings {
        location_mode: "fixed".into(),
        location: Some(Location {
            latitude: 37.123456,
            longitude: -122.123456,
            name: "Private fixture".into(),
        }),
        ..Default::default()
    };
    let resolver = Resolver::new().unwrap();
    let v = resolver
        .resolve(&store, &settings, root.path(), false)
        .await
        .unwrap();
    assert_eq!(v.provider, "fixed");
    assert!(!v.stale);
    let debug = format!("{settings:?}");
    assert!(!debug.contains("37.123456") && !debug.contains("Private fixture"));
}
