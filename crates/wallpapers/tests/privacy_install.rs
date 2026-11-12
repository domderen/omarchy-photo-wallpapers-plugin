use omarchy_photo_wallpapers::privacy::install::{download_verified, extract_runtime, selected};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

#[test]
fn selected_artifacts_bind_the_reference_policy_and_ship_no_python_runtime() {
    let set = selected().unwrap();
    assert!(set.runtime_manifest.qualified);
    assert_eq!(set.runtime_manifest.models.len(), 3);
    assert_eq!(set.runtime_manifest.runtime_version, "1.31.1");
    assert_eq!(
        set.runtime_archive.member,
        "onnxruntime/capi/libonnxruntime.so.1.31.1"
    );
    assert!(
        set.downloads
            .iter()
            .all(|d| d.url.starts_with("https://huggingface.co/") && d.url.contains("/resolve/"))
    );
    assert!(
        set.runtime_archive
            .url
            .starts_with("https://files.pythonhosted.org/")
    );
    set.runtime_manifest.validate().unwrap();
    assert!(set.reference_qualified());
}
#[tokio::test]
async fn download_tampering_and_cancellation_cannot_publish_artifacts() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    let server = MockServer::start().await;
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("model.onnx");
    Mock::given(method("GET"))
        .and(path("/bad"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"wrong".to_vec()))
        .mount(&server)
        .await;
    let hash = format!("{:x}", Sha256::digest(b"right"));
    assert!(
        download_verified(
            &reqwest::Client::new(),
            &format!("{}/bad", server.uri()),
            &target,
            5,
            &hash,
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert!(!target.exists());
    let token = CancellationToken::new();
    token.cancel();
    assert!(
        download_verified(
            &reqwest::Client::new(),
            &format!("{}/bad", server.uri()),
            &target,
            5,
            &hash,
            token
        )
        .await
        .is_err()
    );
    assert!(!target.exists());
}
#[test]
fn extracting_a_native_runtime_requires_the_exact_member_and_hash() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("synthetic.tar");
    std::fs::write(root.path().join("runtime.so"), b"synthetic-runtime").unwrap();
    let status = std::process::Command::new("tar")
        .args(["-cf"])
        .arg(&archive)
        .arg("-C")
        .arg(root.path())
        .arg("runtime.so")
        .status()
        .unwrap();
    assert!(status.success());
    let artifact = omarchy_photo_wallpapers::privacy::models::Artifact {
        id: "runtime".into(),
        path: "native.so".into(),
        format: "native".into(),
        sha256: format!("{:x}", Sha256::digest(b"synthetic-runtime")),
        bytes: 17,
        license: "MIT".into(),
        license_source: "https://example.invalid/LICENSE".into(),
    };
    assert!(
        extract_runtime(
            &archive,
            "missing.so",
            &root.path().join("bad.so"),
            &artifact
        )
        .is_err()
    );
    let mut wrong = artifact.clone();
    wrong.sha256 = "a".repeat(64);
    assert!(extract_runtime(&archive, "runtime.so", &root.path().join("bad.so"), &wrong).is_err());
    assert!(!root.path().join("bad.so").exists());
    extract_runtime(
        &archive,
        "runtime.so",
        &root.path().join("native.so"),
        &artifact,
    )
    .unwrap();
    assert_eq!(
        std::fs::read(root.path().join("native.so")).unwrap(),
        b"synthetic-runtime"
    );
}

#[tokio::test]
async fn verified_download_handles_success_size_limits_and_unsafe_redirects() {
    use omarchy_photo_wallpapers::privacy::install::download_client;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    let server = MockServer::start().await;
    for (route, reply) in [
        (
            "/valid",
            ResponseTemplate::new(200).set_body_bytes(b"right".to_vec()),
        ),
        (
            "/oversized",
            ResponseTemplate::new(200).set_body_bytes(b"right!".to_vec()),
        ),
        (
            "/redirect",
            ResponseTemplate::new(302).insert_header("Location", "http://example.invalid/model"),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(reply)
            .mount(&server)
            .await;
    }
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("model.onnx");
    let hash = format!("{:x}", Sha256::digest(b"right"));
    let client = download_client().unwrap();
    for route in ["oversized", "redirect"] {
        assert!(
            download_verified(
                &client,
                &format!("{}/{route}", server.uri()),
                &target,
                5,
                &hash,
                CancellationToken::new()
            )
            .await
            .is_err()
        );
        assert!(!target.exists());
    }
    download_verified(
        &client,
        &format!("{}/valid", server.uri()),
        &target,
        5,
        &hash,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"right");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}
