//! Prepare fictional promotional photos through the real pipeline, without network or desktop effects.
use anyhow::{Context, Result, ensure};
use omarchy_photo_wallpapers::{
    imaging::{PrepareOptions, prepare},
    model::{Asset, Enhancements, Membership, Source, SourceRef},
    storage::{Paths, Store, atomic_json},
};
use serde_json::json;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    let input = PathBuf::from(args.get(1).context("input_directory_required")?);
    let profile = PathBuf::from(args.get(2).context("new_profile_required")?);
    ensure!(!profile.exists(), "new_profile_required");
    let store = Store::open(&Paths::under(&profile))?;
    let width = args
        .get(3)
        .map(|v| v.to_string_lossy().parse::<u32>())
        .transpose()?
        .unwrap_or(2880);
    let height = args
        .get(4)
        .map(|v| v.to_string_lossy().parse::<u32>())
        .transpose()?
        .unwrap_or(1800);
    ensure!(
        (640..=7680).contains(&width) && (400..=4320).contains(&height),
        "invalid_demo_geometry"
    );
    let options = PrepareOptions {
        width,
        height,
        person_sensitive: false,
        fallback: "fit".into(),
        enhancements: Enhancements::default(),
        preferred_crop: None,
    };
    let mut ready = Vec::new();
    let mut assets = Vec::new();
    let mut sources = vec![Source {
        provider: "demo".into(),
        ..Source::library()
    }];
    let mut memberships = Vec::new();
    let mut report = Vec::new();
    for name in ["coastal-morning", "alpine-picnic", "blue-hour", "campfire"] {
        let path = input.join(format!("{name}.jpg"));
        let (width, height) = image::image_dimensions(&path)?;
        let asset = Asset {
            id: format!("synthetic-promo-{name}"),
            revision: "ai-generated-demo-v1".into(),
            width,
            height,
            orientation: 1,
            rendition: "ai-generated-sample".into(),
            ..Asset::default()
        };
        let prepared = prepare(&store, &asset, &path, &options, &CancellationToken::new())?;
        report.push(json!({"name":name,"input_size":[width,height],"output_size":[prepared.width,prepared.height],"render_sha256":prepared.render_digest,"framing":prepared.mode,"provenance":"ai-generated-fictional-family"}));
        sources.push(Source {
            id: format!("demo-{name}"),
            kind: "album".into(),
            name: format!("Fictional family — {name}"),
            count: 1,
            provider: "demo".into(),
            ..Source::default()
        });
        memberships.push(Membership {
            asset_id: asset.id.clone(),
            kind: "album".into(),
            source_id: format!("demo-{name}"),
        });
        ready.push(prepared);
        assets.push(asset);
    }
    store.activate_catalog(
        &assets,
        &sources,
        &memberships,
        "synthetic-promotional-inputs",
    )?;
    let settings = store.settings()?;
    store.update_settings(
        settings.revision,
        json!({"sources":[SourceRef::library()],"interval_seconds":3600,"paused":true}),
    )?;
    store.set_meta("ready-v2", &serde_json::to_value(ready)?)?;
    atomic_json(
        &profile.join("preparation-report.json"),
        &json!({"inputs":"ai-generated","network_configured":false,"desktop_applied":false,"photos":report}),
    )?;
    println!(
        "{}",
        json!({"prepared":4,"network_configured":false,"desktop_applied":false})
    );
    Ok(())
}
