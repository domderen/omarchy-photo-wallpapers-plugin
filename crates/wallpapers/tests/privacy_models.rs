use omarchy_photo_wallpapers::privacy::models::{ModelManifest, ModelSet};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn manifest() -> Value {
    let artifact = |id: &str, path: &str, format: &str| json!({"id":id,"path":path,"format":format,"sha256":format!("{:x}",Sha256::digest(b"synthetic")),"bytes":9,"license":"MIT","license_source":"https://example.invalid/LICENSE"});
    json!({"schema_version":1,"runtime_version":"1.31.1","runtime":artifact("runtime","runtime.bin","native"),"models":[{"artifact":artifact("context","model.onnx","onnx"),"role":"context","output":"pooler_output","mean":[0.5,0.5,0.5],"std":[0.5,0.5,0.5],"embeddings":{},"clear_margin":0.025,"blocked_margin":0.01}],"preprocessing_version":"full-frame-v1","policy_version":"meeting-v1","qualified":false})
}
fn fixture() -> (tempfile::TempDir, Value) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("runtime.bin"), b"synthetic").unwrap();
    std::fs::write(root.path().join("model.onnx"), b"synthetic").unwrap();
    (root, manifest())
}
#[test]
fn immutable_artifacts_are_verified_before_any_native_load() {
    let (root, v) = fixture();
    let m: ModelManifest = serde_json::from_value(v).unwrap();
    let loaded = ModelSet::load_verified(root.path(), &m).unwrap();
    assert!(!loaded.qualified());
    assert_eq!(loaded.identity().len(), 64);
    std::fs::write(root.path().join("model.onnx"), b"tampering").unwrap();
    assert!(ModelSet::load_verified(root.path(), &m).is_err());
}
#[test]
fn symlinks_ancestors_fifo_traversal_licenses_and_versions_fail_closed() {
    for case in [
        "symlink",
        "ancestor",
        "fifo",
        "traversal",
        "license",
        "version",
        "oversized",
        "nonfinite",
    ] {
        let (root, mut v) = fixture();
        match case {
            "symlink" => {
                std::fs::remove_file(root.path().join("model.onnx")).unwrap();
                std::os::unix::fs::symlink(
                    root.path().join("runtime.bin"),
                    root.path().join("model.onnx"),
                )
                .unwrap();
            }
            "ancestor" => {
                std::os::unix::fs::symlink(root.path(), root.path().join("linked")).unwrap();
                v["models"][0]["artifact"]["path"] = json!("linked/model.onnx");
            }
            "fifo" => {
                std::fs::remove_file(root.path().join("model.onnx")).unwrap();
                assert!(
                    std::process::Command::new("mkfifo")
                        .arg(root.path().join("model.onnx"))
                        .status()
                        .unwrap()
                        .success()
                );
            }
            "traversal" => v["models"][0]["artifact"]["path"] = json!("../model.onnx"),
            "license" => v["models"][0]["artifact"]["license"] = json!("AGPL-3.0"),
            "version" => v["runtime_version"] = json!("1.20.0"),
            "oversized" => v["models"][0]["artifact"]["bytes"] = json!(1024_u64 * 1024 * 1024),
            _ => v["models"][0]["std"] = json!([0, 0, 0]),
        }
        let m: ModelManifest = serde_json::from_value(v).unwrap();
        assert!(
            ModelSet::load_verified(root.path(), &m).is_err(),
            "case={case}"
        );
    }
}
