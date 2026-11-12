//! A bounded, short-lived process owns inference. The UI/idle daemon owns none.
use super::{
    models::ModelManifest,
    policy::{PrivacyChoice, ScreeningState, Signals, decide},
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Semaphore,
};
use tokio_util::sync::CancellationToken;
const INPUT_LIMIT: usize = 2 * 1024 * 1024;
const OUTPUT_LIMIT: usize = 16 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewJob {
    pub png_base64: String,
    pub sha256: String,
}
impl PreviewJob {
    pub fn from_png(png: Vec<u8>) -> Self {
        Self {
            sha256: format!("{:x}", Sha256::digest(&png)),
            png_base64: STANDARD.encode(png),
        }
    }
    pub fn pixels(&self) -> Result<Vec<u8>> {
        ensure!(
            self.png_base64.len() <= INPUT_LIMIT / 2,
            "privacy_preview_limit"
        );
        let bytes = STANDARD
            .decode(&self.png_base64)
            .context("invalid_privacy_preview")?;
        ensure!(
            !bytes.is_empty() && format!("{:x}", Sha256::digest(&bytes)) == self.sha256,
            "invalid_privacy_preview"
        );
        Ok(bytes)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerInput {
    pub schema_version: u32,
    pub request_id: String,
    pub root: PathBuf,
    pub manifest: ModelManifest,
    pub preview: PreviewJob,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreeningOutput {
    pub schema_version: u32,
    pub request_id: String,
    pub modelset_sha256: String,
    pub preview_sha256: String,
    pub qualified: bool,
    pub signals: Signals,
}
impl ScreeningOutput {
    pub fn state(&self) -> ScreeningState {
        if self.qualified {
            decide(&self.signals, PrivacyChoice::Auto, false)
        } else {
            ScreeningState::Uncertain
        }
    }
}
#[derive(Clone)]
pub struct ScreeningWorker {
    executable: PathBuf,
    root: PathBuf,
    manifest: ModelManifest,
    timeout: Duration,
    inflight: Arc<Mutex<HashSet<String>>>,
    gate: Arc<Semaphore>,
}
struct Pending {
    key: String,
    inflight: Arc<Mutex<HashSet<String>>>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Ok(mut set) = self.inflight.lock() {
            set.remove(&self.key);
        }
    }
}
impl ScreeningWorker {
    pub fn new(
        executable: PathBuf,
        root: PathBuf,
        manifest: ModelManifest,
        timeout: Duration,
    ) -> Self {
        Self {
            executable,
            root,
            manifest,
            timeout,
            inflight: Arc::new(Mutex::new(HashSet::new())),
            gate: Arc::new(Semaphore::new(1)),
        }
    }
    pub async fn screen(
        &self,
        preview: PreviewJob,
        stop: CancellationToken,
    ) -> Result<ScreeningOutput> {
        preview.pixels()?;
        let key = format!("{}:{}", self.manifest.identity()?, preview.sha256);
        ensure!(
            self.inflight
                .lock()
                .map_err(|_| anyhow::anyhow!("privacy_worker_lock"))?
                .insert(key.clone()),
            "duplicate_privacy_job"
        );
        let pending = Pending {
            key,
            inflight: self.inflight.clone(),
        };
        let token = stop.child_token();
        let guard = token.clone().drop_guard();
        let worker = self.clone();
        // This task retains ownership if the caller drops its future. The drop
        // guard cancels it, and the task still kills/waits the child and unlocks.
        let task = tokio::spawn(async move {
            let _pending = pending;
            worker.run(preview, token).await
        });
        let result = task.await.context("privacy_worker_failed")?;
        drop(guard);
        result
    }
    async fn run(&self, preview: PreviewJob, stop: CancellationToken) -> Result<ScreeningOutput> {
        let _permit = tokio::select! {_=stop.cancelled()=>anyhow::bail!("privacy_screening_cancelled"),p=self.gate.acquire()=>p.context("privacy_worker_unavailable")?};
        ensure!(!stop.is_cancelled(), "privacy_screening_cancelled");
        let request_id = uuid::Uuid::new_v4().to_string();
        let preview_sha256 = preview.sha256.clone();
        let modelset_sha256 = self.manifest.identity()?;
        let input = WorkerInput {
            schema_version: 1,
            request_id: request_id.clone(),
            root: self.root.clone(),
            manifest: self.manifest.clone(),
            preview,
        };
        let mut bytes = serde_json::to_vec(&input)?;
        bytes.push(b'\n');
        ensure!(bytes.len() <= INPUT_LIMIT, "privacy_worker_input_limit");
        let mut child = tokio::process::Command::new(&self.executable)
            .arg("privacy-worker")
            .env("OMP_NUM_THREADS", "1")
            .env("OPENBLAS_NUM_THREADS", "1")
            .env("VECLIB_MAXIMUM_THREADS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("privacy_worker_unavailable")?;
        let mut stdin = child.stdin.take().context("privacy_worker_stdin")?;
        let stdout = child.stdout.take().context("privacy_worker_stdout")?;
        let result: Result<Vec<u8>> = {
            let work = async {
                stdin.write_all(&bytes).await?;
                drop(stdin);
                let mut output = Vec::new();
                stdout
                    .take(OUTPUT_LIMIT as u64 + 1)
                    .read_to_end(&mut output)
                    .await?;
                ensure!(output.len() <= OUTPUT_LIMIT, "privacy_worker_output_limit");
                ensure!(child.wait().await?.success(), "privacy_worker_failed");
                Ok(output)
            };
            tokio::select! {_=stop.cancelled()=>Err(anyhow::anyhow!("privacy_screening_cancelled")),_=tokio::time::sleep(self.timeout)=>Err(anyhow::anyhow!("privacy_screening_timeout")),result=work=>result}
        };
        if result.is_err() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        let output: ScreeningOutput =
            serde_json::from_slice(&result?).context("invalid_privacy_worker_output")?;
        ensure!(
            output.schema_version == 1
                && output.request_id == request_id
                && output.preview_sha256 == preview_sha256
                && output.modelset_sha256 == modelset_sha256
                && output.qualified == self.manifest.qualified,
            "invalid_privacy_worker_output"
        );
        Ok(output)
    }
}
