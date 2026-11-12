use crate::{imaging::Prepared, storage::Paths};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::{fs::File, time::Duration};
#[async_trait]
pub trait Platform: Send + Sync {
    async fn geometry(&self) -> Result<(u32, u32)>;
    async fn apply(&self, prepared: &Prepared) -> Result<()>;
    async fn output_names(&self) -> Result<Vec<String>> {
        Ok(vec!["default".into()])
    }
    async fn apply_instant(&self, prepared: &Prepared) -> Result<()> {
        self.apply(prepared).await
    }
}
pub struct Omarchy;
#[async_trait]
impl Platform for Omarchy {
    async fn output_names(&self) -> Result<Vec<String>> {
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new("hyprctl")
                .args(["-j", "monitors"])
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        ensure!(result.status.success(), "display_geometry_unavailable");
        let rows: Vec<Value> = serde_json::from_slice(&result.stdout)?;
        let mut names: Vec<String> = rows
            .iter()
            .filter(|r| r["disabled"] != true)
            .filter_map(|r| r["name"].as_str())
            .filter(|name| !name.is_empty() && name.len() <= 128)
            .map(str::to_owned)
            .collect();
        names.sort();
        names.dedup();
        ensure!(!names.is_empty(), "no_active_outputs");
        Ok(names)
    }
    async fn apply_instant(&self, prepared: &Prepared) -> Result<()> {
        ensure!(
            prepared.render_path.is_absolute() && prepared.render_path.is_file(),
            "render_unavailable"
        );
        let path = prepared.render_path.canonicalize()?;
        let status = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new("omarchy-shell")
                .args(["-q", "background", "setInstant"])
                .arg(path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await??;
        ensure!(status.success(), "wallpaper_apply_failed");
        // Update Omarchy's persistent symlink after the instant IPC. The normal
        // setter then sees the same path and cannot start its 420ms old-image reveal.
        self.apply(prepared).await
    }
    async fn geometry(&self) -> Result<(u32, u32)> {
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new("hyprctl")
                .args(["-j", "monitors"])
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        ensure!(result.status.success(), "display_geometry_unavailable");
        let rows: Vec<Value> = serde_json::from_slice(&result.stdout)?;
        let mut sizes = Vec::new();
        for row in rows {
            if row["disabled"] == true {
                continue;
            }
            let mut w = row["width"].as_u64().context("invalid_display_geometry")? as u32;
            let mut h = row["height"].as_u64().context("invalid_display_geometry")? as u32;
            if row["transform"].as_u64().unwrap_or(0) % 2 == 1 {
                std::mem::swap(&mut w, &mut h);
            }
            sizes.push((w, h));
        }
        sizes
            .into_iter()
            .max_by_key(|(w, h)| *w as u64 * *h as u64)
            .context("no_active_outputs")
    }
    async fn apply(&self, prepared: &Prepared) -> Result<()> {
        ensure!(
            prepared.render_path.is_absolute() && prepared.render_path.is_file(),
            "render_unavailable"
        );
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new("omarchy-theme-bg-set")
                .arg(&prepared.render_path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await??;
        ensure!(result.success(), "wallpaper_apply_failed");
        Ok(())
    }
}
pub fn lock(paths: &Paths) -> Result<File> {
    crate::storage::private_dir(&paths.runtime)?;
    let file = crate::storage::private_file(
        &paths.runtime.join("engine.lock"),
        rustix::fs::OFlags::CREATE | rustix::fs::OFlags::RDWR,
    )?;
    fs2::FileExt::try_lock_exclusive(&file)
        .map_err(|_| anyhow::anyhow!("daemon_already_running"))?;
    Ok(file)
}
#[cfg(not(target_os = "linux"))]
pub async fn deadline(time: Option<DateTime<Utc>>) {
    if let Some(time) = time {
        let wait = (time - Utc::now()).to_std().unwrap_or_default();
        tokio::time::sleep(wait).await;
    } else {
        std::future::pending::<()>().await;
    }
}
#[cfg(target_os = "linux")]
pub async fn deadline(time: Option<DateTime<Utc>>) {
    use rustix::time::{
        Itimerspec, TimerfdClockId, TimerfdFlags, TimerfdTimerFlags, Timespec, timerfd_create,
        timerfd_settime,
    };
    let Some(time) = time else {
        std::future::pending::<()>().await;
        return;
    };
    if time <= Utc::now() {
        return;
    }
    let result = async {
        let fd = timerfd_create(
            TimerfdClockId::Realtime,
            TimerfdFlags::CLOEXEC | TimerfdFlags::NONBLOCK,
        )?;
        timerfd_settime(
            &fd,
            TimerfdTimerFlags::ABSTIME | TimerfdTimerFlags::CANCEL_ON_SET,
            &Itimerspec {
                it_interval: Timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                },
                it_value: Timespec {
                    tv_sec: time.timestamp(),
                    tv_nsec: time.timestamp_subsec_nanos().into(),
                },
            },
        )?;
        // SAFETY: OwnedFd exclusively owns this live timer descriptor; nothing can
        // replace or close it before AsyncFd drops its owned value.
        let timer = unsafe {
            tokio::io::unix::AsyncFd::register_with_interest(fd, tokio::io::Interest::READABLE)
        }?;
        let _ready = timer.readable().await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if result.is_err() {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    // CLOCK_REALTIME includes suspended time; clock adjustments also wake re-evaluation.
}

pub fn monitor_geometry(
    engine: crate::engine::EngineHandle,
    stop: tokio_util::sync::CancellationToken,
) {
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let Ok(signature) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") else {
            return;
        };
        let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
            return;
        };
        let socket = std::path::PathBuf::from(runtime)
            .join("hypr")
            .join(signature)
            .join(".socket2.sock");
        loop {
            if let Ok(stream) = tokio::net::UnixStream::connect(&socket).await {
                engine.geometry_changed().await;
                let mut lines = BufReader::new(stream).lines();
                loop {
                    let line = tokio::select! {_=stop.cancelled()=>return,r=lines.next_line()=>r};
                    match line {
                        Ok(Some(line)) => {
                            if line.starts_with("monitoradded")
                                || line.starts_with("monitorremoved")
                                || line.starts_with("configreloaded")
                            {
                                engine.geometry_changed().await;
                            }
                        }
                        _ => break,
                    }
                }
            }
            tokio::select! {_=stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(5))=>{}}
        }
    });
}

/// Serialize supported lifecycle changes with settings/mode requests. The OS
/// releases the lock on process exit, so a crashed installer cannot leave a flag.
pub fn lifecycle_lock(paths: &Paths, exclusive: bool) -> Result<File> {
    crate::storage::private_dir(&paths.data)?;
    let file = crate::storage::private_file(
        &paths.data.join("lifecycle.lock"),
        rustix::fs::OFlags::CREATE | rustix::fs::OFlags::RDWR,
    )?;
    let result = if exclusive {
        fs2::FileExt::try_lock_exclusive(&file)
    } else {
        fs2::FileExt::try_lock_shared(&file)
    };
    result.map_err(|_| anyhow::anyhow!("installation_in_progress"))?;
    Ok(file)
}
