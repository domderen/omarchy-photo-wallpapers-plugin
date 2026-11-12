use base64::{Engine, engine::general_purpose::STANDARD as B64};
use omarchy_photo_wallpapers::{
    icloud::{
        auth::{Endpoints, ICloudClient},
        photos::*,
    },
    storage::{Paths, Store, atomic_json},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, header, method, path},
};
const MEDIA: &[u8] = b"\xff\xd8\xffsynthetic-jpeg";
fn records(origin: &str) -> Value {
    json!([
     {"recordType":"CPLMaster","recordName":"master-1","recordChangeTag":"m1","fields":{"filenameEnc":{"value":B64.encode("same.jpg")},"resOriginalWidth":{"value":4000},"resOriginalHeight":{"value":3000},"resOriginalFileType":{"value":"public.jpeg"},"resOriginalRes":{"value":{"downloadURL":format!("{origin}/image"),"size":MEDIA.len(),"fileChecksum":B64.encode(Sha256::digest(MEDIA))}}}},
     {"recordType":"CPLAsset","recordName":"asset-1","recordChangeTag":"a1","fields":{"masterRef":{"value":{"recordName":"master-1"}},"assetDate":{"value":1700000000000i64},"isFavorite":{"value":1}}}
    ])
}
fn client(paths: &Paths, origin: &str) -> ICloudClient {
    atomic_json(&paths.config.join("apple-session.json"),&json!({"username":"synthetic@example.invalid","client_id":"test","headers":{"session_token":"synthetic"},"account":{"dsInfo":{"dsid":"123"},"webservices":{"ckdatabasews":{"url":origin}}}})).unwrap();
    ICloudClient::with_endpoints(paths, Endpoints::local(origin)).unwrap()
}
#[test]
fn parsing_scopes_identity_and_excludes_hidden_without_filename_matching() {
    let zone = json!({"zoneName":"PrimarySync"});
    let mut rows = records("https://cvws.icloud-content.com");
    let asset = parse_assets("123", "private", &zone, rows.as_array().unwrap())
        .unwrap()
        .remove(0);
    assert!(asset.id.contains("123"));
    assert_eq!(asset.cloud_guid, "asset-1");
    assert_eq!(asset.filename, "same.jpg");
    assert_eq!(asset.rendition, "original_fallback");
    assert_eq!((asset.width, asset.height), (4000, 3000));
    assert!(asset.favorite);
    rows[1]["fields"]["isHidden"] = json!({"value":1});
    assert!(
        parse_assets("123", "private", &zone, rows.as_array().unwrap())
            .unwrap()
            .is_empty()
    );
    rows[1]["fields"]["isHidden"] = json!({"value":0});
    rows[1]["fields"]["masterRef"]["value"]["recordName"] = json!("missing");
    assert!(parse_assets("123", "private", &zone, rows.as_array().unwrap()).is_err());
}
#[tokio::test]
async fn helper_free_pagination_and_direct_validated_download() {
    let server = MockServer::start().await;
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::under(temp.path());
    let store = Store::open(&paths).unwrap();
    let progress = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = progress.clone();
    let provider = PhotosProvider::new(&client(&paths, &server.uri()))
        .unwrap()
        .with_progress(move |value| observed.lock().unwrap().push(value));
    Mock::given(method("POST"))
        .and(body_partial_json(
            json!({"query":{"recordType":"HyperionIndexCountLookup"}}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"records":[{"fields":{"itemCount":{"value":1}}}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(
            "/database/1/com.apple.photos.cloud/production/private/changes/database",
        ))
        .and(header("Origin", server.uri()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"zones":[{"zoneID":{"zoneName":"PrimarySync"}}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(
            "/database/1/com.apple.photos.cloud/production/shared/changes/database",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"zones":[]})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            json!({"query":{"recordType":"CPLAlbumByPositionLive"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"records":[]})))
        .mount(&server)
        .await;
    Mock::given(method("POST")).and(body_partial_json(json!({"query":{"recordType":ALL_QUERY,"filterBy":[{"fieldName":"startRank","fieldValue":{"type":"INT64","value":0},"comparator":"EQUALS"},{"fieldName":"direction","fieldValue":{"type":"STRING","value":"ASCENDING"},"comparator":"EQUALS"}]}}))).respond_with(ResponseTemplate::new(200).set_body_json(json!({"records":[records(&server.uri())[1].clone()],"continuationMarker":"next-100"}))).with_priority(10).expect(1).mount(&server).await;
    Mock::given(method("POST")).and(body_partial_json(json!({"query":{"recordType":ALL_QUERY,"filterBy":[{"fieldName":"startRank","fieldValue":{"type":"INT64","value":0},"comparator":"EQUALS"},{"fieldName":"direction","fieldValue":{"type":"STRING","value":"ASCENDING"},"comparator":"EQUALS"}]},"continuationMarker":"next-100"}))).respond_with(ResponseTemplate::new(200).set_body_json(json!({"records":[]}))).expect(1).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/image"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(MEDIA.to_vec()))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(
            "/database/1/com.apple.photos.cloud/production/private/records/lookup",
        ))
        .and(body_partial_json(
            json!({"records":[{"recordName":"master-1"}]}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"records":[records(&server.uri())[0].clone()]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let snapshot = provider.catalog().await.unwrap();
    assert_eq!(snapshot.assets.len(), 1);
    assert!(
        progress
            .lock()
            .unwrap()
            .iter()
            .any(|value| value["phase"] == "photos"
                && value["completed"] == 1
                && value["total"] == 1)
    );
    snapshot.activate(&store).unwrap();
    assert_eq!(store.assets().unwrap().len(), 1);
    let asset = &snapshot.assets[0];
    let image = provider.download(asset, &paths).await.unwrap();
    assert_eq!(std::fs::read(&image).unwrap(), MEDIA);
    assert_eq!(provider.download(asset, &paths).await.unwrap(), image);
    let mut metadata_changed = asset.clone();
    metadata_changed.revision = "favorite-or-album-change".into();
    assert_eq!(
        provider.download(&metadata_changed, &paths).await.unwrap(),
        image
    );
    std::fs::write(&image, b"corrupted").unwrap();
    provider.download(asset, &paths).await.unwrap();
    assert_eq!(std::fs::read(image).unwrap(), MEDIA);
    assert!(!paths.config.join("credentials.json").exists());
}
#[tokio::test]
async fn incomplete_or_expired_catalog_preserves_last_publication() {
    let server = MockServer::start().await;
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::under(temp.path());
    let store = Store::open(&paths).unwrap();
    let provider = PhotosProvider::new(&client(&paths, &server.uri())).unwrap();
    let asset = parse_assets(
        "123",
        "private",
        &json!({"zoneName":"PrimarySync"}),
        records(&server.uri()).as_array().unwrap(),
    )
    .unwrap();
    store.activate_catalog(&asset, &[], &[], "old").unwrap();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(421).set_body_string("private-session-details"))
        .expect(1)
        .mount(&server)
        .await;
    let error = provider.catalog().await.err().unwrap();
    assert_eq!(error.to_string(), "apple_session_expired");
    assert_eq!(store.assets().unwrap().len(), 1);
}

#[tokio::test]
async fn album_continuations_and_transient_errors_are_bounded() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let server = MockServer::start().await;
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::under(temp.path());
    Store::open(&paths).unwrap();
    let provider = PhotosProvider::new(&client(&paths, &server.uri())).unwrap();
    Mock::given(method("POST"))
        .and(body_partial_json(
            json!({"query":{"recordType":"HyperionIndexCountLookup"}}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"records":[{"fields":{"itemCount":{"value":1}}}]})),
        )
        .mount(&server)
        .await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    Mock::given(method("POST"))
        .and(path(
            "/database/1/com.apple.photos.cloud/production/private/changes/database",
        ))
        .respond_with(move |_: &wiremock::Request| {
            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(503)
            } else {
                ResponseTemplate::new(200)
                    .set_body_json(json!({"zones":[{"zoneID":{"zoneName":"PrimarySync"}}]}))
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(
            "/database/1/com.apple.photos.cloud/production/shared/changes/database",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"zones":[]})))
        .mount(&server)
        .await;
    let origin = server.uri();
    Mock::given(method("POST")).and(body_partial_json(json!({"query":{"recordType":"CPLAlbumByPositionLive"}}))).respond_with(move |r:&wiremock::Request| {
  let input:Value=serde_json::from_slice(&r.body).unwrap();
  if input.pointer("/query/filterBy/0/fieldValue/value").and_then(Value::as_str)==Some("folder") {ResponseTemplate::new(200).set_body_json(json!({"records":[{"recordName":"album-3","fields":{"albumType":{"value":0},"albumNameEnc":{"value":B64.encode("Nested")}}}]}))}else if input["continuationMarker"]=="second" {ResponseTemplate::new(200).set_body_json(json!({"records":[{"recordName":"album-2","fields":{"albumType":{"value":0},"albumNameEnc":{"value":B64.encode("Second")}}}]}))}else{ResponseTemplate::new(200).set_body_json(json!({"records":[{"recordName":"album-1","fields":{"albumType":{"value":0},"albumNameEnc":{"value":B64.encode("First")}}},{"recordName":"folder","fields":{"albumType":{"value":3}}}],"continuationMarker":"second"}))}
 }).expect(3).mount(&server).await;
    Mock::given(method("POST"))
        .and(path(
            "/database/1/com.apple.photos.cloud/production/private/records/query",
        ))
        .and(body_partial_json(json!({"query":{"recordType":"CPLContainerRelationLiveByPosition"}})))
        .respond_with(move |r: &wiremock::Request| {
            let input: Value = serde_json::from_slice(&r.body).unwrap();
            let rank = input
                .pointer("/query/filterBy/0/fieldValue/value")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            ResponseTemplate::new(200)
                .set_body_json(json!({"records":if rank==0 {records(&origin)}else{panic!("terminal page was queried again")}}))
        })
        .with_priority(10)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"query":{"recordType":ALL_QUERY}})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"records":records(&server.uri())})),
        )
        .mount(&server)
        .await;
    let snapshot = provider.catalog().await.unwrap();
    assert_eq!(snapshot.sources.len(), 4);
    assert_eq!(snapshot.memberships.len(), 4);
    assert_eq!(snapshot.sources[1].name, "First");
    assert_eq!(snapshot.sources[2].name, "Second");
}

#[tokio::test]
async fn downloads_reject_wrong_size_html_and_unsafe_origins() {
    let server = MockServer::start().await;
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::under(temp.path());
    Store::open(&paths).unwrap();
    let provider = PhotosProvider::new(&client(&paths, &server.uri())).unwrap();
    let mut asset = parse_assets(
        "123",
        "private",
        &json!({"zoneName":"PrimarySync"}),
        records(&server.uri()).as_array().unwrap(),
    )
    .unwrap()
    .remove(0);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; MEDIA.len()]))
        .mount(&server)
        .await;
    assert_eq!(
        provider
            .download(&asset, &paths)
            .await
            .err()
            .unwrap()
            .to_string(),
        "invalid_downloaded_media"
    );
    asset.resources.get_mut("original_fallback").unwrap().url =
        "https://icloud.com.attacker.invalid/image".into();
    assert_eq!(
        provider
            .download(&asset, &paths)
            .await
            .err()
            .unwrap()
            .to_string(),
        "untrusted_media_url"
    );
    for url in [
        "http://icloud.com/image",
        "https://user:pass@icloud.com/image",
        "https://localhost/image",
    ] {
        assert!(validate_media_url(url, None).is_err());
    }
}
#[test]
fn incomplete_preferred_rendition_falls_back_without_rejecting_valid_catalog_images() {
    let mut rows = records("https://cvws.icloud-content.com");
    rows[1]["fields"]["resJPEGFullRes"] = json!({"value":{"downloadURL":"https://cvws.icloud-content.com/edited","size":100,"fileChecksum":"opaque"}});
    let assets = parse_assets(
        "123",
        "private",
        &json!({"zoneName":"PrimarySync"}),
        rows.as_array().unwrap(),
    )
    .unwrap();
    assert_eq!(assets[0].rendition, "original_fallback");
}
#[tokio::test]
async fn a_terminal_page_shorter_than_the_cloud_count_cannot_replace_a_complete_catalog() {
    let server = MockServer::start().await;
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::under(temp.path());
    let store = Store::open(&paths).unwrap();
    let old = parse_assets(
        "123",
        "private",
        &json!({"zoneName":"PrimarySync"}),
        records(&server.uri()).as_array().unwrap(),
    )
    .unwrap();
    store
        .activate_catalog(&old, &[], &[], "previous-complete")
        .unwrap();
    let provider = PhotosProvider::new(&client(&paths, &server.uri())).unwrap();
    Mock::given(method("POST"))
        .and(path(
            "/database/1/com.apple.photos.cloud/production/private/changes/database",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"zones":[{"zoneID":{"zoneName":"PrimarySync"}}]})),
        )
        .mount(&server)
        .await;
    let origin = server.uri();
    Mock::given(method("POST"))
        .and(path(
            "/database/1/com.apple.photos.cloud/production/private/records/query",
        ))
        .respond_with(move |r: &wiremock::Request| {
            let value: Value = serde_json::from_slice(&r.body).unwrap();
            ResponseTemplate::new(200).set_body_json(
                if value["query"]["recordType"] == "HyperionIndexCountLookup" {
                    json!({"records":[{"fields":{"itemCount":{"value":2}}}]})
                } else {
                    json!({"records":records(&origin)})
                },
            )
        })
        .mount(&server)
        .await;
    assert_eq!(
        provider.catalog().await.err().unwrap().to_string(),
        "apple_catalog_incomplete"
    );
    assert_eq!(
        store.meta("catalog_generation").unwrap(),
        "previous-complete"
    );
    assert_eq!(store.assets().unwrap().len(), 1);
}
#[tokio::test]
async fn expired_signed_media_url_is_refreshed_by_exact_cloud_identity_without_helper_or_rescan() {
    let server = MockServer::start().await;
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::under(root.path());
    Store::open(&paths).unwrap();
    let provider = PhotosProvider::new(&client(&paths, &server.uri())).unwrap();
    let asset = parse_assets(
        "123",
        "private",
        &json!({"zoneName":"PrimarySync"}),
        records(&server.uri()).as_array().unwrap(),
    )
    .unwrap()
    .remove(0);
    Mock::given(method("GET"))
        .and(path("/image"))
        .respond_with(ResponseTemplate::new(410))
        .expect(1)
        .mount(&server)
        .await;
    let mut current = records(&server.uri());
    current[0]["fields"]["resOriginalRes"]["value"]["downloadURL"] =
        json!(format!("{}/fresh-image", server.uri()));
    Mock::given(method("POST"))
        .and(path(
            "/database/1/com.apple.photos.cloud/production/private/records/lookup",
        ))
        .and(body_partial_json(
            json!({"records":[{"recordName":"asset-1"},{"recordName":"master-1"}]}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"records":current})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/fresh-image"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(MEDIA.to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let file = provider.download(&asset, &paths).await.unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), MEDIA);
    assert_eq!(provider.download(&asset, &paths).await.unwrap(), file);
}
