use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
    time::Duration,
};
fn cli(root: &std::path::Path, args: &[&str], input: Option<Value>) -> (bool, Value) {
    let mut process = Command::new(env!("CARGO_BIN_EXE_omarchy-photo-wallpapers"))
        .arg("--profile")
        .arg(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(value) = input {
        process
            .stdin
            .take()
            .unwrap()
            .write_all(value.to_string().as_bytes())
            .unwrap();
    }
    let output = process.wait_with_output().unwrap();
    let body = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("CLI returned invalid JSON"));
    (output.status.success(), body)
}
struct Daemon(std::process::Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn rust_cli_defaults_revisions_secret_stdin_and_single_instance() {
    let root = tempfile::tempdir().unwrap();
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_omarchy-photo-wallpapers"))
            .arg("--profile")
            .arg(root.path())
            .args(["daemon", "--no-network"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    // Cold executable validation/startup on macOS can exceed one second. Wait
    // for readiness; the command responsiveness checks remain independent.
    for _ in 0..1000 {
        if root.path().join("run/engine.sock").exists() {
            break;
        }
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "daemon exited before binding its socket"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let (ok, status) = cli(root.path(), &["status", "--json"], None);
    assert!(
        ok,
        "status failed: {status}; socket bytes={}",
        root.path().join("run/engine.sock").as_os_str().len()
    );
    assert_eq!(status["backend"], "rust");
    assert_eq!(status["interval_seconds"], 3600);
    let (ok, health) = cli(root.path(), &["health"], None);
    assert!(ok);
    assert_eq!(
        health,
        json!({"daemon_running":true,"backend_version":env!("CARGO_PKG_VERSION"),"protocol_version":1})
    );
    assert!(!root.path().join("config/helper-v2.json").exists());
    let (ok, saved) = cli(
        root.path(),
        &[
            "config",
            "apply",
            "--expected-revision",
            "0",
            "--stdin-json",
            "--json",
        ],
        Some(json!({"interval_seconds":900})),
    );
    assert!(ok);
    assert_eq!(saved["interval_seconds"], 900);
    let (ok, conflict) = cli(
        root.path(),
        &[
            "config",
            "apply",
            "--expected-revision",
            "0",
            "--stdin-json",
        ],
        Some(json!({"interval_seconds":60})),
    );
    assert!(!ok);
    assert_eq!(conflict["error"], "settings_conflict");
    let (ok, result) = cli(root.path(), &["auth", "login"], None);
    assert!(!ok);
    assert_eq!(result["error"], "stdin_json_required");
    let (ok, result) = cli(
        root.path(),
        &["auth", "login", "--stdin-json"],
        Some(
            json!({"username":"synthetic@example.invalid","password":"synthetic-password-never-echo"}),
        ),
    );
    assert!(!ok);
    assert!(!result.to_string().contains("synthetic-password"));
    let (ok, result) = cli(root.path(), &["daemon", "--no-network"], None);
    assert!(!ok);
    assert_eq!(result["error"], "daemon_already_running");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(root.path().join("run/engine.sock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    Command::new("kill")
        .args(["-TERM", &daemon.0.id().to_string()])
        .status()
        .unwrap();
    assert!(daemon.0.wait().unwrap().success());
}

#[test]
fn meeting_cli_exposes_explicit_idempotent_on_off_and_status_operations() {
    let binary = env!("CARGO_BIN_EXE_omarchy-photo-wallpapers");
    let help = std::process::Command::new(binary)
        .args(["meeting", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("on") && text.contains("off") && text.contains("status"));
    let bad = std::process::Command::new(binary)
        .args(["meeting", "toggle"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert!(
        String::from_utf8(bad.stderr)
            .unwrap()
            .contains("invalid value")
    );
}
