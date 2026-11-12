//! Single-worker preparation with persistent features and immutable rendered outputs.
pub mod decode;
use crate::{
    model::{Asset, Enhancements},
    storage::{Store, atomic_bytes, private_dir},
};
use anyhow::{Context, Result, ensure};
use image::{
    DynamicImage, RgbImage,
    imageops::{self, FilterType},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;
const ANALYSIS_VERSION: &str = "prepare-v2-2";
const RENDER_VERSION: &str = "prepare-v2-3";
#[derive(Clone, Copy, Default, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Features {
    pub width: u32,
    pub height: u32,
    pub focus: [f64; 2],
    pub faces: Vec<Rect>,
    pub quality: f64,
    pub brightness: f64,
    pub fingerprint: u64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct PrepareOptions {
    pub width: u32,
    pub height: u32,
    pub person_sensitive: bool,
    pub fallback: String,
    pub enhancements: Enhancements,
    pub preferred_crop: Option<Rect>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Prepared {
    pub asset_id: String,
    pub revision: String,
    pub render_path: PathBuf,
    pub render_digest: String,
    pub config_key: String,
    pub rect: Rect,
    pub mode: String,
    pub fingerprint: u64,
    pub score: f64,
    #[serde(default)]
    pub source_ids: Vec<String>,
    pub source_width: u32,
    pub source_height: u32,
    pub width: u32,
    pub height: u32,
    pub rendition: String,
    #[serde(default)]
    pub provenance: String,
    #[serde(default)]
    pub applied_at: Option<chrono::DateTime<chrono::Utc>>,
    pub decode_count: u32,
    pub cache_hit: bool,
    pub analysis_cache_hit: bool,
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn check(stop: &CancellationToken) -> Result<()> {
    ensure!(!stop.is_cancelled(), "preparation_cancelled");
    Ok(())
}
pub fn analyze(rgb: &RgbImage) -> Result<Features> {
    let thumb = DynamicImage::ImageRgb8(imageops::thumbnail(rgb, 512, 512)).into_luma8();
    let (w, h) = thumb.dimensions();
    ensure!(w > 2 && h > 2, "image_too_small");
    let small = imageops::resize(&thumb, 9, 8, FilterType::Triangle);
    let mut fingerprint = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            fingerprint = (fingerprint << 1) | u64::from(small[(x, y)][0] > small[(x + 1, y)][0]);
        }
    }
    let mut total = 0f64;
    let mut cx = 0.;
    let mut cy = 0.;
    let mut sharpness = 0.;
    let mut light = 0.;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let c = thumb[(x, y)][0] as f64;
            light += c;
            let edge = (4. * c
                - thumb[(x - 1, y)][0] as f64
                - thumb[(x + 1, y)][0] as f64
                - thumb[(x, y - 1)][0] as f64
                - thumb[(x, y + 1)][0] as f64)
                .abs();
            let weight = edge
                * (0.35
                    + 0.65
                        * (1.
                            - ((x as f64 / w as f64 - 0.5).abs()
                                + (y as f64 / h as f64 - 0.5).abs())
                            .min(1.)));
            total += weight;
            cx += weight * x as f64;
            cy += weight * y as f64;
            sharpness += edge;
        }
    }
    let pixels = ((w - 2) * (h - 2)) as f64;
    let brightness = light / pixels / 255.;
    let quality =
        (sharpness / pixels / 25.).min(1.) * 0.6 + (1. - (brightness - 0.5).abs() * 2.) * 0.4;
    let focus = if total > 0. {
        [
            cx / total / w as f64 * rgb.width() as f64,
            cy / total / h as f64 * rgb.height() as f64,
        ]
    } else {
        [rgb.width() as f64 / 2., rgb.height() as f64 / 2.]
    };
    let model = rustface::read_model(std::io::Cursor::new(include_bytes!(
        "../../assets/seeta_fd_frontal_v1.0.bin"
    )))?;
    let mut detector = rustface::create_detector_with_model(model);
    detector.set_min_face_size(32);
    detector.set_score_thresh(3.);
    detector.set_pyramid_scale_factor(0.75);
    detector.set_slide_window_step(4, 4);
    let data = rustface::ImageData::new(thumb.as_raw(), w, h);
    let mut faces = Vec::new();
    for face in detector.detect(&data) {
        let b = face.bbox();
        let sx = rgb.width() as f64 / w as f64;
        let sy = rgb.height() as f64 / h as f64;
        let x = ((b.x() as f64 - b.width() as f64 * 0.12) * sx).max(0.) as u32;
        let y = ((b.y() as f64 - b.height() as f64 * 0.18) * sy).max(0.) as u32;
        faces.push(Rect {
            x,
            y,
            width: ((b.width() as f64 * 1.24 * sx) as u32).min(rgb.width() - x),
            height: ((b.height() as f64 * 1.36 * sy) as u32).min(rgb.height() - y),
        });
    }
    Ok(Features {
        width: rgb.width(),
        height: rgb.height(),
        focus,
        faces,
        quality,
        brightness,
        fingerprint,
    })
}
fn valid_rect(r: Rect, w: u32, h: u32) -> bool {
    r.width > 0
        && r.height > 0
        && r.x.checked_add(r.width).is_some_and(|x| x <= w)
        && r.y.checked_add(r.height).is_some_and(|y| y <= h)
}
pub fn choose_crop(f: &Features, o: &PrepareOptions) -> Result<(Rect, String)> {
    ensure!(
        o.width > 0
            && o.height > 0
            && o.width <= 16384
            && o.height <= 16384
            && o.width as u64 * o.height as u64 <= 40_000_000,
        "invalid_display_geometry"
    );
    ensure!(f.width > 0 && f.height > 0, "invalid_source_geometry");
    let full = Rect {
        x: 0,
        y: 0,
        width: f.width,
        height: f.height,
    };
    if let Some(rect) = o.preferred_crop {
        ensure!(
            valid_rect(rect, f.width, f.height),
            "invalid_preferred_crop"
        );
        return Ok((rect, "fit".into()));
    }
    let aspect = o.width as f64 / o.height as f64;
    let cw = (f.width as f64).min(f.height as f64 * aspect).round() as u32;
    let ch = (cw as f64 / aspect).round() as u32;
    // A face detector cannot establish that smaller, obscured subjects are absent.
    // Visual QA found an undetected child outside an aggressive portrait crop.
    let aggressive_portrait =
        f.height > f.width && (cw as f64 * ch as f64) / (f.width as f64 * f.height as f64) < 0.6;
    if !o.person_sensitive
        && !aggressive_portrait
        && cw as f64 / o.width as f64 >= 0.8
        && ch as f64 / o.height as f64 >= 0.8
    {
        let mut anchors = vec![[f.width as f64 / 2., f.height as f64 / 2.], f.focus];
        if !f.faces.is_empty() {
            let x1 = f.faces.iter().map(|r| r.x).min().unwrap();
            let x2 = f.faces.iter().map(|r| r.x + r.width).max().unwrap();
            let y1 = f.faces.iter().map(|r| r.y).min().unwrap();
            let y2 = f.faces.iter().map(|r| r.y + r.height).max().unwrap();
            anchors.push([(x1 + x2) as f64 / 2., (y1 + y2) as f64 / 2.]);
        }
        for x in [0.33, 0.5, 0.67] {
            for y in [0.25, 0.5, 0.75] {
                anchors.push([f.width as f64 * x, f.height as f64 * y]);
            }
        }
        let mut candidates = Vec::new();
        for anchor in anchors {
            let rect = Rect {
                x: (anchor[0] - cw as f64 / 2.)
                    .clamp(0., (f.width - cw) as f64)
                    .round() as u32,
                y: (anchor[1] - ch as f64 / 2.)
                    .clamp(0., (f.height - ch) as f64)
                    .round() as u32,
                width: cw,
                height: ch,
            };
            if f.faces.iter().any(|r| {
                rect.x > r.x
                    || rect.y > r.y
                    || rect.x + rect.width < r.x + r.width
                    || rect.y + rect.height < r.y + r.height
            }) {
                continue;
            }
            let rx = (f.focus[0] - rect.x as f64) / cw as f64;
            let ry = (f.focus[1] - rect.y as f64) / ch as f64;
            let score = [0.33, 0.5, 0.67]
                .iter()
                .map(|t| (rx - t).abs())
                .fold(f64::INFINITY, f64::min)
                + [0.33, 0.5, 0.67]
                    .iter()
                    .map(|t| (ry - t).abs())
                    .fold(f64::INFINITY, f64::min);
            candidates.push((score, rect));
        }
        candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, rect)) = candidates.first() {
            return Ok((*rect, "fill".into()));
        }
    }
    ensure!(o.fallback == "fit", "no_safe_crop");
    let scale = (o.width as f64 / f.width as f64).min(o.height as f64 / f.height as f64);
    ensure!(scale <= 1.5, "insufficient_image_resolution");
    Ok((full, "fit".into()))
}
pub fn config_key(o: &PrepareOptions) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&(RENDER_VERSION, o))?))
}
pub fn cached_render(
    store: &Store,
    asset: &Asset,
    options: &PrepareOptions,
) -> Result<Option<Prepared>> {
    let key = config_key(options)?;
    let raw:Option<String>=store.connection()?.query_row("SELECT data FROM renders WHERE asset_id=?1 AND revision=?2 AND config_key=?3 AND length(data)<=1048576 ORDER BY used DESC LIMIT 1",params![asset.id,asset.revision,key],|r|r.get(0)).optional()?;
    let Some(mut cached) = raw.and_then(|v| serde_json::from_str::<Prepared>(&v).ok()) else {
        return Ok(None);
    };
    if cached.asset_id != asset.id
        || cached.revision != asset.revision
        || cached.config_key != key
        || !crate::storage::read_bounded(&cached.render_path, 100 * 1024 * 1024)
            .is_ok_and(|bytes| digest(&bytes) == cached.render_digest)
    {
        return Ok(None);
    }
    cached.cache_hit = true;
    cached.analysis_cache_hit = true;
    cached.decode_count = 0;
    cached.applied_at = None;
    Ok(Some(cached))
}
pub fn cached_ids(store: &Store, options: &PrepareOptions) -> Result<Vec<String>> {
    let db = store.connection()?;
    let mut query=db.prepare("SELECT r.asset_id,r.data FROM renders r JOIN assets a ON r.asset_id=a.id AND r.revision=a.revision WHERE r.config_key=?1 AND length(r.data)<=1048576")?;
    let mut ids = HashSet::new();
    for row in query.query_map([config_key(options)?], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (id, data) = row?;
        if let Ok(cached) = serde_json::from_str::<Prepared>(&data)
            && cached.asset_id == id
            && std::fs::metadata(&cached.render_path)
                .is_ok_and(|m| m.is_file() && m.len() <= 100 * 1024 * 1024)
        {
            ids.insert(id);
        }
    }
    let mut ids: Vec<_> = ids.into_iter().collect();
    ids.sort();
    Ok(ids)
}
pub fn prepare(
    store: &Store,
    asset: &Asset,
    path: &Path,
    o: &PrepareOptions,
    stop: &CancellationToken,
) -> Result<Prepared> {
    prepare_with_decoded(store, asset, path, o, stop, None)
}
pub fn prepare_with_decoded(
    store: &Store,
    asset: &Asset,
    path: &Path,
    o: &PrepareOptions,
    stop: &CancellationToken,
    decoded: Option<(String, RgbImage)>,
) -> Result<Prepared> {
    check(stop)?;
    let bytes =
        crate::storage::read_bounded(path, 150 * 1024 * 1024).context("media_unavailable")?;
    ensure!(bytes.len() <= 150 * 1024 * 1024, "image_size_limit");
    let content = digest(&bytes);
    if let Some((expected, _)) = &decoded {
        ensure!(expected == &content, "source_changed_during_preparation");
    }
    let analysis_key =
        digest(format!("{ANALYSIS_VERSION}:{content}:{}", asset.orientation).as_bytes());
    let config_key = config_key(o)?;
    let lookup_key = digest(format!("{analysis_key}:{config_key}").as_bytes());
    let db = store.connection()?;
    if let Some(data) = db
        .query_row(
            "SELECT data FROM renders WHERE key=?1",
            [&lookup_key],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        && let Ok(mut cached) = serde_json::from_str::<Prepared>(&data)
        && let Ok(file) = crate::storage::read_bounded(&cached.render_path, 100 * 1024 * 1024)
        && digest(&file) == cached.render_digest
    {
        cached.asset_id = asset.id.clone();
        cached.revision = asset.revision.clone();
        cached.rendition = asset.rendition.clone();
        cached.provenance = if asset.library_id.starts_with("icloud:") {
            "icloud"
        } else {
            "local"
        }
        .into();
        cached.applied_at = None;
        cached.cache_hit = true;
        cached.analysis_cache_hit = true;
        cached.decode_count = 0;
        return Ok(cached);
    }
    check(stop)?;
    let cached: Option<Features> = db
        .query_row(
            "SELECT data FROM analysis WHERE key=?1",
            [&analysis_key],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .and_then(|s| serde_json::from_str(&s).ok());
    let rgb = match decoded {
        Some((expected, rgb)) => {
            ensure!(expected == content, "source_changed_during_preparation");
            rgb
        }
        None => decode::decode(&bytes, asset.orientation)?,
    };
    let cached = cached.filter(|f| {
        f.width == rgb.width()
            && f.height == rgb.height()
            && f.quality.is_finite()
            && f.brightness.is_finite()
            && f.focus.iter().all(|n| n.is_finite())
            && f.faces.len() <= 1000
            && f.faces.iter().all(|r| valid_rect(*r, f.width, f.height))
    });
    let analysis_cache_hit = cached.is_some();
    check(stop)?;
    let f = if let Some(f) = cached {
        f
    } else {
        let f = analyze(&rgb)?;
        check(stop)?;
        db.execute(
            "INSERT OR REPLACE INTO analysis VALUES(?1,?2)",
            params![analysis_key, serde_json::to_string(&f)?],
        )?;
        f
    };
    let (rect, mode) = choose_crop(&f, o)?;
    check(stop)?;
    let cropped = imageops::crop_imm(&rgb, rect.x, rect.y, rect.width, rect.height).to_image();
    drop(rgb);
    let resized = imageops::resize(
        &cropped,
        if mode == "fill" {
            o.width
        } else {
            (rect.width as f64
                * (o.width as f64 / rect.width as f64).min(o.height as f64 / rect.height as f64))
            .round()
            .max(1.) as u32
        },
        if mode == "fill" {
            o.height
        } else {
            (rect.height as f64
                * (o.width as f64 / rect.width as f64).min(o.height as f64 / rect.height as f64))
            .round()
            .max(1.) as u32
        },
        FilterType::Lanczos3,
    );
    drop(cropped);
    let mut output = if mode == "fill" {
        resized
    } else {
        // Blur a small backdrop, then enlarge it. Full-resolution blur would
        // add substantial CPU without useful detail behind the sharp photo.
        let mut backdrop = DynamicImage::ImageRgb8(resized.clone())
            .resize_to_fill(256, 160, FilterType::Triangle)
            .to_rgb8();
        backdrop = imageops::blur(&backdrop, 8.);
        for pixel in backdrop.pixels_mut() {
            for channel in &mut pixel.0 {
                *channel = (*channel as f32 * 0.20).round() as u8;
            }
        }
        let mut canvas = imageops::resize(&backdrop, o.width, o.height, FilterType::Triangle);
        let x = (o.width - resized.width()) / 2;
        let y = (o.height - resized.height()) / 2;
        imageops::replace(&mut canvas, &resized, x.into(), y.into());
        canvas
    };
    adjust(&mut output, &o.enhancements, f.brightness);
    check(stop)?;
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 92).encode_image(&output)?;
    check(stop)?;
    let render_digest = digest(&encoded);
    let directory = store.paths.data.join("renders-v2");
    private_dir(&directory)?;
    let render_path = directory.join(format!("{lookup_key}.jpg"));
    atomic_bytes(&render_path, &encoded)?;
    let result = Prepared {
        asset_id: asset.id.clone(),
        revision: asset.revision.clone(),
        render_path,
        render_digest,
        config_key,
        rect,
        mode,
        fingerprint: f.fingerprint,
        score: f.quality,
        source_ids: Vec::new(),
        source_width: f.width,
        source_height: f.height,
        width: o.width,
        height: o.height,
        rendition: asset.rendition.clone(),
        provenance: if asset.library_id.starts_with("icloud:") {
            "icloud"
        } else {
            "local"
        }
        .into(),
        applied_at: None,
        decode_count: 1,
        cache_hit: false,
        analysis_cache_hit,
    };
    check(stop)?;
    db.execute(
        "INSERT OR REPLACE INTO renders VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            lookup_key,
            asset.id,
            asset.revision,
            result.config_key,
            serde_json::to_string(&result)?,
            chrono::Utc::now().timestamp()
        ],
    )?;
    Ok(result)
}
fn adjust(image: &mut RgbImage, options: &Enhancements, brightness: f64) {
    if !options.exposure && !options.contrast && !options.white_balance {
        return;
    }
    let exposure = if options.exposure {
        (0.48 / brightness.max(0.01)).clamp(0.9, 1.12)
    } else {
        1.
    };
    let mut balance = [1.; 3];
    if options.white_balance {
        let mut sums = [0f64; 3];
        for p in image.pixels() {
            for c in 0..3 {
                sums[c] += p[c] as f64;
            }
        }
        let mean = sums.iter().sum::<f64>() / 3.;
        for c in 0..3 {
            balance[c] = (mean / sums[c].max(1.)).clamp(0.97, 1.03);
        }
    }
    let contrast = if options.contrast { 1.035 } else { 1. };
    for p in image.pixels_mut() {
        for c in 0..3 {
            p[c] = ((p[c] as f64 * exposure * balance[c] - 127.5) * contrast + 127.5)
                .clamp(0., 255.)
                .round() as u8;
        }
    }
}
pub fn evict(store: &Store, budget: u64, protected: &[PathBuf]) -> Result<u64> {
    let protected: HashSet<_> = protected.iter().collect();
    let mut files = Vec::new();
    let mut total = 0u64;
    for folder in ["media-v2", "renders-v2", "privacy/previews"] {
        let dir = store.paths.data.join(folder);
        let metadata = match std::fs::symlink_metadata(&dir) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "unsafe_cache_directory"
        );
        crate::storage::validate_private_parents(&dir.join("entry"))?;
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let meta = std::fs::symlink_metadata(&path)?;
            if !meta.is_file() || meta.file_type().is_symlink() {
                continue;
            }
            total += meta.len();
            files.push((meta.modified()?, path, meta.len()));
        }
    }
    files.sort_by_key(|r| r.0);
    for (_, path, size) in files {
        if total <= budget {
            break;
        }
        if protected.contains(&path) {
            continue;
        }
        std::fs::remove_file(&path)?;
        total = total.saturating_sub(size);
    }
    Ok(total)
}
