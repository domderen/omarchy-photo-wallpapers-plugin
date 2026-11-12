use omarchy_photo_wallpapers::{
    storage::{Paths, Store},
    themes::palette::{Generator, Pair},
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

fn fixture() -> Value {
    let roles = [
        ("background", "#fff8f5", "#171310"),
        ("on_background", "#201a17", "#ece0d8"),
        ("surface_container_lowest", "#ffffff", "#100d0a"),
        ("surface_container_high", "#eaded5", "#312923"),
        ("on_surface_variant", "#50443b", "#d5c3b5"),
        ("primary", "#885020", "#ffb77a"),
        ("primary_container", "#ffdcc0", "#6b390b"),
        ("on_primary_container", "#301500", "#ffdcc0"),
        ("secondary", "#745943", "#e3bfa4"),
        ("tertiary", "#5b6234", "#c3cd8d"),
        ("error", "#ba1a1a", "#ffb4ab"),
    ];
    json!({"colors":roles.into_iter().map(|(key,light,dark)|(key.to_string(),json!({"light":{"color":light},"dark":{"color":dark}}))).collect::<serde_json::Map<String,Value>>()})
}
fn source(root: &Path) -> PathBuf {
    let p = root.join("render.png");
    image::RgbImage::from_pixel(120, 80, image::Rgb([180, 120, 70]))
        .save(&p)
        .unwrap();
    p
}
fn generator(root: &Path, body: &str) -> Generator {
    let exe = root.join("generator");
    std::fs::write(&exe, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
    Generator::for_program(exe, "matugen 4.2.0".into(), Duration::from_secs(2))
}
#[test]
fn material_modes_have_readable_semantic_omarchy_palettes() {
    let pair = Pair::from_matugen(&fixture()).unwrap();
    for (mode, palette) in [("light", &pair.light), ("dark", &pair.dark)] {
        assert_eq!(palette.mode, mode);
        assert!(palette.contrast("background", "foreground").unwrap() >= 4.5);
        assert!(palette.contrast("background", "accent").unwrap() >= 3.0);
        assert_ne!(palette.colors["red"], palette.colors["green"]);
        assert!(palette.colors.contains_key("color15"));
        let text = palette.toml().unwrap();
        assert!(text.contains("mode = "));
    }
    let mut wrong = fixture();
    wrong["colors"]["on_background"]["dark"]["color"] = json!("#171310");
    assert!(Pair::from_matugen(&wrong).is_err());
    let mut injection = fixture();
    injection["colors"]["primary"]["light"]["color"] = json!("#ffffff\ncommand=bad");
    assert!(Pair::from_matugen(&injection).is_err());
}
#[tokio::test]
async fn paired_cache_survives_missing_generator_and_corruption_rebuilds() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(&root.path().join("app"))).unwrap();
    let image = source(root.path());
    std::fs::write(root.path().join("fixture.json"), fixture().to_string()).unwrap();
    let generator = generator(
        root.path(),
        "echo run >> \"$(dirname \"$0\")/runs\"\ncat \"$(dirname \"$0\")/fixture.json\"",
    );
    let one = generator
        .prepare(&store, &image, "render-fixture", &CancellationToken::new())
        .await
        .unwrap();
    assert!(!one.cache_hit);
    assert!(one.pair.light.contrast("background", "foreground").unwrap() >= 4.5);
    let two = generator
        .prepare(&store, &image, "render-fixture", &CancellationToken::new())
        .await
        .unwrap();
    assert!(two.cache_hit);
    assert_eq!(
        std::fs::read_to_string(root.path().join("runs"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    std::fs::write(&two.cache_path, "broken").unwrap();
    let three = generator
        .prepare(&store, &image, "render-fixture", &CancellationToken::new())
        .await
        .unwrap();
    assert!(!three.cache_hit);
    std::fs::remove_file(root.path().join("generator")).unwrap();
    std::fs::remove_file(image).unwrap();
    assert!(
        generator
            .prepare(
                &store,
                &root.path().join("missing"),
                "render-fixture",
                &CancellationToken::new()
            )
            .await
            .unwrap()
            .cache_hit
    );
}
#[tokio::test]
async fn generator_timeout_and_cancellation_kill_work_without_publishing() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(&root.path().join("app"))).unwrap();
    let image = source(root.path());
    let _ = generator(root.path(), "exec sleep 10");
    let generator = Generator::for_program(
        root.path().join("generator"),
        "matugen 4.2.0".into(),
        Duration::from_millis(50),
    );
    assert_eq!(
        generator
            .prepare(&store, &image, "slow-fixture", &CancellationToken::new())
            .await
            .err()
            .unwrap()
            .to_string(),
        "palette_generation_timed_out"
    );
    let stop = CancellationToken::new();
    stop.cancel();
    assert_eq!(
        generator
            .prepare(&store, &image, "cancel-fixture", &stop)
            .await
            .err()
            .unwrap()
            .to_string(),
        "theme_cancelled"
    );
    let published = std::fs::read_dir(store.paths.data.join("theme-palettes-v1"))
        .map(|v| {
            v.filter_map(Result::ok)
                .filter(|p| p.path().extension().is_some_and(|e| e == "json"))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(published, 0);
}
#[tokio::test]
async fn malformed_generator_output_never_replaces_a_good_pair() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(&root.path().join("app"))).unwrap();
    let image = source(root.path());
    std::fs::write(root.path().join("fixture.json"), fixture().to_string()).unwrap();
    let generator = generator(root.path(), "cat \"$(dirname \"$0\")/fixture.json\"");
    let good = generator
        .prepare(&store, &image, "good-fixture", &CancellationToken::new())
        .await
        .unwrap();
    let original = std::fs::read(&good.cache_path).unwrap();
    std::fs::write(root.path().join("fixture.json"), "not-json").unwrap();
    assert_eq!(
        generator
            .prepare(&store, &image, "invalid-fixture", &CancellationToken::new())
            .await
            .err()
            .unwrap()
            .to_string(),
        "invalid_palette"
    );
    assert_eq!(std::fs::read(&good.cache_path).unwrap(), original);
}

#[test]
fn styles_preserve_mode_and_readable_semantics_while_adding_visible_colour() {
    let base = Pair::from_matugen(&fixture()).unwrap();
    let mut light_backgrounds = Vec::new();
    let mut dark_backgrounds = Vec::new();
    for name in ["neutral", "subtle", "tinted", "rich"] {
        let pair = omarchy_photo_wallpapers::themes::style::apply_pair(&base, name).unwrap();
        for palette in [&pair.light, &pair.dark] {
            palette.validate().unwrap();
            for surface in ["background", "dark_background", "lighter_background"] {
                for text in [
                    "foreground",
                    "muted",
                    "red",
                    "green",
                    "yellow",
                    "blue",
                    "magenta",
                    "cyan",
                ] {
                    if name != "neutral" {
                        assert!(
                            palette.contrast(surface, text).unwrap() >= 4.5,
                            "{name}/{surface}/{text}"
                        );
                    }
                }
            }
            if name != "neutral" {
                assert!(
                    palette
                        .contrast("selection_background", "selection_foreground")
                        .unwrap()
                        >= 4.5
                );
                assert!(
                    palette
                        .contrast("background", "selection_background")
                        .unwrap()
                        >= 1.2
                );
            }
        }
        light_backgrounds.push(pair.light.colors["background"].clone());
        dark_backgrounds.push(pair.dark.colors["background"].clone());
    }
    for values in [light_backgrounds, dark_backgrounds] {
        for (i, a) in values.iter().enumerate() {
            for b in &values[i + 1..] {
                let distance: u32 = [1, 3, 5]
                    .into_iter()
                    .map(|offset| {
                        let x = i32::from(u8::from_str_radix(&a[offset..offset + 2], 16).unwrap());
                        let y = i32::from(u8::from_str_radix(&b[offset..offset + 2], 16).unwrap());
                        x.abs_diff(y)
                    })
                    .sum();
                assert!(distance >= 12, "styles must look distinct: {a}/{b}");
            }
        }
    }
    assert!(omarchy_photo_wallpapers::themes::style::apply_pair(&base, "invalid").is_err());
}

#[tokio::test]
async fn all_styles_reuse_a_paired_cache_with_the_image_and_generator_removed() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(&root.path().join("app"))).unwrap();
    let image = source(root.path());
    std::fs::write(root.path().join("fixture.json"), fixture().to_string()).unwrap();
    let generator = generator(root.path(), "cat \"$(dirname \"$0\")/fixture.json\"");
    generator
        .prepare(&store, &image, "style-fixture", &CancellationToken::new())
        .await
        .unwrap();
    std::fs::remove_file(root.path().join("generator")).unwrap();
    std::fs::remove_file(&image).unwrap();
    for name in ["subtle", "tinted", "rich"] {
        let cached = generator
            .prepare(&store, &image, "style-fixture", &CancellationToken::new())
            .await
            .unwrap();
        assert!(cached.cache_hit);
        omarchy_photo_wallpapers::themes::style::apply_pair(&cached.pair, name).unwrap();
    }
}

#[test]
fn incomplete_cached_aliases_cannot_crash_restyling() {
    let mut base = Pair::from_matugen(&fixture()).unwrap();
    base.light.colors.remove("selection_background");
    assert!(omarchy_photo_wallpapers::themes::style::apply_pair(&base, "tinted").is_err());
}

#[tokio::test]
async fn fractional_suitability_roundtrips_without_invalidating_a_valid_palette() {
    use sha2::{Digest, Sha256};
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(&root.path().join("app"))).unwrap();
    let image = source(root.path());
    std::fs::write(root.path().join("fixture.json"), fixture().to_string()).unwrap();
    let generator = generator(root.path(), "cat \"$(dirname \"$0\")/fixture.json\"");
    let prepared = generator
        .prepare(
            &store,
            &image,
            "fractional-cache",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut pair = prepared.pair;
    pair.suitability.brightness = 0.12332771411609933;
    pair.suitability.day_score = 0.17322520601253907;
    pair.suitability.highlights = 0.0007080078125;
    let bytes = serde_json::to_vec(&pair).unwrap();
    let checksum = hex::encode(Sha256::digest(&bytes));
    let cache: Value =
        serde_json::from_slice(&std::fs::read(&prepared.cache_path).unwrap()).unwrap();
    // Embed the original serialised pair directly so the fixture does not parse
    // and accidentally normalise the values before the application reads them.
    let text = format!(
        "{{\"key\":{},\"checksum\":{},\"pair\":{}}}",
        cache["key"],
        serde_json::to_string(&checksum).unwrap(),
        String::from_utf8(bytes).unwrap()
    );
    std::fs::write(&prepared.cache_path, text).unwrap();
    std::fs::remove_file(root.path().join("generator")).unwrap();
    std::fs::remove_file(&image).unwrap();
    let reused = generator
        .prepare(
            &store,
            &image,
            "fractional-cache",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(reused.cache_hit);
    assert_eq!(
        reused.pair.suitability.brightness.to_bits(),
        pair.suitability.brightness.to_bits()
    );
}
