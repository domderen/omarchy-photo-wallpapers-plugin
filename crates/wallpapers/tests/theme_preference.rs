use omarchy_photo_wallpapers::{
    model::{Asset, Membership, Source, SourceRef},
    storage::{Paths, Store},
    themes::preference,
};
use serde_json::json;

#[test]
fn photo_content_measurement_ignores_fit_padding_and_survives_removed_thumbnail() {
    use omarchy_photo_wallpapers::{
        imaging::Rect,
        themes::{palette, preference::ContentLayout},
    };
    let (_root, store) = setup();
    let mut thumb = image::RgbImage::from_pixel(256, 160, image::Rgb([0, 0, 0]));
    for y in 0..160 {
        for x in 88..168 {
            thumb.put_pixel(x, y, image::Rgb([230, 230, 230]));
        }
    }
    let path = palette::thumbnail_path(&store, "portrait");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    thumb.save(&path).unwrap();
    let layout = ContentLayout {
        width: 1600,
        height: 1000,
        rect: Rect {
            x: 0,
            y: 0,
            width: 500,
            height: 1000,
        },
        mode: "fit".into(),
    };
    store
        .set_meta("suitability-v1:portrait", &json!({"day_score":0.35}))
        .unwrap();
    let score = preference::measure_content(&store, "portrait", &layout)
        .unwrap()
        .unwrap();
    assert!(
        score.day_score > 0.9,
        "bright photo must not be classified by its dark padding"
    );
    assert!(preference::bonus(&store, "portrait", "portrait", "light").unwrap() > 0.5);
    std::fs::remove_file(path).unwrap();
    assert_eq!(
        preference::measure_content(&store, "portrait", &layout)
            .unwrap()
            .unwrap()
            .day_score,
        score.day_score
    );
    store
        .feedback("portrait", "period", Some(json!({"period":"night"})))
        .unwrap();
    assert!(preference::bonus(&store, "portrait", "portrait", "light").unwrap() < 0.0);
}

#[test]
fn missing_or_invalid_content_geometry_keeps_legacy_fallback() {
    use omarchy_photo_wallpapers::themes::preference::ContentLayout;
    let (_root, store) = setup();
    assert!(
        preference::measure_content(&store, "unknown", &ContentLayout::default())
            .unwrap()
            .is_none()
    );
    store
        .set_meta("suitability-v1:unknown", &json!({"day_score":0.8}))
        .unwrap();
    store
        .set_meta("suitability-v2:unknown", &json!({"day_score":-20}))
        .unwrap();
    assert!(preference::bonus(&store, "one", "unknown", "light").unwrap() > 0.0);
}

#[test]
fn explicit_mode_wins_over_a_stale_theme_actor_and_disabled_preferences_are_neutral() {
    use omarchy_photo_wallpapers::model::Settings;
    let mut settings = Settings {
        match_colors: true,
        ..Default::default()
    };
    settings.theme.mode = "light".into();
    assert_eq!(
        preference::mode(&settings, &json!({"mode":"dark"})),
        Some("light")
    );
    settings.theme.mode = "auto".into();
    assert_eq!(
        preference::mode(&settings, &json!({"mode":"dark","mode_source":"solar"})),
        Some("dark")
    );
    settings.theme.prefer_time_of_day = false;
    assert_eq!(preference::mode(&settings, &json!({"mode":"dark"})), None);
}

#[test]
fn cached_candidates_are_current_eligible_existing_and_use_content_scores() {
    let (_root, store) = setup();
    let assets: Vec<_> = ["day", "night", "outside", "old", "missing"]
        .into_iter()
        .map(|id| Asset {
            id: id.into(),
            revision: "r2".into(),
            width: 2000,
            height: 1200,
            ..Default::default()
        })
        .collect();
    store
        .activate_catalog(&assets, &[Source::library()], &[], "fixture")
        .unwrap();
    let path = store.paths.data.join("render.jpg");
    std::fs::write(&path, b"cached").unwrap();
    for id in ["day", "night", "outside", "old", "missing"] {
        let data = json!({"asset_id":id,"revision":if id=="old"{"r1"}else{"r2"},"config_key":"cfg","render_path":if id=="missing"{store.paths.data.join("missing.jpg")}else{path.clone()},"render_digest":id,"score":0.8});
        store
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO renders VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![
                    id,
                    id,
                    data["revision"].as_str().unwrap(),
                    "cfg",
                    data.to_string(),
                    1.
                ],
            )
            .unwrap();
        store
            .set_meta(&format!("suitability-v1:{id}"), &json!({"day_score":0.1}))
            .unwrap();
        if id != "night" {
            store
                .set_meta(&format!("suitability-v2:{id}"), &json!({"day_score":0.9}))
                .unwrap();
        }
    }
    let eligible = ["day", "night", "old", "missing"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let day = preference::cached_candidates(&store, "cfg", &eligible, "light").unwrap();
    assert_eq!(
        day.iter().map(|v| v.asset_id.as_str()).collect::<Vec<_>>(),
        vec!["day"]
    );
    let night = preference::cached_candidates(&store, "cfg", &eligible, "dark").unwrap();
    assert_eq!(
        night
            .iter()
            .map(|v| v.asset_id.as_str())
            .collect::<Vec<_>>(),
        vec!["night"]
    );
    assert!(
        preference::cached_candidates(&store, "different", &eligible, "light")
            .unwrap()
            .is_empty()
    );
}
fn setup() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(root.path())).unwrap();
    (root, store)
}
#[test]
fn day_and_night_preferences_are_soft_and_unknown_images_stay_neutral() {
    let (_root, store) = setup();
    store
        .set_meta(
            "suitability-v1:bright",
            &json!({"brightness":0.8,"highlights":0.2,"day_score":0.9}),
        )
        .unwrap();
    store
        .set_meta(
            "suitability-v1:dim",
            &json!({"brightness":0.1,"highlights":0.0,"day_score":0.1}),
        )
        .unwrap();
    assert!(
        preference::bonus(&store, "one", "bright", "light").unwrap()
            > preference::bonus(&store, "two", "dim", "light").unwrap()
    );
    assert!(
        preference::bonus(&store, "one", "bright", "dark").unwrap()
            < preference::bonus(&store, "two", "dim", "dark").unwrap()
    );
    assert_eq!(
        preference::bonus(&store, "unknown", "missing", "dark").unwrap(),
        0.0
    );
    assert!(preference::bonus(&store, "one", "bright", "light").unwrap() < 1.0);
}

#[test]
fn night_suitability_can_outweigh_the_exposure_score_of_a_daylight_photo() {
    let (_root, store) = setup();
    store
        .set_meta("suitability-v2:day", &json!({"day_score":0.67}))
        .unwrap();
    store
        .set_meta("suitability-v2:night", &json!({"day_score":0.30}))
        .unwrap();
    let day = 0.95 + preference::bonus(&store, "day", "day", "dark").unwrap();
    let night = 0.36 + preference::bonus(&store, "night", "night", "dark").unwrap();
    assert!(
        night > day,
        "exposure quality must not defeat a clearly suitable night photo"
    );
}
#[test]
fn explicit_period_overrides_persist_and_reset_without_erasing_exclusions() {
    let (root, store) = setup();
    store
        .feedback("one", "period", Some(json!({"period":"night"})))
        .unwrap();
    store.feedback("one", "never", None).unwrap();
    assert!(preference::bonus(&store, "one", "missing", "dark").unwrap() > 0.0);
    assert!(preference::bonus(&store, "one", "missing", "light").unwrap() < 0.0);
    drop(store);
    let store = Store::open(&Paths::under(root.path())).unwrap();
    assert_eq!(preference::period(&store, "one").unwrap(), "night");
    store
        .feedback("one", "period", Some(json!({"period":"either"})))
        .unwrap();
    assert_eq!(
        preference::bonus(&store, "one", "missing", "light").unwrap(),
        0.0
    );
    assert!(
        store
            .feedback("one", "period", Some(json!({"period":"bad"})))
            .is_err()
    );
    assert_eq!(preference::period(&store, "one").unwrap(), "either");
    store
        .feedback("one", "period", Some(json!({"period":"auto"})))
        .unwrap();
    assert_eq!(preference::period(&store, "one").unwrap(), "auto");
    assert!(
        store
            .connection()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM feedback WHERE asset_id='one' AND action='never')",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
    );
}
#[test]
fn preferences_cannot_expand_selected_sources_or_reinclude_excluded_photos() {
    let (_root, store) = setup();
    let make = |id: &str| Asset {
        id: id.into(),
        cloud_guid: id.into(),
        revision: "r1".into(),
        width: 2000,
        height: 1200,
        ..Default::default()
    };
    store
        .activate_catalog(
            &[make("chosen"), make("other"), make("excluded")],
            &[Source::library()],
            &[
                Membership {
                    asset_id: "chosen".into(),
                    kind: "person".into(),
                    source_id: "person-one".into(),
                },
                Membership {
                    asset_id: "excluded".into(),
                    kind: "person".into(),
                    source_id: "person-one".into(),
                },
            ],
            "fixture",
        )
        .unwrap();
    store
        .feedback("other", "period", Some(json!({"period":"night"})))
        .unwrap();
    store
        .feedback("excluded", "period", Some(json!({"period":"night"})))
        .unwrap();
    store.feedback("excluded", "never", None).unwrap();
    assert_eq!(
        store
            .eligible_ids(&[SourceRef {
                kind: "person".into(),
                id: "person-one".into()
            }])
            .unwrap(),
        vec!["chosen"]
    );
}
