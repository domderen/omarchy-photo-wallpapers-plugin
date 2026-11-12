use omarchy_photo_wallpapers::privacy::policy::{
    Evidence, PrivacyChoice, ScreeningState, Signals, decide,
};

fn clear() -> Signals {
    Signals {
        nudity: Evidence::Clear,
        underwear: Evidence::Clear,
        bathing: Evidence::Clear,
        sleepwear: Evidence::Clear,
        swimwear: Evidence::Uncertain,
    }
}

#[test]
fn everyday_and_swimwear_are_automatically_admitted_without_approval() {
    let mut signals = clear();
    assert_eq!(
        decide(&signals, PrivacyChoice::Auto, false),
        ScreeningState::Clear
    );
    signals.swimwear = Evidence::Clear;
    assert_eq!(
        decide(&signals, PrivacyChoice::Auto, false),
        ScreeningState::Clear
    );
}

#[test]
fn swimwear_never_overrides_another_person_in_sleepwear() {
    let mut signals = clear();
    signals.swimwear = Evidence::Clear;
    signals.sleepwear = Evidence::Blocked;
    assert_eq!(
        decide(&signals, PrivacyChoice::Auto, false),
        ScreeningState::Sensitive
    );
}

#[test]
fn every_required_check_must_clear_and_defaults_are_conservative() {
    assert_eq!(
        decide(&Signals::default(), PrivacyChoice::Auto, false),
        ScreeningState::Uncertain
    );
    let mut signals = clear();
    signals.bathing = Evidence::Uncertain;
    assert_eq!(
        decide(&signals, PrivacyChoice::Auto, false),
        ScreeningState::Uncertain
    );
    signals.underwear = Evidence::Blocked;
    assert_eq!(
        decide(&signals, PrivacyChoice::Auto, false),
        ScreeningState::Sensitive
    );
}

#[test]
fn optional_corrections_never_override_never_show_again() {
    assert_eq!(
        decide(&clear(), PrivacyChoice::PersonalOnly, false),
        ScreeningState::Sensitive
    );
    assert_eq!(
        decide(&Signals::default(), PrivacyChoice::Allow, false),
        ScreeningState::Clear
    );
    assert_eq!(
        decide(&clear(), PrivacyChoice::Allow, true),
        ScreeningState::Sensitive
    );
}

#[test]
fn old_settings_default_to_both_filters_off() {
    let settings: omarchy_photo_wallpapers::model::Settings =
        serde_json::from_value(serde_json::json!({"interval_seconds":900,"paused":true})).unwrap();
    assert!(!settings.privacy.meeting);
    assert!(!settings.privacy.filter_personal);
    assert_eq!(settings.interval_seconds, 900);
    assert!(settings.paused);
    assert!(!settings.privacy.active());
}
