//! Cheap deterministic surface styles derived from the paired Matugen cache.
//! No image decoding, subprocess, randomness or cache mutation occurs here.
use super::palette::{Pair, Palette, contrast};
use anyhow::{Result, ensure};

pub fn apply_pair(base: &Pair, style: &str) -> Result<Pair> {
    base.validate()?;
    let (light, dark) = match style {
        "neutral" => return Ok(base.clone()),
        "subtle" => (0.35, 0.25),
        "tinted" => (0.72, 0.50),
        "rich" => (1.0, 0.80),
        _ => anyhow::bail!("invalid_theme_style"),
    };
    let pair = Pair {
        light: apply(&base.light, light)?,
        dark: apply(&base.dark, dark)?,
        suitability: base.suitability.clone(),
    };
    pair.validate()?;
    Ok(pair)
}
fn mix(a: &str, b: &str, weight: f64) -> Result<String> {
    let decode = |s: &str, offset| u8::from_str_radix(&s[offset..offset + 2], 16).map(f64::from);
    let mut values = Vec::new();
    for offset in [1, 3, 5] {
        values
            .push((decode(a, offset)? * (1. - weight) + decode(b, offset)? * weight).round() as u8);
    }
    Ok(format!(
        "#{:02x}{:02x}{:02x}",
        values[0], values[1], values[2]
    ))
}
fn readable(color: &str, surfaces: &[String], target: &str) -> Result<String> {
    for step in 0..=100 {
        let candidate = mix(color, target, f64::from(step) / 100.)?;
        if surfaces
            .iter()
            .all(|bg| contrast(bg, &candidate).is_ok_and(|c| c >= 4.5))
        {
            return Ok(candidate);
        }
    }
    anyhow::bail!("invalid_palette")
}
fn apply(base: &Palette, weight: f64) -> Result<Palette> {
    for name in [
        "selection_background",
        "selection_foreground",
        "dark_background",
        "darker_background",
        "lighter_background",
        "dark_foreground",
        "light_foreground",
        "bright_foreground",
    ] {
        ensure!(base.colors.contains_key(name), "invalid_palette");
    }
    let mut result = base.clone();
    let tint = &base.colors["selection_background"];
    for (name, strength) in [
        ("background", 1.),
        ("dark_background", 0.85),
        ("darker_background", 0.70),
        ("lighter_background", 0.55),
    ] {
        result.colors.insert(
            name.into(),
            mix(&base.colors[name], tint, weight * strength)?,
        );
    }
    let surfaces: Vec<_> = [
        "background",
        "dark_background",
        "darker_background",
        "lighter_background",
    ]
    .into_iter()
    .map(|k| result.colors[k].clone())
    .collect();
    let target = if result.mode == "light" {
        "#000000"
    } else {
        "#ffffff"
    };
    for name in [
        "foreground",
        "dark_foreground",
        "light_foreground",
        "bright_foreground",
        "muted",
        "accent",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
    ] {
        result.colors.insert(
            name.into(),
            readable(&base.colors[name], &surfaces, target)?,
        );
    }
    // Rich surfaces can approach the original primary-container selection.
    // Move selection towards the readable accent until it remains distinguishable.
    let mut selection = tint.clone();
    for step in 35..=100 {
        selection = mix(tint, &result.colors["accent"], f64::from(step) / 100.)?;
        if contrast(&result.colors["background"], &selection)? >= 1.2 {
            break;
        }
    }
    ensure!(
        contrast(&result.colors["background"], &selection)? >= 1.2,
        "invalid_palette"
    );
    let text = readable(
        &base.colors["selection_foreground"],
        std::slice::from_ref(&selection),
        target,
    )
    .or_else(|_| {
        readable(
            &base.colors["selection_foreground"],
            std::slice::from_ref(&selection),
            if target == "#000000" {
                "#ffffff"
            } else {
                "#000000"
            },
        )
    })?;
    result.colors.insert("selection".into(), selection.clone());
    result
        .colors
        .insert("selection_background".into(), selection);
    result.colors.insert("selection_foreground".into(), text);
    for (index, name) in [
        "dark_background",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
        "foreground",
        "muted",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
        "bright_foreground",
    ]
    .into_iter()
    .enumerate()
    {
        result
            .colors
            .insert(format!("color{index}"), result.colors[name].clone());
    }
    result.validate()?;
    Ok(result)
}
