use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    Clear,
    Blocked,
    #[default]
    Uncertain,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreeningState {
    #[default]
    Unscanned,
    Clear,
    Sensitive,
    Uncertain,
    Error,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyChoice {
    #[default]
    Auto,
    PersonalOnly,
    Allow,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signals {
    pub nudity: Evidence,
    pub underwear: Evidence,
    pub bathing: Evidence,
    pub sleepwear: Evidence,
    /// Diagnostic only: swimwear is permitted, never an exception for others.
    pub swimwear: Evidence,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrivacySettings {
    pub meeting: bool,
    pub filter_personal: bool,
}
impl PrivacySettings {
    pub fn active(&self) -> bool {
        self.meeting || self.filter_personal
    }
}

pub fn decide(signals: &Signals, choice: PrivacyChoice, never: bool) -> ScreeningState {
    if never || choice == PrivacyChoice::PersonalOnly {
        return ScreeningState::Sensitive;
    }
    if choice == PrivacyChoice::Allow {
        return ScreeningState::Clear;
    }
    let required = [
        signals.nudity,
        signals.underwear,
        signals.bathing,
        signals.sleepwear,
    ];
    if required.contains(&Evidence::Blocked) {
        ScreeningState::Sensitive
    } else if required.contains(&Evidence::Uncertain) {
        ScreeningState::Uncertain
    } else {
        ScreeningState::Clear
    }
}
