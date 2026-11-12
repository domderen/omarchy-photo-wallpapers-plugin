//! Optional palette generation and solar scheduling, independent of Apple and QML.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
pub mod location;
pub mod palette;
pub mod platform;
pub mod preference;
pub mod style;
pub mod worker;

// Coordinates are private application data; intentionally no Debug implementation.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
    pub name: String,
}
impl Location {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.latitude.is_finite()
                && (-90.0..=90.0).contains(&self.latitude)
                && self.longitude.is_finite()
                && (-180.0..=180.0).contains(&self.longitude)
                && self.name.len() <= 160
                && !self.name.chars().any(char::is_control),
            "invalid_location"
        );
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeSettings {
    pub mode: String,
    #[serde(default = "legacy_style")]
    pub style: String,
    pub location_mode: String,
    pub location: Option<Location>,
    pub sunrise_offset_minutes: i32,
    pub sunset_offset_minutes: i32,
    pub prefer_time_of_day: bool,
}
impl std::fmt::Debug for ThemeSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThemeSettings")
            .field("mode", &self.mode)
            .field("style", &self.style)
            .field("location_mode", &self.location_mode)
            .field("location", &self.location.as_ref().map(|_| "configured"))
            .field("sunrise_offset_minutes", &self.sunrise_offset_minutes)
            .field("sunset_offset_minutes", &self.sunset_offset_minutes)
            .field("prefer_time_of_day", &self.prefer_time_of_day)
            .finish()
    }
}
impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            mode: "auto".into(),
            style: "tinted".into(),
            location_mode: "current".into(),
            location: None,
            sunrise_offset_minutes: 0,
            sunset_offset_minutes: 0,
            prefer_time_of_day: true,
        }
    }
}
impl ThemeSettings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            ["neutral", "subtle", "tinted", "rich"].contains(&self.style.as_str()),
            "invalid_theme_style"
        );
        ensure!(
            ["auto", "light", "dark"].contains(&self.mode.as_str()),
            "invalid_theme_mode"
        );
        ensure!(
            ["current", "fixed"].contains(&self.location_mode.as_str()),
            "invalid_location_mode"
        );
        ensure!(
            (-180..=180).contains(&self.sunrise_offset_minutes)
                && (-180..=180).contains(&self.sunset_offset_minutes),
            "invalid_solar_offsets"
        );
        if let Some(place) = &self.location {
            place.validate()?;
        }
        ensure!(
            self.location_mode != "fixed" || self.location.is_some(),
            "location_required"
        );
        Ok(())
    }
}
fn legacy_style() -> String {
    "neutral".into()
}
