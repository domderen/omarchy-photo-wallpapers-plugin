//! CPU-only native execution; called exclusively inside the short-lived worker.
use super::{
    models::{ModelRole, ModelSet, ModelSpec},
    policy::{Evidence, Signals},
    worker::{ScreeningOutput, WorkerInput},
};
use anyhow::{Context, Result, ensure};
use image::{
    ImageFormat, ImageReader, Limits,
    imageops::{self, FilterType},
};
use std::{collections::BTreeMap, io::Cursor};

pub fn full_frame_tensor(png: &[u8], model: &ModelSpec) -> Result<Vec<f32>> {
    ensure!(png.len() <= 768 * 1024, "privacy_preview_limit");
    let mut reader = ImageReader::with_format(Cursor::new(png), ImageFormat::Png);
    let mut limits = Limits::default();
    limits.max_image_width = Some(512);
    limits.max_image_height = Some(512);
    limits.max_alloc = Some(4 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().context("invalid_privacy_preview")?;
    ensure!(
        image.width() > 2 && image.height() > 2 && image.color() == image::ColorType::Rgb8,
        "invalid_privacy_preview"
    );
    let width = image.width();
    let height = image.height();
    let scale = 224.0 / (width.max(height) as f64);
    let w = ((width as f64 * scale).round() as u32).clamp(1, 224);
    let h = ((height as f64 * scale).round() as u32).clamp(1, 224);
    let thumb = imageops::resize(&image.to_rgb8(), w, h, FilterType::CatmullRom);
    let mut canvas = image::RgbImage::from_pixel(
        224,
        224,
        image::Rgb(model.mean.map(|v| (v * 255.0).round() as u8)),
    );
    imageops::replace(
        &mut canvas,
        &thumb,
        ((224 - w) / 2).into(),
        ((224 - h) / 2).into(),
    );
    let mut tensor = vec![0.0; 3 * 224 * 224];
    for (x, y, pixel) in canvas.enumerate_pixels() {
        for c in 0..3 {
            tensor[c * 224 * 224 + (y * 224 + x) as usize] =
                (pixel[c] as f32 / 255.0 - model.mean[c]) / model.std[c];
        }
    }
    ensure!(
        tensor.iter().all(|x| x.is_finite()),
        "invalid_privacy_tensor"
    );
    Ok(tensor)
}
fn evidence(allowed: f32, sensitive: f32, clear: f32, blocked: f32) -> Evidence {
    if sensitive - allowed >= blocked {
        Evidence::Blocked
    } else if allowed - sensitive >= clear {
        Evidence::Clear
    } else {
        Evidence::Uncertain
    }
}
pub fn context_signals(
    scores: &BTreeMap<String, f32>,
    clear: f32,
    blocked: f32,
) -> Result<Signals> {
    ensure!(
        scores.len() == 8
            && scores
                .values()
                .all(|v| v.is_finite() && (-1.01..=1.01).contains(v)),
        "invalid_privacy_scores"
    );
    let score = |k: &str| scores.get(k).copied().context("invalid_privacy_scores");
    let allowed = [score("ordinary")?, score("swimwear")?, score("non_person")?]
        .into_iter()
        .fold(f32::NEG_INFINITY, f32::max);
    Ok(Signals {
        nudity: evidence(allowed, score("nudity")?, clear, blocked),
        underwear: evidence(allowed, score("underwear")?, clear, blocked),
        bathing: evidence(allowed, score("bathing")?, clear, blocked),
        sleepwear: evidence(allowed, score("sleepwear")?, clear, blocked),
        swimwear: if score("swimwear")? >= allowed - 0.001 {
            Evidence::Clear
        } else {
            Evidence::Uncertain
        },
    })
}
fn merge(a: Evidence, b: Evidence) -> Evidence {
    if a == Evidence::Blocked || b == Evidence::Blocked {
        Evidence::Blocked
    } else if a == Evidence::Clear && b == Evidence::Clear {
        Evidence::Clear
    } else {
        Evidence::Uncertain
    }
}
pub fn combine_contexts(a: Signals, b: Signals) -> Signals {
    Signals {
        nudity: merge(a.nudity, b.nudity),
        underwear: merge(a.underwear, b.underwear),
        bathing: merge(a.bathing, b.bathing),
        sleepwear: merge(a.sleepwear, b.sleepwear),
        swimwear: if a.swimwear == Evidence::Clear || b.swimwear == Evidence::Clear {
            Evidence::Clear
        } else {
            Evidence::Uncertain
        },
    }
}
pub fn screen(input: WorkerInput) -> Result<ScreeningOutput> {
    ensure!(
        input.schema_version == 1 && uuid::Uuid::parse_str(&input.request_id).is_ok(),
        "invalid_privacy_worker_input"
    );
    let png = input.preview.pixels()?;
    let set = ModelSet::load_verified(&input.root, &input.manifest)?;
    let environment = ort::init_from(set.runtime_path())?
        .with_name("photo-wallpapers-privacy")
        .build()?;
    let mut context = None;
    let mut sensitive = None;
    for model in &set.models {
        let tensor = ort::value::Tensor::from_array((
            [1_usize, 3, 224, 224],
            full_frame_tensor(&png, &model.spec)?,
        ))?;
        let mut session = ort::session::Session::builder(&environment)?
            .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()])
            .map_err(ort::Error::<()>::from)?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            .map_err(ort::Error::<()>::from)?
            .with_intra_threads(1)
            .map_err(ort::Error::<()>::from)?
            .with_inter_threads(1)
            .map_err(ort::Error::<()>::from)?
            .with_parallel_execution(false)
            .map_err(ort::Error::<()>::from)?
            .with_intra_op_spinning(false)
            .map_err(ort::Error::<()>::from)?
            .with_inter_op_spinning(false)
            .map_err(ort::Error::<()>::from)?
            .with_memory_pattern(false)
            .map_err(ort::Error::<()>::from)?
            .commit_from_memory(&model.bytes)?;
        let outputs = session.run(ort::inputs!["pixel_values"=>tensor])?;
        let (shape, values) = outputs
            .get(&model.spec.output)
            .context("invalid_privacy_output")?
            .try_extract_tensor::<f32>()?;
        ensure!(
            shape.len() == 2 && shape[0] == 1 && values.iter().all(|v| v.is_finite()),
            "invalid_privacy_output"
        );
        match model.spec.role {
            ModelRole::Sensitive => {
                ensure!(values.len() == 2, "invalid_privacy_output");
                let base = values[0].max(values[1]);
                let p = (values[1] - base).exp()
                    / ((values[0] - base).exp() + (values[1] - base).exp());
                sensitive = Some(if p <= model.spec.clear_margin {
                    Evidence::Clear
                } else if p >= model.spec.blocked_margin {
                    Evidence::Blocked
                } else {
                    Evidence::Uncertain
                });
            }
            ModelRole::Context => {
                let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
                ensure!(norm.is_finite() && norm > 1e-12, "invalid_privacy_output");
                let mut scores = BTreeMap::new();
                for (label, vector) in &model.spec.embeddings {
                    ensure!(vector.len() == values.len(), "invalid_privacy_output");
                    scores.insert(
                        label.clone(),
                        vector.iter().zip(values).map(|(a, b)| a * b / norm).sum(),
                    );
                }
                let next =
                    context_signals(&scores, model.spec.clear_margin, model.spec.blocked_margin)?;
                context = Some(match context.take() {
                    Some(previous) => combine_contexts(previous, next),
                    None => next,
                });
            }
        }
    }
    let mut signals = context.context("privacy_context_model_missing")?;
    signals.nudity = merge(
        signals.nudity,
        sensitive.context("privacy_sensitive_model_missing")?,
    );
    Ok(ScreeningOutput {
        schema_version: 1,
        request_id: input.request_id,
        modelset_sha256: set.identity().into(),
        preview_sha256: input.preview.sha256,
        qualified: set.qualified(),
        signals,
    })
}
