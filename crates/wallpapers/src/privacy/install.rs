//! Optional verified data/native-runtime installation. No Python is installed.
use super::{
    models::{Artifact, ModelManifest, ModelSet},
    store::DecisionKey,
};
use crate::storage::{Store, atomic_bytes, atomic_json, private_dir, validate_private_parents};
use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Download {
    pub path: PathBuf,
    pub url: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeArchive {
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub member: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selected {
    pub schema_version: u32,
    pub id: String,
    pub platform: String,
    pub runtime_manifest: ModelManifest,
    pub downloads: Vec<Download>,
    pub runtime_archive: RuntimeArchive,
    pub artifact_manifest: Value,
    pub reference_evaluation_sha256: String,
}
impl Selected {
    pub fn reference_qualified(&self) -> bool {
        format!(
            "{:x}",
            Sha256::digest(include_bytes!(
                "../../../../third_party/privacy-reference-evaluation.json"
            ))
        ) == self.reference_evaluation_sha256
            && serde_json::from_slice::<Value>(include_bytes!(
                "../../../../third_party/privacy-reference-evaluation.json"
            ))
            .is_ok_and(|v| v["qualified"] == true && v["false_clear"] == 0 && v["errors"] == 0)
    }
}
pub fn selected() -> Result<Selected> {
    let set: Selected =
        serde_json::from_str(include_str!("../../../../third_party/privacy-models.json"))?;
    ensure!(
        set.schema_version == 1
            && set.id == "meeting-v2"
            && set.platform == "linux-x86_64"
            && set.reference_qualified(),
        "privacy_model_manifest_invalid"
    );
    set.runtime_manifest.validate()?;
    ensure!(
        set.runtime_manifest.qualified && set.downloads.len() == set.runtime_manifest.models.len(),
        "privacy_model_manifest_invalid"
    );
    for spec in &set.runtime_manifest.models {
        let matches: Vec<_> = set
            .downloads
            .iter()
            .filter(|d| d.path == spec.artifact.path)
            .collect();
        ensure!(matches.len() == 1, "privacy_model_manifest_invalid");
        let url = reqwest::Url::parse(&matches[0].url)?;
        ensure!(
            url.scheme() == "https"
                && url.host_str() == Some("huggingface.co")
                && url.path().contains("/resolve/"),
            "privacy_model_origin_invalid"
        );
    }
    let url = reqwest::Url::parse(&set.runtime_archive.url)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("files.pythonhosted.org")
            && set.runtime_archive.bytes <= 64 * 1024 * 1024
            && set.runtime_archive.member == "onnxruntime/capi/libonnxruntime.so.1.31.1",
        "privacy_runtime_origin_invalid"
    );
    Ok(set)
}
pub fn download_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error("privacy_download_redirect_limit");
            }
            let target = attempt.url();
            let local = |url: &reqwest::Url| {
                url.scheme() == "http"
                    && matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))
            };
            if target.scheme() == "https"
                || (local(target) && attempt.previous().first().is_some_and(local))
            {
                attempt.follow()
            } else {
                attempt.error("privacy_download_origin_invalid")
            }
        }))
        .build()?)
}
pub async fn download_verified(
    client: &reqwest::Client,
    url: &str,
    target: &Path,
    bytes: u64,
    sha256: &str,
    stop: CancellationToken,
) -> Result<()> {
    ensure!(
        bytes > 0 && bytes <= 512 * 1024 * 1024 && sha256.len() == 64,
        "privacy_download_limit"
    );
    let parsed = reqwest::Url::parse(url)?;
    ensure!(
        parsed.scheme() == "https"
            || (parsed.scheme() == "http"
                && matches!(parsed.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))),
        "privacy_download_origin_invalid"
    );
    let parent = target.parent().context("privacy_download_parent")?;
    private_dir(parent)?;
    let temporary = parent.join(format!(".download-{}", uuid::Uuid::new_v4()));
    let result=async {
        let response=tokio::select!{_=stop.cancelled()=>anyhow::bail!("privacy_install_cancelled"),r=client.get(url).send()=>r?};
        ensure!(response.status().is_success(),"privacy_download_failed");
        ensure!(response.content_length().is_none_or(|n|n==bytes),"privacy_download_size_mismatch");
        let file=OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;let mut file=tokio::fs::File::from_std(file);let mut digest=Sha256::new();let mut count=0_u64;let mut stream=response.bytes_stream();
        loop {let chunk=tokio::select!{_=stop.cancelled()=>anyhow::bail!("privacy_install_cancelled"),v=stream.next()=>v};let Some(chunk)=chunk else{break};let chunk=chunk?;count+=chunk.len() as u64;ensure!(count<=bytes,"privacy_download_size_mismatch");digest.update(&chunk);file.write_all(&chunk).await?;}
        ensure!(count==bytes && format!("{:x}",digest.finalize())==sha256,"privacy_download_checksum_mismatch");
        file.sync_all().await?;drop(file);ensure!(!stop.is_cancelled(),"privacy_install_cancelled");
        tokio::fs::rename(&temporary,target).await?;std::fs::File::open(parent)?.sync_all()?;Ok(())
    }.await;
    let _ = std::fs::remove_file(&temporary);
    result
}
pub fn extract_runtime(
    archive: &Path,
    member: &str,
    target: &Path,
    artifact: &Artifact,
) -> Result<()> {
    ensure!(
        artifact.bytes > 0 && artifact.bytes <= 128 * 1024 * 1024,
        "privacy_runtime_limit"
    );
    validate_private_parents(archive)?;
    let tool = if Path::new("/usr/bin/bsdtar").exists() {
        "/usr/bin/bsdtar"
    } else {
        "/usr/bin/tar"
    };
    let mut child = std::process::Command::new(tool)
        .arg("-xOf")
        .arg(archive)
        .arg("--")
        .arg(member)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("privacy_runtime_extract_unavailable")?;
    let result = (|| -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .context("privacy_runtime_extract_failed")?
            .take(artifact.bytes + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 == artifact.bytes,
            "privacy_runtime_size_mismatch"
        );
        ensure!(child.wait()?.success(), "privacy_runtime_extract_failed");
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)) == artifact.sha256,
            "privacy_runtime_checksum_mismatch"
        );
        Ok(bytes)
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    atomic_bytes(target, &result?)
}
pub type Progress = Arc<dyn Fn(Value) + Send + Sync>;
pub async fn install(
    store: Store,
    stop: CancellationToken,
    progress: Progress,
) -> Result<DecisionKey> {
    ensure!(
        cfg!(all(target_os = "linux", target_arch = "x86_64")),
        "privacy_runtime_architecture_unsupported"
    );
    let set = selected()?;
    let id = set.runtime_manifest.identity()?;
    let parent = store.paths.data.join("privacy/models");
    private_dir(&parent)?;
    let destination = parent.join(&id);
    let key = DecisionKey {
        modelset_sha256: id.clone(),
        preprocessing_version: set.runtime_manifest.preprocessing_version.clone(),
        policy_version: set.runtime_manifest.policy_version.clone(),
    };
    let activate = |manifest: &ModelManifest| -> Result<()> {
        atomic_json(
            &parent.join("active.json"),
            &serde_json::to_value(manifest)?,
        )?;
        store.set_meta("privacy-policy-v1", &serde_json::to_value(&key)?)?;
        Ok(())
    };
    if ModelSet::verify_files(&destination, &set.runtime_manifest).is_ok() {
        activate(&set.runtime_manifest)?;
        progress(json!({"state":"ready","completed":4,"total":4}));
        return Ok(key);
    }
    let stage = parent.join(format!(".stage-{}", uuid::Uuid::new_v4()));
    private_dir(&stage)?;
    let result = async {
        let client = download_client()?;
        progress(json!({"state":"downloading","item":"native runtime","completed":0,"total":4}));
        let archive = stage.join("runtime.whl");
        download_verified(
            &client,
            &set.runtime_archive.url,
            &archive,
            set.runtime_archive.bytes,
            &set.runtime_archive.sha256,
            stop.clone(),
        )
        .await?;
        let a = archive.clone();
        let target = stage.join(&set.runtime_manifest.runtime.path);
        let spec = set.runtime_manifest.runtime.clone();
        let member = set.runtime_archive.member.clone();
        tokio::task::spawn_blocking(move || extract_runtime(&a, &member, &target, &spec)).await??;
        std::fs::remove_file(&archive)?;
        ensure!(!stop.is_cancelled(), "privacy_install_cancelled");
        for (i, model) in set.runtime_manifest.models.iter().enumerate() {
            progress(
                json!({"state":"downloading","item":model.artifact.id,"completed":i+1,"total":4}),
            );
            let source = set
                .downloads
                .iter()
                .find(|d| d.path == model.artifact.path)
                .context("privacy_model_manifest_invalid")?;
            download_verified(
                &client,
                &source.url,
                &stage.join(&model.artifact.path),
                model.artifact.bytes,
                &model.artifact.sha256,
                stop.clone(),
            )
            .await?;
        }
        let verify = stage.clone();
        let manifest = set.runtime_manifest.clone();
        tokio::task::spawn_blocking(move || ModelSet::verify_files(&verify, &manifest)).await??;
        ensure!(!stop.is_cancelled(), "privacy_install_cancelled");
        for (name, bytes) in [
            (
                "onnxruntime-MIT.txt",
                include_bytes!("../../../../third_party/onnxruntime-MIT.txt").as_slice(),
            ),
            (
                "onnxruntime-ThirdPartyNotices.txt",
                include_bytes!("../../../../third_party/onnxruntime-ThirdPartyNotices.txt")
                    .as_slice(),
            ),
            (
                "privacy-models-Apache-2.0.txt",
                include_bytes!("../../../../third_party/privacy-models-Apache-2.0.txt").as_slice(),
            ),
            (
                "privacy-model-NOTICE.md",
                include_bytes!("../../../../third_party/privacy-model-NOTICE.md").as_slice(),
            ),
        ] {
            atomic_bytes(&stage.join("licenses").join(name), bytes)?;
        }
        if destination.exists() {
            validate_private_parents(&destination.join("entry"))?;
            ensure!(
                std::fs::symlink_metadata(&destination)?.is_dir(),
                "privacy_model_path_invalid"
            );
            std::fs::rename(
                &destination,
                parent.join(format!(".quarantine-{}", uuid::Uuid::new_v4())),
            )?;
        }
        std::fs::rename(&stage, &destination)?;
        std::fs::File::open(&parent)?.sync_all()?;
        activate(&set.runtime_manifest)?;
        progress(json!({"state":"ready","completed":4,"total":4}));
        Ok(key.clone())
    }
    .await;
    if stage.exists() {
        let _ = std::fs::remove_dir_all(&stage);
    }
    result
}
