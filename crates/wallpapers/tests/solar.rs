use chrono::{DateTime, Duration, Utc};
use omarchy_photo_wallpapers::{
    solar,
    themes::{Location, ThemeSettings},
};
use serde_json::json;

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}
fn location(lat: f64, lon: f64) -> Location {
    Location {
        latitude: lat,
        longitude: lon,
        name: "Fixture".into(),
    }
}

#[test]
fn equinox_events_and_modes_are_local_calculations() {
    let place = location(51.4779, 0.0);
    let state = solar::state(at("2026-03-20T12:00:00Z"), &place, 0, 0).unwrap();
    assert_eq!(state.mode, "light");
    assert!(
        (state.next_sunset.unwrap() - at("2026-03-20T18:14:00Z"))
            .num_minutes()
            .abs()
            < 6
    );
    assert_eq!(
        solar::state(at("2026-03-20T23:00:00Z"), &place, 0, 0)
            .unwrap()
            .mode,
        "dark"
    );
}
#[test]
fn offsets_are_applied_to_events_and_not_to_clock_readings() {
    let place = location(52.23, 21.01);
    let base = solar::state(at("2026-06-20T00:00:00Z"), &place, 0, 0).unwrap();
    let sunrise = base.next_sunrise.unwrap();
    assert_eq!(
        solar::state(sunrise + Duration::minutes(15), &place, 30, 0)
            .unwrap()
            .mode,
        "dark"
    );
    assert_eq!(
        solar::state(sunrise + Duration::minutes(31), &place, 30, 0)
            .unwrap()
            .mode,
        "light"
    );
    assert_eq!(
        solar::state(sunrise, &place, 181, 0)
            .unwrap_err()
            .to_string(),
        "invalid_solar_offsets"
    );
}
#[test]
fn international_date_line_and_dst_dates_have_ordered_future_events() {
    for (lat, lon) in [(1.8, 179.0), (1.8, -179.0), (52.23, 21.01)] {
        for now in [
            "2026-03-29T00:30:00Z",
            "2026-10-25T01:30:00Z",
            "2026-12-31T23:30:00Z",
        ] {
            let now = at(now);
            let s = solar::state(now, &location(lat, lon), 0, 0).unwrap();
            assert!(s.next_change.unwrap() > now);
            assert!(s.next_change.unwrap() < now + Duration::hours(25));
            assert!(s.next_sunrise.unwrap() > now && s.next_sunset.unwrap() > now);
        }
    }
}
#[test]
fn polar_day_and_night_do_not_invent_a_daily_event() {
    let place = location(78.22, 15.65);
    let day = solar::state(at("2026-06-21T12:00:00Z"), &place, 0, 0).unwrap();
    assert_eq!(day.mode, "light");
    assert_eq!(day.polar.as_deref(), Some("day"));
    let night = solar::state(at("2026-12-21T12:00:00Z"), &place, 0, 0).unwrap();
    assert_eq!(night.mode, "dark");
    assert_eq!(night.polar.as_deref(), Some("night"));
    assert!(day.next_change.unwrap() > at("2026-07-01T00:00:00Z"));
    assert!(night.next_change.unwrap() > at("2027-01-01T00:00:00Z"));
}
#[test]
fn additive_theme_settings_validate_without_requiring_a_location_while_disabled() {
    let root = tempfile::tempdir().unwrap();
    let store = omarchy_photo_wallpapers::storage::Store::open(
        &omarchy_photo_wallpapers::storage::Paths::under(root.path()),
    )
    .unwrap();
    assert_eq!(store.settings().unwrap().theme.mode, "auto");
    let changed=store.update_settings(0,json!({"match_colors":true,"theme":{"mode":"dark","location_mode":"fixed","location":{"latitude":52.2,"longitude":21.0,"name":"Fixture"}}})).unwrap();
    assert!(changed.match_colors);
    assert_eq!(changed.interval_seconds, 3600);
    for value in [
        json!({"mode":"bad"}),
        json!({"sunrise_offset_minutes":181}),
        json!({"location_mode":"fixed"}),
        json!({"location":{"latitude":91,"longitude":0,"name":"bad"}}),
    ] {
        assert!(store.update_settings(1, json!({"theme":value})).is_err());
    }
    assert_eq!(store.settings().unwrap().revision, 1);
    assert!(ThemeSettings::default().validate().is_ok());
}

#[test]
fn style_defaults_preserve_legacy_palettes_and_validate_new_choices() {
    let old: ThemeSettings = serde_json::from_value(json!({"mode":"auto"})).unwrap();
    assert_eq!(old.style, "neutral");
    assert_eq!(ThemeSettings::default().style, "tinted");
    for style in ["neutral", "subtle", "tinted", "rich"] {
        let mut settings = old.clone();
        settings.style = style.into();
        settings.validate().unwrap();
        assert_eq!(
            serde_json::from_value::<ThemeSettings>(serde_json::to_value(settings).unwrap())
                .unwrap()
                .style,
            style
        );
    }
    let mut wrong = old;
    wrong.style = "unknown".into();
    assert_eq!(
        wrong.validate().unwrap_err().to_string(),
        "invalid_theme_style"
    );
}
