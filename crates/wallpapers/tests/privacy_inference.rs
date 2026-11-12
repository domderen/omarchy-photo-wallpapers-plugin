use omarchy_photo_wallpapers::privacy::{
    inference::{context_signals, full_frame_tensor},
    models::ModelSpec,
    policy::{Evidence, PrivacyChoice, ScreeningState, decide},
};
use serde_json::json;
fn spec() -> ModelSpec {
    serde_json::from_value(json!({"artifact":{"id":"synthetic","path":"model.onnx","format":"onnx","sha256":"a".repeat(64),"bytes":1,"license":"MIT","license_source":"https://example.invalid/LICENSE"},"role":"context","output":"pooler_output","mean":[0.5,0.5,0.5],"std":[0.5,0.5,0.5],"embeddings":{},"clear_margin":0.02,"blocked_margin":0.01})).unwrap()
}
#[test]
fn full_frame_preprocessing_retains_both_ends_of_a_wide_photo() {
    let mut image = image::RgbImage::new(400, 100);
    for (x, _, pixel) in image.enumerate_pixels_mut() {
        *pixel = if x < 200 {
            image::Rgb([255, 0, 0])
        } else {
            image::Rgb([0, 0, 255])
        };
    }
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).unwrap();
    let tensor = full_frame_tensor(&png.into_inner(), &spec()).unwrap();
    assert_eq!(tensor.len(), 3 * 224 * 224);
    assert!(tensor[112 * 224] > 0.9);
    assert!(tensor[2 * 224 * 224 + 112 * 224 + 223] > 0.9);
    assert!(tensor[0].abs() < 0.01); // mean-filled letterbox, not a center crop
}
#[test]
fn invalid_or_oversized_preview_never_reaches_native_inference() {
    assert!(full_frame_tensor(b"not-an-image", &spec()).is_err());
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbImage::new(4096, 1)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    assert!(full_frame_tensor(&png.into_inner(), &spec()).is_err());
}
#[test]
fn swimwear_similarity_cannot_cancel_an_independent_sleepwear_signal() {
    let scores = std::collections::BTreeMap::from([
        ("ordinary".into(), 0.1),
        ("swimwear".into(), 0.3),
        ("non_person".into(), 0.05),
        ("ordinary_sleeping".into(), 0.1),
        ("sleepwear".into(), 0.35),
        ("nudity".into(), 0.1),
        ("underwear".into(), 0.1),
        ("bathing".into(), 0.1),
    ]);
    let signals = context_signals(&scores, 0.02, 0.01).unwrap();
    assert_eq!(signals.sleepwear, Evidence::Blocked);
    assert_eq!(
        decide(&signals, PrivacyChoice::Auto, false),
        ScreeningState::Sensitive
    );
}

#[test]
fn sleeping_context_is_never_a_clothing_clearance() {
    let scores = std::collections::BTreeMap::from([
        ("ordinary".into(), 0.1),
        ("swimwear".into(), 0.2),
        ("non_person".into(), 0.05),
        ("ordinary_sleeping".into(), 0.9),
        ("sleepwear".into(), 0.5),
        ("nudity".into(), 0.1),
        ("underwear".into(), 0.1),
        ("bathing".into(), 0.1),
    ]);
    let signals = context_signals(&scores, 0.02, 0.01).unwrap();
    assert_eq!(signals.sleepwear, Evidence::Blocked);
    assert_eq!(
        decide(&signals, PrivacyChoice::Auto, false),
        ScreeningState::Sensitive
    );
}

#[test]
fn context_cross_checks_require_agreement_and_preserve_blocked_findings() {
    use omarchy_photo_wallpapers::privacy::inference::combine_contexts;
    use omarchy_photo_wallpapers::privacy::policy::Signals;
    let clear = Signals {
        nudity: Evidence::Clear,
        underwear: Evidence::Clear,
        bathing: Evidence::Clear,
        sleepwear: Evidence::Clear,
        swimwear: Evidence::Clear,
    };
    let mut second = clear.clone();
    second.sleepwear = Evidence::Uncertain;
    assert_eq!(
        decide(
            &combine_contexts(clear.clone(), second.clone()),
            PrivacyChoice::Auto,
            false
        ),
        ScreeningState::Uncertain
    );
    second.sleepwear = Evidence::Blocked;
    assert_eq!(
        decide(&combine_contexts(clear, second), PrivacyChoice::Auto, false),
        ScreeningState::Sensitive
    );
}
