use omarchy_photo_wallpapers::privacy::{
    models::ModelManifest,
    worker::{PreviewJob, ScreeningWorker},
};
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

fn fake(script: &str, timeout: Duration) -> (tempfile::TempDir, Arc<ScreeningWorker>, String) {
    let root = tempfile::tempdir().unwrap();
    let exe = root.path().join("fake-worker");
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'/pid\nread request\n{script}\n",
            root.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
    let manifest:ModelManifest=serde_json::from_value(json!({"schema_version":1,"runtime_version":"1.31.1","runtime":{"id":"runtime","path":"unused","format":"native","sha256":"a".repeat(64),"bytes":1,"license":"MIT","license_source":"https://example.invalid/LICENSE"},"models":[],"preprocessing_version":"v1","policy_version":"v1","qualified":false})).unwrap();
    let identity = manifest.identity().unwrap();
    let worker = Arc::new(ScreeningWorker::new(
        exe,
        root.path().into(),
        manifest,
        timeout,
    ));
    (root, worker, identity)
}
fn preview() -> PreviewJob {
    PreviewJob::from_png(b"synthetic-preview".to_vec())
}
fn assert_reaped(root: &std::path::Path) {
    let pid = std::fs::read_to_string(root.join("pid")).unwrap();
    assert!(
        !std::process::Command::new("kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}
#[tokio::test]
async fn malformed_or_oversized_output_cannot_authorize_an_image() {
    for script in ["printf '{}\\n'", "head -c 20000 /dev/zero"] {
        let (root, worker, _) = fake(script, Duration::from_secs(10));
        let result = worker.screen(preview(), CancellationToken::new()).await;
        assert!(result.is_err());
        let error = result.as_ref().unwrap_err().to_string();
        assert!(error.contains("output"), "wrong failure: {error}");
        assert!(
            root.path().join("pid").exists(),
            "worker never executed for {script}: {result:?}"
        );
        assert_reaped(root.path());
    }
}
#[tokio::test]
async fn timeout_and_cancellation_kill_and_reap_the_single_worker() {
    let (root, worker, _) = fake("exec sleep 30", Duration::from_secs(3));
    assert!(
        worker
            .screen(preview(), CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("timeout")
    );
    assert_reaped(root.path());
    let (root, worker, _) = fake("exec sleep 30", Duration::from_secs(10));
    let stop = CancellationToken::new();
    let token = stop.clone();
    let task = tokio::spawn(async move { worker.screen(preview(), token).await });
    for _ in 0..500 {
        if root.path().join("pid").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        root.path().join("pid").exists(),
        "fixture did not execute before cancellation"
    );
    stop.cancel();
    assert!(
        task.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert_reaped(root.path());
}
#[tokio::test]
async fn duplicate_jobs_do_not_start_duplicate_native_sessions() {
    let (root, worker, _) = fake("exec sleep 30", Duration::from_secs(10));
    let stop = CancellationToken::new();
    let token = stop.clone();
    let a = worker.clone();
    let task = tokio::spawn(async move { a.screen(preview(), token).await });
    for _ in 0..500 {
        if root.path().join("pid").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        root.path().join("pid").exists(),
        "fixture did not execute before duplicate check"
    );
    assert!(
        worker
            .screen(preview(), CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    assert!(
        root.path().join("pid").exists(),
        "fixture did not execute before cancellation"
    );
    stop.cancel();
    let _ = task.await;
    assert_reaped(root.path());
}

#[tokio::test]
async fn valid_unqualified_results_are_not_automatic_admission_and_worker_exits() {
    let (root, worker, identity) = fake("exit 0", Duration::from_secs(10));
    let job = preview();
    let output=serde_json::json!({"schema_version":1,"request_id":"REQUEST","modelset_sha256":identity,"preview_sha256":job.sha256,"qualified":false,"signals":{"nudity":"clear","underwear":"clear","bathing":"clear","sleepwear":"clear","swimwear":"clear"}}).to_string();
    let script = format!(
        "#!/bin/sh\nprintf '%s' \"$$\" > '{}/pid'\nread request\nid=$(printf '%s' \"$request\" | sed -n 's/.*\"request_id\":\"\\([^\"]*\\)\".*/\\1/p')\nprintf '%s' '{}' | sed \"s/REQUEST/$id/\"\n",
        root.path().display(),
        output
    );
    std::fs::write(root.path().join("fake-worker"), script).unwrap();
    let result = worker.screen(job, CancellationToken::new()).await.unwrap();
    assert_eq!(
        result.state(),
        omarchy_photo_wallpapers::privacy::policy::ScreeningState::Uncertain
    );
    assert_reaped(root.path());
}

#[tokio::test]
async fn dropping_the_callers_future_still_cancels_and_reaps_the_child() {
    let (root, worker, _) = fake("exec sleep 30", Duration::from_secs(10));
    let task =
        tokio::spawn(async move { worker.screen(preview(), CancellationToken::new()).await });
    for _ in 0..200 {
        if root.path().join("pid").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(root.path().join("pid").exists());
    task.abort();
    let _ = task.await;
    let pid = std::fs::read_to_string(root.path().join("pid")).unwrap();
    for _ in 0..200 {
        if !std::process::Command::new("kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_reaped(root.path());
}
