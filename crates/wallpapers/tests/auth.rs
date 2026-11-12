use omarchy_photo_wallpapers::{
    icloud::{auth::*, srp::*},
    storage::Paths,
};
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, header, method, path},
};

#[test]
fn apple_srp_matches_independent_server_fixture() {
    let value: Value = serde_json::from_str(include_str!("fixtures/srp.json")).unwrap();
    let client = SrpClient::from_private([1; 32]).unwrap();
    assert_eq!(client.public_a(), value["public_a"]);
    let challenge: Challenge = serde_json::from_value(value["challenge"].clone()).unwrap();
    let proof = client
        .prove(
            value["username"].as_str().unwrap(),
            value["password"].as_str().unwrap(),
            &challenge,
        )
        .unwrap();
    assert_eq!(proof.m1, value["m1"]);
    assert_eq!(proof.m2, value["m2"]);
    for change in [
        json!({"iteration":0}),
        json!({"iteration":1000001}),
        json!({"protocol":"unknown"}),
        json!({"b":"AA=="}),
    ] {
        let mut invalid = value["challenge"].clone();
        for (k, v) in change.as_object().unwrap() {
            invalid[k] = v.clone();
        }
        assert!(
            client
                .prove(
                    "test",
                    "password",
                    &serde_json::from_value(invalid).unwrap()
                )
                .is_err()
        );
    }
}

async fn init(server: &MockServer) {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/srp.json")).unwrap();
    Mock::given(method("POST"))
        .and(path("/appleauth/auth/signin/init"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(&fixture["challenge"])
                .insert_header("scnt", "synthetic-scnt")
                .insert_header("X-Apple-ID-Session-Id", "synthetic-sid"),
        )
        .mount(server)
        .await;
}
fn account(server: &MockServer, pcs: bool) -> Value {
    json!({"dsInfo":{"dsid":"123","isWebAccessAllowed":true},"webservices":{"ckdatabasews":{"url":server.uri(),"status":"active","pcsRequired":pcs}}})
}

#[tokio::test]
async fn login_mfa_session_resume_and_no_password_persistence() {
    let server = MockServer::start().await;
    init(&server).await;
    Mock::given(method("POST"))
        .and(path("/appleauth/auth/signin/complete"))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(json!({"authType":"hsa2"}))
                .insert_header("X-Apple-Session-Token", "synthetic-session"),
        )
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/appleauth/auth/verify/trusteddevice/securitycode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/appleauth/auth/verify/trusteddevice/securitycode"))
        .and(body_partial_json(json!({"securityCode":{"code":"123456"}})))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/appleauth/auth/2sv/trust"))
        .respond_with(
            ResponseTemplate::new(204)
                .insert_header("X-Apple-TwoSV-Trust-Token", "synthetic-trust"),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/setup/ws/1/accountLogin"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(account(&server, false))
                .insert_header(
                    "Set-Cookie",
                    "sessioncookie=synthetic-cookie; Path=/; HttpOnly",
                ),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/setup/ws/1/validate"))
        .and(header("Cookie", "sessioncookie=synthetic-cookie"))
        .respond_with(ResponseTemplate::new(200).set_body_json(account(&server, false)))
        .mount(&server)
        .await;
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::under(temp.path());
    let mut client = ICloudClient::with_endpoints(&paths, Endpoints::local(&server.uri())).unwrap();
    let info = client
        .login("test@example.invalid", "do-not-store-this-password")
        .await
        .unwrap();
    assert_eq!(info.state, "mfa_required");
    assert_eq!(
        client.submit_mfa("123456").await.unwrap().state,
        "authenticated"
    );
    drop(client);
    let text = std::fs::read_to_string(paths.config.join("apple-session.json")).unwrap();
    assert!(!text.contains("do-not-store-this-password"));
    assert!(!text.contains("123456"));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(paths.config.join("apple-session.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let mut resumed =
        ICloudClient::with_endpoints(&paths, Endpoints::local(&server.uri())).unwrap();
    assert_eq!(resumed.resume().await.unwrap().state, "authenticated");
    let public = serde_json::to_string(&resumed.info()).unwrap();
    for secret in [
        "synthetic-session",
        "synthetic-trust",
        "synthetic-cookie",
        "synthetic-scnt",
    ] {
        assert!(!public.contains(secret));
    }
}

#[tokio::test]
async fn invalid_protocol_leaves_sign_in_in_a_recoverable_error_state() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/appleauth/auth/signin/init"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"salt":"YQ==","b":"Ag==","iteration":1000,"protocol":"unknown","c":"synthetic"}),
        ))
        .mount(&server)
        .await;
    let temp = tempfile::tempdir().unwrap();
    let mut client =
        ICloudClient::with_endpoints(&Paths::under(temp.path()), Endpoints::local(&server.uri()))
            .unwrap();
    assert!(
        client
            .login("test@example.invalid", "synthetic-password")
            .await
            .is_err()
    );
    assert_eq!(client.info().state, "error");
}

#[tokio::test]
async fn account_response_cannot_redirect_authenticated_requests_to_an_untrusted_host() {
    let server = MockServer::start().await;
    init(&server).await;
    Mock::given(method("POST"))
        .and(path("/appleauth/auth/signin/complete"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("X-Apple-Session-Token", "synthetic-session")
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST")).and(path("/setup/ws/1/accountLogin"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"dsInfo":{"dsid":"123"},"webservices":{"ckdatabasews":{"url":"https://attacker.invalid"}}}))).mount(&server).await;
    let temp = tempfile::tempdir().unwrap();
    let mut client =
        ICloudClient::with_endpoints(&Paths::under(temp.path()), Endpoints::local(&server.uri()))
            .unwrap();
    assert!(
        client
            .login("test@example.invalid", "synthetic-password")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn adp_requests_device_authorization_without_changing_security_settings() {
    let server = MockServer::start().await;
    init(&server).await;
    Mock::given(method("POST"))
        .and(path("/appleauth/auth/signin/complete"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("X-Apple-Session-Token", "synthetic-session")
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/setup/ws/1/accountLogin"))
        .respond_with(ResponseTemplate::new(200).set_body_json(account(&server, true)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/setup/ws/1/requestPCS"))
        .and(body_partial_json(
            json!({"appName":"photos","derivedFromUserAction":true}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status":"failure","isWebAccessAllowed":true})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let temp = tempfile::tempdir().unwrap();
    let mut client =
        ICloudClient::with_endpoints(&Paths::under(temp.path()), Endpoints::local(&server.uri()))
            .unwrap();
    assert_eq!(
        client
            .login("test@example.invalid", "synthetic-password")
            .await
            .unwrap()
            .state,
        "device_approval_required"
    );
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|r| !r.url.path().to_ascii_lowercase().contains("disable"))
    );
}

#[tokio::test]
async fn apple_errors_are_sanitized_and_challenges_are_not_retried_indefinitely() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/appleauth/auth/signin/init"))
        .respond_with(ResponseTemplate::new(429).set_body_string("server-private-token"))
        .expect(1)
        .mount(&server)
        .await;
    let temp = tempfile::tempdir().unwrap();
    let mut client =
        ICloudClient::with_endpoints(&Paths::under(temp.path()), Endpoints::local(&server.uri()))
            .unwrap();
    let error = client
        .login("test@example.invalid", "secret-password")
        .await
        .err()
        .unwrap();
    assert!(!format!("{error:#}").contains("server-private-token"));
    assert_eq!(client.info().error.as_deref(), Some("apple_rate_limited"));
}

#[tokio::test]
async fn escrow_and_trusted_device_approval_complete_without_security_changes() {
    let server = MockServer::start().await;
    init(&server).await;
    let fixture: Value = serde_json::from_str(include_str!("fixtures/srp.json")).unwrap();
    Mock::given(method("POST"))
        .and(path("/appleauth/auth/signin/complete"))
        .respond_with(
            ResponseTemplate::new(409)
                .insert_header("X-Apple-EDP", "true")
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/appleauth/auth/escrow/init"))
        .and(body_partial_json(json!({"accountName":""})))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fixture["challenge"]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/appleauth/auth/escrow/complete"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("X-Apple-Session-Token", "synthetic-escrow-session")
                .set_body_json(json!({})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/setup/ws/1/accountLogin"))
        .and(body_partial_json(
            json!({"dsWebAuthToken":"synthetic-escrow-session"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(account(&server, true)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/setup/ws/1/requestPCS"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status":"success"}))
                .insert_header(
                    "Set-Cookie",
                    "X-APPLE-WEBAUTH-PCS-Photos=synthetic-pcs; Path=/; HttpOnly",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let temp = tempfile::tempdir().unwrap();
    let mut client =
        ICloudClient::with_endpoints(&Paths::under(temp.path()), Endpoints::local(&server.uri()))
            .unwrap();
    assert_eq!(
        client
            .login("test@example.invalid", "synthetic-password")
            .await
            .unwrap()
            .state,
        "authenticated"
    );
    assert!(!client.info().adp_authorization_required);
    assert_eq!(client.logout().unwrap().state, "not_signed_in");
    let saved = std::fs::read_to_string(temp.path().join("config/apple-session.json")).unwrap();
    assert!(!saved.contains("synthetic-pcs"));
    assert!(!saved.contains("synthetic-escrow-session"));
}
#[tokio::test]
async fn existing_photos_authorization_cookie_does_not_request_redundant_device_approval() {
    let server = MockServer::start().await;
    init(&server).await;
    Mock::given(method("POST"))
        .and(path("/appleauth/auth/signin/complete"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("X-Apple-Session-Token", "synthetic-session")
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/setup/ws/1/accountLogin"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(account(&server, true))
                .insert_header(
                    "Set-Cookie",
                    "X-APPLE-WEBAUTH-PCS-Photos=existing-authorization; Path=/; HttpOnly",
                ),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/setup/ws/1/requestPCS"))
        .respond_with(ResponseTemplate::new(412).set_body_json(
            json!({"status":"failure","message":"User has not disabled iCloud DRS."}),
        ))
        .expect(0)
        .mount(&server)
        .await;
    let temp = tempfile::tempdir().unwrap();
    let mut client =
        ICloudClient::with_endpoints(&Paths::under(temp.path()), Endpoints::local(&server.uri()))
            .unwrap();
    assert_eq!(
        client
            .login("test@example.invalid", "synthetic-password")
            .await
            .unwrap()
            .state,
        "authenticated"
    );
    assert_eq!(
        client.authorize_device().await.unwrap().state,
        "authenticated"
    );
}
