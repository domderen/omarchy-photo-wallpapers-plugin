//! UTC solar events from the pinned sunrise ephemeris. Civil/DST changes do not
//! shift astronomical instants; UI formats those instants in the device timezone.
use crate::themes::Location;
use anyhow::{Result, ensure};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sunrise::{Coordinates, SolarDay, SolarEvent};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SolarState {
    pub mode: String,
    pub next_sunrise: Option<DateTime<Utc>>,
    pub next_sunset: Option<DateTime<Utc>>,
    pub next_change: Option<DateTime<Utc>>,
    pub polar: Option<String>,
}

pub fn state(
    now: DateTime<Utc>,
    location: &Location,
    sunrise_offset: i32,
    sunset_offset: i32,
) -> Result<SolarState> {
    location.validate()?;
    ensure!(
        (-180..=180).contains(&sunrise_offset) && (-180..=180).contains(&sunset_offset),
        "invalid_solar_offsets"
    );
    let coordinates = Coordinates::new(location.latitude, location.longitude)
        .ok_or_else(|| anyhow::anyhow!("invalid_location"))?;
    let date = now.date_naive();
    let events_for = |delta: i64| {
        let solar = SolarDay::new(coordinates, date + Duration::days(delta));
        [
            (
                solar
                    .event_time(SolarEvent::Sunrise)
                    .map(|t| t + Duration::minutes(sunrise_offset.into())),
                true,
            ),
            (
                solar
                    .event_time(SolarEvent::Sunset)
                    .map(|t| t + Duration::minutes(sunset_offset.into())),
                false,
            ),
        ]
        .into_iter()
        .filter_map(|(time, day)| time.map(|t| (t, day)))
        .collect::<Vec<_>>()
    };
    let mut events: Vec<_> = (-2..=2).flat_map(events_for).collect();
    // High latitudes may have no event for months. Find actual adjoining
    // transitions rather than synthesizing daily sunrise/sunset values.
    if !events.iter().any(|(t, _)| *t <= now) {
        for d in 3..=190 {
            let older = events_for(-d);
            if !older.is_empty() {
                events.extend(older);
                break;
            }
        }
    }
    if !events.iter().any(|(t, _)| *t > now) {
        for d in 3..=190 {
            let newer = events_for(d);
            if !newer.is_empty() {
                events.extend(newer);
                break;
            }
        }
    }
    events.sort_by_key(|(t, _)| *t);
    let local_date = (now + Duration::seconds((location.longitude * 240.) as i64)).date_naive();
    let solar_today = SolarDay::new(coordinates, local_date);
    let no_daily_events = solar_today.event_time(SolarEvent::Sunrise).is_none()
        && solar_today.event_time(SolarEvent::Sunset).is_none();
    // At the last ordinary day before a polar season both ephemeris events
    // can exist, so its last sunset cannot identify midnight sun. The nearest
    // lower latitude with events distinguishes the two regimes using the same
    // declination/refraction model: its day is nearly24h or nearly0h.
    let polar_day = || {
        for step in 1..=90 {
            let latitude =
                location.latitude.signum() * (location.latitude.abs() - f64::from(step)).max(0.);
            let near = SolarDay::new(
                Coordinates::new(latitude, location.longitude).unwrap(),
                local_date,
            );
            if let (Some(rise), Some(set)) = (
                near.event_time(SolarEvent::Sunrise),
                near.event_time(SolarEvent::Sunset),
            ) {
                return set - rise > Duration::hours(12);
            }
        }
        false
    };
    let day = if no_daily_events {
        polar_day()
    } else {
        events
            .iter()
            .rev()
            .find(|(t, _)| *t <= now)
            .is_some_and(|(_, day)| *day)
    };
    let next_sunrise = events
        .iter()
        .find(|(t, day)| *t > now && *day)
        .map(|(t, _)| *t);
    let next_sunset = events
        .iter()
        .find(|(t, day)| *t > now && !*day)
        .map(|(t, _)| *t);
    let next_change = events
        .iter()
        .find(|(t, new_day)| *t > now && *new_day != day)
        .map(|(t, _)| *t);
    let polar = no_daily_events.then(|| if day { "day".into() } else { "night".into() });
    Ok(SolarState {
        mode: if day { "light".into() } else { "dark".into() },
        next_sunrise,
        next_sunset,
        next_change,
        polar,
    })
}
