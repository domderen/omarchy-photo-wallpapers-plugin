//! Bounded, side-effect-free Matugen JSON generation with paired atomic caches.
use crate::storage::{Store, atomic_bytes, private_dir, read_bounded};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;
const VERSION: &str = "material-omarchy-v1";

pub fn thumbnail_path(store: &Store, render_digest: &str) -> PathBuf {
    store
        .paths
        .data
        .join("theme-inputs-v1")
        .join(format!("{}.jpg", hash(render_digest.as_bytes())))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Palette {
    pub mode: String,
    pub colors: BTreeMap<String, String>,
}
impl Palette {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            ["light", "dark"].contains(&self.mode.as_str()) && self.colors.len() <= 64,
            "invalid_palette"
        );
        for (key, color) in &self.colors {
            ensure!(
                !key.is_empty()
                    && key.len() <= 40
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "invalid_palette"
            );
            rgb(color)?;
        }
        for key in [
            "background",
            "foreground",
            "accent",
            "selection",
            "muted",
            "red",
            "green",
            "yellow",
            "blue",
            "magenta",
            "cyan",
            "color0",
            "color15",
        ] {
            ensure!(self.colors.contains_key(key), "invalid_palette");
        }
        ensure!(
            self.contrast("background", "foreground")? >= 4.5
                && self.contrast("background", "accent")? >= 3.,
            "invalid_palette"
        );
        Ok(())
    }
    pub fn contrast(&self, a: &str, b: &str) -> Result<f64> {
        let a = self
            .colors
            .get(a)
            .ok_or_else(|| anyhow::anyhow!("invalid_palette"))?;
        let b = self
            .colors
            .get(b)
            .ok_or_else(|| anyhow::anyhow!("invalid_palette"))?;
        contrast(a, b)
    }
    pub fn toml(&self) -> Result<String> {
        self.validate()?;
        let mut text = format!(
            "# Generated Photo Wallpapers palette\nmode = \"{}\"\n",
            self.mode
        );
        for (key, value) in &self.colors {
            text.push_str(&format!("{key} = \"{value}\"\n"));
        }
        Ok(text)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Suitability {
    pub brightness: f64,
    pub highlights: f64,
    pub day_score: f64,
}
impl Default for Suitability {
    fn default() -> Self {
        Self {
            brightness: 0.5,
            highlights: 0.,
            day_score: 0.5,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pair {
    pub light: Palette,
    pub dark: Palette,
    pub suitability: Suitability,
}
impl Pair {
    pub fn from_matugen(value: &Value) -> Result<Self> {
        let make = |mode: &str| -> Result<Palette> {
            let role = |name: &str| -> Result<String> {
                let color = value["colors"][name][mode]["color"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("invalid_palette"))?;
                rgb(color)?;
                Ok(color.to_ascii_lowercase())
            };
            let mut colors = BTreeMap::new();
            for (target, source) in [
                ("background", "background"),
                ("foreground", "on_background"),
                ("accent", "primary"),
                ("dark_background", "surface_container_lowest"),
                ("darker_background", "surface_container_lowest"),
                ("lighter_background", "surface_container_high"),
                ("dark_foreground", "on_surface_variant"),
                ("light_foreground", "on_background"),
                ("bright_foreground", "on_background"),
                ("muted", "on_surface_variant"),
                ("selection", "primary_container"),
                ("selection_foreground", "on_primary_container"),
                ("selection_background", "primary_container"),
                ("red", "error"),
            ] {
                colors.insert(target.into(), role(source)?);
            }
            // Semantic terminal hues must remain recognizable. Matugen's raw
            // Base16 entries can be near-neutral/low-contrast for dark inputs.
            // Keep semantic ANSI anchors while photo-derived Material surfaces,
            // selection and accents carry the wallpaper's colour identity.
            let anchors = if mode == "light" {
                [
                    ("green", "#256d32"),
                    ("yellow", "#795500"),
                    ("blue", "#305da8"),
                    ("magenta", "#8e4585"),
                    ("cyan", "#006b73"),
                ]
            } else {
                [
                    ("green", "#81c784"),
                    ("yellow", "#e5c07b"),
                    ("blue", "#82aaff"),
                    ("magenta", "#c792ea"),
                    ("cyan", "#89ddff"),
                ]
            };
            for (name, color) in anchors {
                colors.insert(name.into(), color.into());
            }
            let ansi = [
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
            ];
            for (i, key) in ansi.into_iter().enumerate() {
                let value = colors[key].clone();
                colors.insert(format!("color{i}"), value);
            }
            let palette = Palette {
                mode: mode.into(),
                colors,
            };
            palette.validate()?;
            Ok(palette)
        };
        let pair = Self {
            light: make("light")?,
            dark: make("dark")?,
            suitability: Suitability::default(),
        };
        pair.validate()?;
        Ok(pair)
    }
    pub fn validate(&self) -> Result<()> {
        self.light.validate()?;
        self.dark.validate()?;
        ensure!(
            self.light.mode == "light"
                && self.dark.mode == "dark"
                && [
                    self.suitability.brightness,
                    self.suitability.highlights,
                    self.suitability.day_score
                ]
                .into_iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(&v)),
            "invalid_palette"
        );
        Ok(())
    }
}
fn rgb(color: &str) -> Result<[f64; 3]> {
    ensure!(
        color.len() == 7
            && color.starts_with('#')
            && color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit),
        "invalid_palette"
    );
    Ok([
        u8::from_str_radix(&color[1..3], 16)?,
        u8::from_str_radix(&color[3..5], 16)?,
        u8::from_str_radix(&color[5..7], 16)?,
    ]
    .map(|v| f64::from(v) / 255.))
}
fn luminance(color: &str) -> Result<f64> {
    let channels = rgb(color)?.map(|v| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    });
    Ok(channels
        .into_iter()
        .zip([0.2126, 0.7152, 0.0722])
        .map(|(a, b)| a * b)
        .sum())
}
pub(super) fn contrast(a: &str, b: &str) -> Result<f64> {
    let a = luminance(a)?;
    let b = luminance(b)?;
    Ok((a.max(b) + 0.05) / (a.min(b) + 0.05))
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    key: String,
    checksum: String,
    pair: Pair,
}
pub struct PreparedPair {
    pub pair: Pair,
    pub cache_path: PathBuf,
    pub cache_hit: bool,
}
#[derive(Clone)]
pub struct Generator {
    program: PathBuf,
    pub version: String,
    timeout: Duration,
}
impl Generator {
    pub fn for_program(program: PathBuf, version: String, timeout: Duration) -> Self {
        Self {
            program,
            version,
            timeout,
        }
    }
    pub async fn detect(store: &Store) -> Result<Self> {
        let result = output(
            Path::new("matugen"),
            &["--version".into()],
            Duration::from_secs(5),
            &CancellationToken::new(),
            4096,
        )
        .await;
        let version = result
            .ok()
            .and_then(|v| String::from_utf8(v).ok())
            .map(|v| v.trim().to_string())
            .filter(|v| valid_version(v));
        let version = if let Some(version) = version {
            store.set_meta("theme-generator-version", &json!(version))?;
            version
        } else {
            store
                .meta("theme-generator-version")?
                .as_str()
                .filter(|v| valid_version(v))
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("theme_generator_unavailable"))?
        };
        Ok(Self::for_program(
            "matugen".into(),
            version,
            Duration::from_secs(15),
        ))
    }
    pub async fn prepare(
        &self,
        store: &Store,
        render: &Path,
        render_digest: &str,
        stop: &CancellationToken,
    ) -> Result<PreparedPair> {
        ensure!(!stop.is_cancelled(), "theme_cancelled");
        ensure!(render_digest.len() <= 128, "invalid_palette");
        let key = hash(format!("{VERSION}:{}:{render_digest}", self.version).as_bytes());
        let directory = store.paths.data.join("theme-palettes-v1");
        private_dir(&directory)?;
        let path = directory.join(format!("{key}.json"));
        if let Ok(bytes) = read_bounded(&path, 256 * 1024)
            && let Ok(cache) = serde_json::from_slice::<Cache>(&bytes)
            && cache.key == key
            && cache.pair.validate().is_ok()
            && cache.checksum == hash(&serde_json::to_vec(&cache.pair)?)
        {
            return Ok(PreparedPair {
                pair: cache.pair,
                cache_path: path,
                cache_hit: true,
            });
        }
        let input_dir = store.paths.data.join("theme-inputs-v1");
        private_dir(&input_dir)?;
        let input = thumbnail_path(store, render_digest);
        let input_for_worker = input.clone();
        let render = render.to_path_buf();
        let worker_stop = stop.clone();
        let suitability = tokio::task::spawn_blocking(move || {
            thumbnail(&render, &input_for_worker, &worker_stop)
        })
        .await??;
        ensure!(!stop.is_cancelled(), "theme_cancelled");
        let config = store.paths.config.join("matugen-private.toml");
        let config_bytes = b"[config]\n[templates]\n";
        if read_bounded(&config, 1024).ok().as_deref() != Some(config_bytes.as_slice()) {
            atomic_bytes(&config, config_bytes)?;
        }
        let args = vec![
            "--config".into(),
            config.into_os_string(),
            "--dry-run".into(),
            "--json".into(),
            "hex".into(),
            "--include-image-in-json".into(),
            "false".into(),
            "--type".into(),
            "scheme-tonal-spot".into(),
            "--source-color-index".into(),
            "0".into(),
            "--fallback-color".into(),
            "#7aa2f7".into(),
            "image".into(),
            input.into_os_string(),
        ];
        let bytes = output(&self.program, &args, self.timeout, stop, 1024 * 1024).await?;
        let value = serde_json::from_slice::<Value>(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid_palette"))?;
        let mut pair = Pair::from_matugen(&value)?;
        pair.suitability = suitability;
        ensure!(!stop.is_cancelled(), "theme_cancelled");
        pair.validate()?;
        let cache = Cache {
            key,
            checksum: hash(&serde_json::to_vec(&pair)?),
            pair: pair.clone(),
        };
        atomic_bytes(&path, &serde_json::to_vec(&cache)?)?;
        store.set_meta(
            &format!("suitability-v1:{render_digest}"),
            &serde_json::to_value(&pair.suitability)?,
        )?;
        // Only cold preparation scans these small, bounded directories. Protect
        // the displayed/queued pairs; warm rotations and solar switches do no GC.
        let mut protected = vec![path.clone()];
        let mut images = store
            .meta("ready-v2")?
            .as_array()
            .cloned()
            .unwrap_or_default();
        images.push(store.meta("current-v2")?);
        for image in images.iter().take(8) {
            if let Some(digest) = image["render_digest"].as_str() {
                let key = hash(format!("{VERSION}:{}:{digest}", self.version).as_bytes());
                protected.push(directory.join(format!("{key}.json")));
            }
        }
        trim_cache(&directory, &protected)?;
        trim_cache(&input_dir, &[])?;
        Ok(PreparedPair {
            pair,
            cache_path: path,
            cache_hit: false,
        })
    }
}
fn valid_version(version: &str) -> bool {
    version.starts_with("matugen 4.")
        && version.len() <= 80
        && !version.chars().any(char::is_control)
}
fn trim_cache(directory: &Path, protected: &[PathBuf]) -> Result<()> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let stem = path.file_stem().and_then(|v| v.to_str()).unwrap_or("");
        if stem.len() != 64 || !stem.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_file() {
            entries.push((meta.modified()?, meta.len(), path));
        }
    }
    let mut count = entries.len();
    let mut bytes = entries.iter().map(|e| e.1).sum::<u64>();
    entries.sort_by_key(|e| e.0);
    for (_, size, path) in entries {
        if count <= 512 && bytes <= 32 * 1024 * 1024 {
            break;
        }
        if !protected.contains(&path) {
            std::fs::remove_file(path)?;
            count -= 1;
            bytes = bytes.saturating_sub(size);
        }
    }
    Ok(())
}

fn thumbnail(render: &Path, input: &Path, stop: &CancellationToken) -> Result<Suitability> {
    ensure!(!stop.is_cancelled(), "theme_cancelled");
    let bytes = read_bounded(render, 100 * 1024 * 1024)
        .map_err(|_| anyhow::anyhow!("theme_image_unavailable"))?;
    let reader = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format()?;
    let (w, h) = reader.into_dimensions()?;
    ensure!(
        u64::from(w) * u64::from(h) <= 64_000_000,
        "theme_image_too_large"
    );
    let image = image::load_from_memory(&bytes)?
        .thumbnail(256, 256)
        .to_rgb8();
    let mut brightness = 0.;
    let mut highlights = 0.;
    for pixel in image.pixels() {
        let value = (f64::from(pixel[0]) * 0.2126
            + f64::from(pixel[1]) * 0.7152
            + f64::from(pixel[2]) * 0.0722)
            / 255.;
        brightness += value;
        highlights += f64::from(value > 0.85);
    }
    let count = f64::from(image.width()) * f64::from(image.height());
    brightness /= count;
    highlights /= count;
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 85).encode_image(&image)?;
    ensure!(!stop.is_cancelled(), "theme_cancelled");
    atomic_bytes(input, &encoded)?;
    Ok(Suitability {
        brightness,
        highlights,
        day_score: (brightness * 1.4 + highlights * 0.8).clamp(0., 1.),
    })
}
async fn output(
    program: &Path,
    args: &[std::ffi::OsString],
    timeout: Duration,
    stop: &CancellationToken,
    limit: u64,
) -> Result<Vec<u8>> {
    let work = async {
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| anyhow::anyhow!("theme_generator_unavailable"))?;
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("palette_generation_failed"))?
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .await?;
        ensure!(bytes.len() as u64 <= limit, "palette_output_too_large");
        ensure!(child.wait().await?.success(), "palette_generation_failed");
        Ok(bytes)
    };
    tokio::select! {_=stop.cancelled()=>anyhow::bail!("theme_cancelled"),result=tokio::time::timeout(timeout,work)=>result.map_err(|_|anyhow::anyhow!("palette_generation_timed_out"))?}
}

#[cfg(test)]
mod maintenance_tests {
    use super::*;
    #[test]
    fn bounded_cache_preserves_the_current_entry_and_unrelated_files() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..520 {
            std::fs::write(root.path().join(format!("{i:064x}.json")), b"fixture").unwrap();
        }
        let protected = root.path().join(format!("{:064x}.json", 0));
        std::fs::write(root.path().join("user-note.txt"), b"keep").unwrap();
        trim_cache(root.path(), std::slice::from_ref(&protected)).unwrap();
        assert!(protected.exists() && root.path().join("user-note.txt").exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 513);
    }
}
