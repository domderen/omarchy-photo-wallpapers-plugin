//! Soft comfort preference; source membership/exclusions remain authoritative.
use super::palette::{self, Suitability};
use crate::{
    imaging::Rect,
    storage::{Store, read_bounded},
};
use anyhow::Result;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
const PERIOD_WEIGHT: f64 = 2.4;

pub fn mode<'a>(settings: &'a crate::model::Settings, status: &'a Value) -> Option<&'a str> {
    if !settings.match_colors || !settings.theme.prefer_time_of_day {
        return None;
    }
    Some(match settings.theme.mode.as_str() {
        "light" => "light",
        "dark" => "dark",
        _ => status["mode"]
            .as_str()
            .filter(|m| ["light", "dark"].contains(m))
            .unwrap_or("dark"),
    })
}

pub struct Candidate {
    pub asset_id: String,
    pub score: f64,
}

/// One bounded indexed snapshot; the controller still enforces current, ready,
/// failed and recent-history guards. No image read or library membership scan.
pub fn cached_candidates(
    store: &Store,
    config: &str,
    eligible: &HashSet<String>,
    mode: &str,
) -> Result<Vec<Candidate>> {
    if !["light", "dark"].contains(&mode) {
        return Ok(Vec::new());
    }
    let db = store.connection()?;
    let mut query=db.prepare("SELECT r.asset_id,r.data,m2.value,m1.value,f.crop FROM renders r JOIN assets a ON a.id=r.asset_id AND a.revision=r.revision LEFT JOIN meta m2 ON m2.key='suitability-v2:'||json_extract(CASE WHEN json_valid(r.data) THEN r.data ELSE '{}' END,'$.render_digest') LEFT JOIN meta m1 ON m1.key='suitability-v1:'||json_extract(CASE WHEN json_valid(r.data) THEN r.data ELSE '{}' END,'$.render_digest') LEFT JOIN feedback f ON f.asset_id=r.asset_id AND f.action='period' WHERE r.config_key=?1 AND length(r.data)<=1048576 ORDER BY r.used DESC LIMIT 2048")?;
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for row in query.query_map([config], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })? {
        let (id, raw, new, old, override_raw) = row?;
        if !eligible.contains(&id) || !seen.insert(id.clone()) {
            continue;
        }
        let Ok(render) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if render["asset_id"] != id
            || render["config_key"] != config
            || !render["render_path"].as_str().is_some_and(|p| {
                std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() <= 100 * 1024 * 1024)
            })
        {
            continue;
        }
        let parse = |v: Option<String>| v.and_then(|s| serde_json::from_str::<Value>(&s).ok());
        let period = parse(override_raw).and_then(|v| v["period"].as_str().map(str::to_owned));
        let day = match period.as_deref() {
            Some("day") => 1.,
            Some("night") => 0.,
            Some("either") => continue,
            _ => parse(new)
                .as_ref()
                .and_then(day_score)
                .or_else(|| parse(old).as_ref().and_then(day_score))
                .unwrap_or(0.5),
        };
        let bonus = (day - 0.5) * PERIOD_WEIGHT * if mode == "light" { 1. } else { -1. };
        if bonus > 0.06 {
            candidates.push(Candidate {
                asset_id: id,
                score: bonus
                    + render["score"]
                        .as_f64()
                        .filter(|v| v.is_finite())
                        .unwrap_or(0.)
                        .clamp(0., 1.5)
                        * 0.15,
            });
        }
    }
    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.asset_id.cmp(&b.asset_id))
    });
    Ok(candidates)
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContentLayout {
    pub width: u32,
    pub height: u32,
    pub rect: Rect,
    pub mode: String,
}

fn day_score(value: &Value) -> Option<f64> {
    value["day_score"]
        .as_f64()
        .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
}

/// Measure only photo pixels in the already bounded palette thumbnail. This is
/// independent of palette identity, and never opens a full-size render/original.
pub fn measure_content(
    store: &Store,
    digest: &str,
    layout: &ContentLayout,
) -> Result<Option<Suitability>> {
    if digest.len() > 128
        || layout.width == 0
        || layout.height == 0
        || layout.rect.width == 0
        || layout.rect.height == 0
        || !["fill", "fit"].contains(&layout.mode.as_str())
    {
        return Ok(None);
    }
    let key = format!("suitability-v2:{digest}");
    let cached = store.meta(&key)?;
    if cached["layout"] == serde_json::to_value(layout)?
        && day_score(&cached).is_some()
        && let Ok(score) = serde_json::from_value::<Suitability>(cached)
    {
        return Ok(Some(score));
    }
    let bytes = match read_bounded(&palette::thumbnail_path(store, digest), 512 * 1024) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let reader = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format()?;
    let (w, h) = reader.into_dimensions()?;
    if w == 0 || h == 0 || w > 256 || h > 256 {
        return Ok(None);
    }
    let thumb = image::load_from_memory(&bytes)?.to_rgb8();
    let (pw, ph) = if layout.mode == "fit" {
        let scale = (layout.width as f64 / layout.rect.width as f64)
            .min(layout.height as f64 / layout.rect.height as f64);
        (
            ((layout.rect.width as f64 * scale / layout.width as f64 * w as f64).round() as u32)
                .clamp(1, w),
            ((layout.rect.height as f64 * scale / layout.height as f64 * h as f64).round() as u32)
                .clamp(1, h),
        )
    } else {
        (w, h)
    };
    let mut brightness = 0.;
    let mut highlights = 0.;
    for y in (h - ph) / 2..(h - ph) / 2 + ph {
        for x in (w - pw) / 2..(w - pw) / 2 + pw {
            let p = thumb[(x, y)];
            let v = (p[0] as f64 * 0.2126 + p[1] as f64 * 0.7152 + p[2] as f64 * 0.0722) / 255.;
            brightness += v;
            highlights += f64::from(v > 0.85);
        }
    }
    brightness /= (pw * ph) as f64;
    highlights /= (pw * ph) as f64;
    let score = Suitability {
        brightness,
        highlights,
        day_score: (brightness * 1.4 + highlights * 0.8).clamp(0., 1.),
    };
    let mut value = serde_json::to_value(&score)?;
    value["layout"] = serde_json::to_value(layout)?;
    store.set_meta(&key, &value)?;
    Ok(Some(score))
}
pub fn period(store: &Store, asset: &str) -> Result<String> {
    let raw: Option<String> = store
        .connection()?
        .query_row(
            "SELECT crop FROM feedback WHERE asset_id=?1 AND action='period'",
            [asset],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw
        .and_then(|v| serde_json::from_str::<Value>(&v).ok())
        .and_then(|v| {
            v["period"]
                .as_str()
                .filter(|s| ["day", "night", "either"].contains(s))
                .map(str::to_owned)
        })
        .unwrap_or("auto".into()))
}
pub fn bonus(store: &Store, asset: &str, render_digest: &str, mode: &str) -> Result<f64> {
    if !["light", "dark"].contains(&mode) {
        return Ok(0.);
    }
    let day = match period(store, asset)?.as_str() {
        "day" => 1.,
        "night" => 0.,
        "either" => return Ok(0.),
        _ => day_score(&store.meta(&format!("suitability-v2:{render_digest}"))?)
            .or(day_score(
                &store.meta(&format!("suitability-v1:{render_digest}"))?,
            ))
            .unwrap_or(0.5),
    };
    Ok((day - 0.5) * PERIOD_WEIGHT * if mode == "light" { 1. } else { -1. })
}
