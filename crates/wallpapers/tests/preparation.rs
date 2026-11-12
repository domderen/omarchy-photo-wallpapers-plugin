use image::{Rgb, RgbImage};
use omarchy_photo_wallpapers::{
    imaging::*,
    model::{Asset, Enhancements},
    storage::{Paths, Store},
};
use tokio_util::sync::CancellationToken;
fn setup() -> (tempfile::TempDir, Store, std::path::PathBuf, Asset) {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&Paths::under(root.path())).unwrap();
    let input = root.path().join("fixture.png");
    RgbImage::from_fn(800, 600, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, 100])
    })
    .save(&input)
    .unwrap();
    let asset = Asset {
        id: "synthetic".into(),
        revision: "r1".into(),
        width: 800,
        height: 600,
        orientation: 1,
        rendition: "edited".into(),
        ..Asset::default()
    };
    (root, store, input, asset)
}
fn options() -> PrepareOptions {
    PrepareOptions {
        width: 640,
        height: 400,
        person_sensitive: false,
        fallback: "fit".into(),
        enhancements: Enhancements::default(),
        preferred_crop: None,
    }
}
#[test]
fn persisted_analysis_and_render_survive_restart_and_corruption() {
    let (_root, store, input, asset) = setup();
    let stop = CancellationToken::new();
    let first = prepare(&store, &asset, &input, &options(), &stop).unwrap();
    assert_eq!(first.decode_count, 1);
    assert!(!first.cache_hit);
    let second = prepare(
        &Store::open(&store.paths).unwrap(),
        &asset,
        &input,
        &options(),
        &stop,
    )
    .unwrap();
    assert!(second.cache_hit);
    assert_eq!(second.decode_count, 0);
    assert_eq!(second.render_path, first.render_path);
    std::fs::write(&first.render_path, b"bad render").unwrap();
    let fixed = prepare(&store, &asset, &input, &options(), &stop).unwrap();
    assert!(!fixed.cache_hit);
    assert!(fixed.analysis_cache_hit);
    assert_eq!(image::open(&fixed.render_path).unwrap().width(), 640);
    let db = store.connection().unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM analysis", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        1
    );
}
#[test]
fn output_change_reuses_features_but_renders_new_pixels() {
    let (_root, store, input, asset) = setup();
    let stop = CancellationToken::new();
    let first = prepare(&store, &asset, &input, &options(), &stop).unwrap();
    let mut changed = options();
    changed.width = 800;
    changed.height = 500;
    let next = prepare(&store, &asset, &input, &changed, &stop).unwrap();
    assert!(next.analysis_cache_hit);
    assert_ne!(first.render_path, next.render_path);
    assert_eq!(image::open(next.render_path).unwrap().height(), 500);
}
#[test]
fn named_membership_without_verified_bounds_preserves_the_full_frame() {
    let (_root, store, input, asset) = setup();
    let mut opts = options();
    opts.person_sensitive = true;
    let result = prepare(&store, &asset, &input, &opts, &CancellationToken::new()).unwrap();
    assert_eq!(result.mode, "fit");
    assert_eq!(
        result.rect,
        Rect {
            x: 0,
            y: 0,
            width: 800,
            height: 600
        }
    );
}
#[test]
fn orientation_and_subject_regions_define_safe_geometry() {
    let mut f = Features {
        width: 1200,
        height: 800,
        focus: [200., 400.],
        faces: vec![Rect {
            x: 30,
            y: 150,
            width: 200,
            height: 300,
        }],
        ..Features::default()
    };
    let (rect, mode) = choose_crop(
        &f,
        &PrepareOptions {
            width: 600,
            height: 800,
            ..options()
        },
    )
    .unwrap();
    assert_eq!(mode, "fill");
    assert!(rect.x <= 30 && rect.x + rect.width >= 230);
    f.faces.push(Rect {
        x: 1000,
        y: 100,
        width: 180,
        height: 300,
    });
    assert_eq!(
        choose_crop(
            &f,
            &PrepareOptions {
                width: 600,
                height: 800,
                ..options()
            }
        )
        .unwrap()
        .1,
        "fit"
    );
    let (_root, store, input, mut asset) = setup();
    asset.orientation = 6;
    let result = prepare(
        &store,
        &asset,
        &input,
        &options(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!((result.source_width, result.source_height), (600, 800));
}
#[test]
fn cancellation_never_publishes_a_partial_render() {
    let (_root, store, input, asset) = setup();
    let stop = CancellationToken::new();
    stop.cancel();
    assert_eq!(
        prepare(&store, &asset, &input, &options(), &stop)
            .err()
            .unwrap()
            .to_string(),
        "preparation_cancelled"
    );
    assert_eq!(
        store
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM renders", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
}
#[test]
fn enhancements_are_opt_in_and_active_render_survives_eviction() {
    let (_root, store, input, asset) = setup();
    let stop = CancellationToken::new();
    let first = prepare(&store, &asset, &input, &options(), &stop).unwrap();
    let mut changed = options();
    changed.enhancements.exposure = true;
    let next = prepare(&store, &asset, &input, &changed, &stop).unwrap();
    assert_ne!(first.render_path, next.render_path);
    evict(&store, 1, std::slice::from_ref(&first.render_path)).unwrap();
    assert!(first.render_path.exists());
    assert!(!next.render_path.exists());
}
#[test]
fn near_duplicate_fingerprint_and_invalid_geometry() {
    let input = RgbImage::from_fn(180, 120, |x, y| Rgb([(x / 2) as u8, y as u8, 10]));
    let one = analyze(&input).unwrap();
    let mut modified = input.clone();
    modified.put_pixel(10, 10, Rgb([100, 100, 100]));
    let two = analyze(&modified).unwrap();
    assert!((one.fingerprint ^ two.fingerprint).count_ones() <= 8);
    assert!(
        choose_crop(
            &one,
            &PrepareOptions {
                width: 0,
                ..options()
            }
        )
        .is_err()
    );
}

#[test]
fn metadata_revision_does_not_invalidate_unchanged_rendered_pixels() {
    let (_root, store, input, mut asset) = setup();
    let stop = CancellationToken::new();
    let first = prepare(&store, &asset, &input, &options(), &stop).unwrap();
    asset.revision = "metadata-only-change".into();
    asset.favorite = true;
    let next = prepare(&store, &asset, &input, &options(), &stop).unwrap();
    assert!(next.cache_hit);
    assert_eq!(next.render_path, first.render_path);
    assert_eq!(next.revision, "metadata-only-change");
}

#[test]
fn semantically_corrupt_cached_features_are_recomputed() {
    let (_root, store, input, asset) = setup();
    let stop = CancellationToken::new();
    let first = prepare(&store, &asset, &input, &options(), &stop).unwrap();
    store
        .connection()
        .unwrap()
        .execute("UPDATE analysis SET data=json_set(data,'$.width',0)", [])
        .unwrap();
    std::fs::remove_file(first.render_path).unwrap();
    let result = prepare(&store, &asset, &input, &options(), &stop).unwrap();
    assert!(!result.analysis_cache_hit);
    assert_eq!(result.source_width, 800);
}
#[test]
fn display_p3_icc_input_is_converted_once_to_srgb() {
    use image::ImageEncoder;
    let (_root, store, input, asset) = setup();
    let pixels = RgbImage::from_pixel(800, 600, Rgb([180, 90, 50]));
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
    encoder
        .set_icc_profile(moxcms::ColorProfile::new_display_p3().encode().unwrap())
        .unwrap();
    encoder
        .write_image(pixels.as_raw(), 800, 600, image::ExtendedColorType::Rgb8)
        .unwrap();
    std::fs::write(&input, bytes).unwrap();
    let result = prepare(
        &store,
        &asset,
        &input,
        &options(),
        &CancellationToken::new(),
    )
    .unwrap();
    let output = image::open(result.render_path).unwrap().to_rgb8();
    let p = output.get_pixel(320, 200);
    assert!(
        p[0] > 185 && p[1] < 89 && p[2] < 49,
        "P3 pixels need a real color conversion: {p:?}"
    );
}

#[test]
fn aggressive_portrait_crop_preserves_small_undetected_subjects() {
    let f = Features {
        width: 3024,
        height: 4032,
        focus: [1500., 1500.],
        faces: vec![Rect {
            x: 1600,
            y: 1000,
            width: 500,
            height: 700,
        }],
        ..Features::default()
    };
    assert_eq!(
        choose_crop(
            &f,
            &PrepareOptions {
                width: 2880,
                height: 1800,
                ..options()
            }
        )
        .unwrap(),
        (
            Rect {
                x: 0,
                y: 0,
                width: 3024,
                height: 4032
            },
            "fit".into()
        )
    );
}
#[test]
fn fit_uses_a_restrained_photo_backdrop_and_keeps_the_full_foreground() {
    let (_root, store, input, mut asset) = setup();
    RgbImage::from_pixel(800, 1200, Rgb([200, 120, 80]))
        .save(&input)
        .unwrap();
    asset.width = 800;
    asset.height = 1200;
    let prepared = prepare(
        &store,
        &asset,
        &input,
        &options(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(prepared.mode, "fit");
    let image = image::open(prepared.render_path).unwrap().to_rgb8();
    let margin = image.get_pixel(0, 0)[0];
    let foreground = image.get_pixel(320, 200)[0];
    assert!(
        margin > 0 && margin < foreground / 2,
        "fit margins should contain a darkened version of the photo"
    );
    assert!((foreground as i32 - 200).abs() < 5);
}
