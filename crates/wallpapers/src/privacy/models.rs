//! Verified data-only model artifacts. Verification never initializes native code.
use crate::storage::{private_file, validate_private_parents};
use anyhow::{Context, Result, ensure};
use rustix::fs::OFlags;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};

pub const RUNTIME_VERSION: &str = "1.31.1";
const MODEL_LIMIT: u64 = 512 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub id: String,
    pub path: PathBuf,
    pub format: String,
    pub sha256: String,
    pub bytes: u64,
    pub license: String,
    pub license_source: String,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    Sensitive,
    Context,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSpec {
    pub artifact: Artifact,
    pub role: ModelRole,
    pub output: String,
    pub mean: [f32; 3],
    pub std: [f32; 3],
    #[serde(default)]
    pub embeddings: BTreeMap<String, Vec<f32>>,
    pub clear_margin: f32,
    pub blocked_margin: f32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelManifest {
    pub schema_version: u32,
    pub runtime_version: String,
    pub runtime: Artifact,
    pub models: Vec<ModelSpec>,
    pub preprocessing_version: String,
    pub policy_version: String,
    pub qualified: bool,
}
impl ModelManifest {
    pub fn identity(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1 && self.runtime_version == RUNTIME_VERSION,
            "invalid_privacy_model_version"
        );
        ensure!(
            !self.models.is_empty()
                && self.models.len() <= 3
                && self.preprocessing_version.len() <= 128
                && !self.preprocessing_version.is_empty()
                && !self.policy_version.is_empty()
                && self.policy_version.len() <= 128,
            "invalid_privacy_models"
        );
        ensure!(self.runtime.format == "native", "invalid_privacy_runtime");
        if self.qualified {
            ensure!(
                (2..=3).contains(&self.models.len())
                    && self
                        .models
                        .iter()
                        .filter(|m| m.role == ModelRole::Sensitive)
                        .count()
                        == 1
                    && self.models.iter().any(|m| m.role == ModelRole::Sensitive)
                    && self.models.iter().any(|m| m.role == ModelRole::Context),
                "incomplete_privacy_models"
            );
        }
        for m in &self.models {
            ensure!(
                m.artifact.format == "onnx"
                    && matches!(
                        m.output.as_str(),
                        "logits" | "pooler_output" | "image_embeds"
                    ),
                "invalid_privacy_model"
            );
            ensure!(
                m.mean
                    .into_iter()
                    .all(|x| x.is_finite() && (0.0..=1.0).contains(&x))
                    && m.std
                        .into_iter()
                        .all(|x| x.is_finite() && x > 0.0 && x <= 1.0)
                    && m.clear_margin.is_finite()
                    && m.blocked_margin.is_finite()
                    && (0.0..=1.0).contains(&m.clear_margin)
                    && (0.0..=1.0).contains(&m.blocked_margin),
                "invalid_privacy_preprocessing"
            );
            if m.role == ModelRole::Context && (self.qualified || !m.embeddings.is_empty()) {
                let dims = m
                    .embeddings
                    .values()
                    .next()
                    .context("invalid_privacy_embeddings")?
                    .len();
                ensure!(matches!(dims, 512 | 768), "invalid_privacy_embeddings");
                let labels = [
                    "ordinary",
                    "swimwear",
                    "sleepwear",
                    "underwear",
                    "bathing",
                    "nudity",
                    "non_person",
                    "ordinary_sleeping",
                ];
                ensure!(
                    m.embeddings.len() == labels.len()
                        && labels.into_iter().all(|k| m.embeddings.contains_key(k)),
                    "invalid_privacy_embeddings"
                );
                for vector in m.embeddings.values() {
                    let norm: f32 = vector.iter().map(|x| x * x).sum();
                    ensure!(
                        vector.len() == dims
                            && vector.iter().all(|x| x.is_finite())
                            && (0.99..=1.01).contains(&norm),
                        "invalid_privacy_embeddings"
                    );
                }
            }
        }
        Ok(())
    }
}
fn verify(root: &Path, artifact: &Artifact) -> Result<File> {
    ensure!(
        !artifact.path.as_os_str().is_empty()
            && artifact
                .path
                .components()
                .all(|p| matches!(p, Component::Normal(_))),
        "invalid_privacy_artifact_path"
    );
    ensure!(
        artifact.bytes > 0
            && artifact.bytes <= MODEL_LIMIT
            && artifact.sha256.len() == 64
            && artifact
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid_privacy_artifact"
    );
    ensure!(
        matches!(
            artifact.license.as_str(),
            "MIT" | "Apache-2.0" | "BSD-3-Clause" | "BSD-2-Clause"
        ),
        "unsupported_privacy_model_license"
    );
    let source = reqwest::Url::parse(&artifact.license_source)?;
    ensure!(
        source.scheme() == "https"
            && source.host_str().is_some()
            && source.username().is_empty()
            && source.password().is_none()
            && source.query().is_none()
            && source.fragment().is_none(),
        "invalid_privacy_license_source"
    );
    let path = root.join(&artifact.path);
    validate_private_parents(&path)?;
    let mut file = private_file(&path, OFlags::RDONLY)?;
    ensure!(
        file.metadata()?.len() == artifact.bytes,
        "privacy_artifact_size_mismatch"
    );
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0; 128 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        ensure!(total <= artifact.bytes, "privacy_artifact_size_mismatch");
        digest.update(&buffer[..n]);
    }
    ensure!(
        total == artifact.bytes && format!("{:x}", digest.finalize()) == artifact.sha256,
        "privacy_artifact_checksum_mismatch"
    );
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}
pub struct VerifiedModel {
    pub spec: ModelSpec,
    pub bytes: Vec<u8>,
}
pub struct ModelSet {
    pub manifest: ModelManifest,
    pub models: Vec<VerifiedModel>,
    pub runtime: File,
    #[cfg(not(target_os = "linux"))]
    root: PathBuf,
    identity: String,
}
impl ModelSet {
    pub fn verify_files(root: &Path, manifest: &ModelManifest) -> Result<String> {
        manifest.validate()?;
        verify(root, &manifest.runtime)?;
        for model in &manifest.models {
            verify(root, &model.artifact)?;
        }
        manifest.identity()
    }
    pub fn load_verified(root: &Path, manifest: &ModelManifest) -> Result<Self> {
        manifest.validate()?;
        let runtime = verify(root, &manifest.runtime)?;
        let mut models = Vec::new();
        for spec in &manifest.models {
            let mut file = verify(root, &spec.artifact)?;
            let mut bytes = Vec::new();
            (&mut file).take(MODEL_LIMIT + 1).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 == spec.artifact.bytes
                    && format!("{:x}", Sha256::digest(&bytes)) == spec.artifact.sha256,
                "privacy_artifact_checksum_mismatch"
            );
            models.push(VerifiedModel {
                spec: spec.clone(),
                bytes,
            });
        }
        Ok(Self {
            manifest: manifest.clone(),
            models,
            runtime,
            #[cfg(not(target_os = "linux"))]
            root: root.into(),
            identity: manifest.identity()?,
        })
    }
    pub fn identity(&self) -> &str {
        &self.identity
    }
    pub fn qualified(&self) -> bool {
        self.manifest.qualified
    }
    pub fn runtime_path(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/proc/self/fd/{}", self.runtime.as_raw_fd()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.root.join(&self.manifest.runtime.path)
        }
    }
}
