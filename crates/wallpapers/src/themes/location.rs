//! Reuse Omarchy's weather location configuration and existing auto provider.
//! Raw responses and coordinates never enter logs or process arguments.
use super::{Location, ThemeSettings};
use crate::storage::{Store, read_bounded};
use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

#[derive(Clone, Serialize, Deserialize)]
pub struct ResolvedLocation {
    pub location: Location,
    pub provider: String,
    pub resolved_at: DateTime<Utc>,
    pub stale: bool,
}
pub struct Resolver {
    client: reqwest::Client,
    auto_url: String,
    geocode_url: String,
}
impl Resolver {
    pub fn new() -> Result<Self> {
        Self::with_endpoints(
            "https://wttr.in/?format=j1",
            "https://geocoding-api.open-meteo.com/v1/search",
        )
    }
    pub fn with_endpoints(auto_url: &str, geocode_url: &str) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("omarchy-photo-wallpapers/0.3")
                .build()?,
            auto_url: auto_url.into(),
            geocode_url: geocode_url.into(),
        })
    }
    async fn get(&self, url: &str, query: &[(&str, &str)]) -> Result<Value> {
        let response = self
            .client
            .get(url)
            .query(query)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("location_unavailable"))?;
        ensure!(response.status().is_success(), "location_unavailable");
        crate::icloud::auth::bounded_json(response, 256 * 1024)
            .await
            .map_err(|_| anyhow::anyhow!("location_unavailable"))
    }
    pub async fn resolve(
        &self,
        store: &Store,
        settings: &ThemeSettings,
        home: &Path,
        network: bool,
    ) -> Result<ResolvedLocation> {
        settings.validate()?;
        if settings.location_mode == "fixed" {
            return Ok(ResolvedLocation {
                location: settings
                    .location
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("location_required"))?,
                provider: "fixed".into(),
                resolved_at: Utc::now(),
                stale: false,
            });
        }
        let cache: Option<ResolvedLocation> =
            serde_json::from_value(store.meta("theme-location-v1")?).ok();
        let configured = read_bounded(
            &home.join(".local/state/omarchy/settings/weather.json"),
            32768,
        )
        .ok()
        .and_then(|s| serde_json::from_slice::<Value>(&s).ok());
        if let Some(location) = configured.as_ref().and_then(parse_location) {
            return save(store, location, "omarchy-weather");
        }
        let found = if network {
            if let Some(name) = configured
                .as_ref()
                .and_then(|v| v["name"].as_str())
                .filter(|n| !n.trim().is_empty() && n.len() <= 160)
            {
                self.get(
                    &self.geocode_url,
                    &[("name", name), ("count", "1"), ("format", "json")],
                )
                .await
                .ok()
                .and_then(|v| {
                    v["results"]
                        .as_array()
                        .and_then(|r| r.first())
                        .and_then(parse_location)
                })
                .map(|v| (v, "omarchy-weather-geocoded"))
            } else {
                self.get(&self.auto_url,&[]).await.ok().and_then(|v| {
                    let area=v["nearest_area"].as_array()?.first()?;
                    parse_location(&json!({"latitude":area["latitude"],"longitude":area["longitude"],"name":area["areaName"][0]["value"].as_str().unwrap_or("Current area")}))
                }).map(|v|(v,"omarchy-weather-ip-estimate"))
            }
        } else {
            None
        };
        if let Some((location, provider)) = found {
            return save(store, location, provider);
        }
        if let Some(mut cache) = cache.filter(|v| v.location.validate().is_ok()) {
            cache.stale = true;
            return Ok(cache);
        }
        anyhow::bail!("location_unavailable")
    }
}
fn parse_location(v: &Value) -> Option<Location> {
    let number = |key: &str| v[key].as_f64().or_else(|| v[key].as_str()?.parse().ok());
    let location = Location {
        latitude: number("latitude")?,
        longitude: number("longitude")?,
        name: v["name"].as_str().unwrap_or("Current area").into(),
    };
    location.validate().ok()?;
    Some(location)
}
fn save(store: &Store, location: Location, provider: &str) -> Result<ResolvedLocation> {
    let value = ResolvedLocation {
        location,
        provider: provider.into(),
        resolved_at: Utc::now(),
        stale: false,
    };
    store.set_meta("theme-location-v1", &serde_json::to_value(&value)?)?;
    Ok(value)
}
