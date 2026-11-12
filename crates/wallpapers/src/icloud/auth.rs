//! Bounded Apple web authentication. Persist sessions, never passwords or MFA codes.
use super::srp::{Challenge, SrpClient};
use crate::storage::{Paths, atomic_json, private_dir};
use anyhow::{Context, Result, ensure};
use reqwest::{
    Client, Method, Response, StatusCode, Url,
    header::{HeaderMap, HeaderValue},
};
use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use zeroize::Zeroizing;

const WIDGET: &str = "d39ba9916b7251055b22c7f910e2ea796ee65e98b2ddecea8f5dde8d9d1a815d";
const LIMIT: usize = 2 * 1024 * 1024;
#[derive(Clone)]
pub struct Endpoints {
    pub auth: String,
    pub setup: String,
    pub home: String,
}
impl Default for Endpoints {
    fn default() -> Self {
        Self {
            auth: "https://idmsa.apple.com/appleauth/auth".into(),
            setup: "https://setup.icloud.com/setup/ws/1".into(),
            home: "https://www.icloud.com".into(),
        }
    }
}
impl Endpoints {
    // Transport injection for protocol tests; never accepted from IPC/CLI input.
    pub fn local(origin: &str) -> Self {
        Self {
            auth: format!("{origin}/appleauth/auth"),
            setup: format!("{origin}/setup/ws/1"),
            home: origin.into(),
        }
    }
    pub fn for_domain(domain: &str) -> Result<Self> {
        ensure!(["com", "cn"].contains(&domain), "invalid_icloud_domain");
        Ok(if domain == "cn" {
            Self {
                auth: "https://idmsa.apple.com.cn/appleauth/auth".into(),
                setup: "https://setup.icloud.com.cn/setup/ws/1".into(),
                home: "https://www.icloud.com.cn".into(),
            }
        } else {
            Self::default()
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthInfo {
    pub state: String,
    pub account: Option<String>,
    pub error: Option<String>,
    pub adp_authorization_required: bool,
}
impl Default for AuthInfo {
    fn default() -> Self {
        Self {
            state: "not_signed_in".into(),
            account: None,
            error: None,
            adp_authorization_required: false,
        }
    }
}
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Session {
    username: String,
    client_id: String,
    headers: BTreeMap<String, String>,
    cookies: Value,
    account: Value,
    stage: String,
}

pub struct ICloudClient {
    pub http: Client,
    pub endpoints: Endpoints,
    pub paths: Paths,
    jar: Arc<CookieStoreMutex>,
    session: Session,
    info: AuthInfo,
    pending_password: Option<Zeroizing<String>>,
}
impl ICloudClient {
    pub fn new(paths: &Paths) -> Result<Self> {
        Self::with_endpoints(paths, Endpoints::default())
    }
    pub fn with_endpoints(paths: &Paths, endpoints: Endpoints) -> Result<Self> {
        private_dir(&paths.config)?;
        let path = paths.config.join("apple-session.json");
        let mut session: Session = if path.exists() {
            serde_json::from_slice(
                &crate::storage::read_bounded(&path, LIMIT as u64)
                    .map_err(|_| anyhow::anyhow!("invalid_saved_apple_session"))?,
            )
            .context("invalid_saved_apple_session")?
        } else {
            Session::default()
        };
        if session.client_id.is_empty() {
            session.client_id = uuid::Uuid::new_v4().to_string();
        }
        let cookies = if session.cookies.is_null() {
            CookieStore::default()
        } else {
            cookie_store::serde::json::load(std::io::Cursor::new(session.cookies.to_string()))
                .map_err(|_| anyhow::anyhow!("invalid_saved_apple_cookies"))?
        };
        let jar = Arc::new(CookieStoreMutex::new(cookies));
        let http = Client::builder()
            .cookie_provider(jar.clone())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 PhotoWallpapers/0.2")
            .build()?;
        let info = AuthInfo {
            account: (!session.username.is_empty()).then(|| session.username.clone()),
            state: if session.stage == "mfa_required" {
                "mfa_required".into()
            } else if session.headers.contains_key("session_token") {
                "session_saved".into()
            } else {
                "not_signed_in".into()
            },
            ..AuthInfo::default()
        };
        Ok(Self {
            http,
            endpoints,
            paths: paths.clone(),
            jar,
            session,
            info,
            pending_password: None,
        })
    }
    pub fn info(&self) -> AuthInfo {
        self.info.clone()
    }
    fn set_state(&mut self, state: &str, error: Option<&str>) -> AuthInfo {
        self.info.state = state.into();
        self.info.error = error.map(str::to_owned);
        self.info.account =
            (!self.session.username.is_empty()).then(|| self.session.username.clone());
        self.info.adp_authorization_required = state == "device_approval_required";
        self.session.stage = state.into();
        self.info()
    }
    pub fn save(&mut self) -> Result<()> {
        let mut cookies = Vec::new();
        let jar = self
            .jar
            .lock()
            .map_err(|_| anyhow::anyhow!("cookie_lock_failed"))?;
        cookie_store::serde::json::save_incl_expired_and_nonpersistent(&jar, &mut cookies)
            .map_err(|_| anyhow::anyhow!("cookie_save_failed"))?;
        drop(jar);
        self.session.cookies = serde_json::from_slice(&cookies)?;
        atomic_json(
            &self.paths.config.join("apple-session.json"),
            &serde_json::to_value(&self.session)?,
        )
    }
    fn auth_headers(&self) -> Result<HeaderMap> {
        let mut h = HeaderMap::new();
        for (k, v) in [
            ("Accept", "application/json, text/javascript"),
            ("Content-Type", "application/json"),
            ("X-Apple-OAuth-Client-Id", WIDGET),
            ("X-Apple-Widget-Key", WIDGET),
            ("X-Apple-OAuth-Client-Type", "firstPartyAuth"),
            ("X-Apple-OAuth-Redirect-URI", &self.endpoints.home),
            ("X-Apple-OAuth-Require-Grant-Code", "true"),
            ("X-Apple-OAuth-Response-Mode", "web_message"),
            ("X-Apple-OAuth-Response-Type", "code"),
            ("X-Apple-OAuth-State", &self.session.client_id),
        ] {
            h.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes())?,
                HeaderValue::from_str(v)?,
            );
        }
        let root = Url::parse(&self.endpoints.auth)?
            .origin()
            .ascii_serialization();
        h.insert("Origin", root.parse()?);
        h.insert("Referer", format!("{root}/").parse()?);
        for (header, key) in [("scnt", "scnt"), ("X-Apple-ID-Session-Id", "session_id")] {
            if let Some(v) = self.session.headers.get(key) {
                h.insert(
                    reqwest::header::HeaderName::from_bytes(header.as_bytes())?,
                    HeaderValue::from_str(v)?,
                );
            }
        }
        Ok(h)
    }
    fn capture(&mut self, h: &HeaderMap) {
        for (header, key) in [
            ("scnt", "scnt"),
            ("X-Apple-ID-Session-Id", "session_id"),
            ("X-Apple-ID-Account-Country", "account_country"),
            ("X-Apple-Session-Token", "session_token"),
            ("X-Apple-TwoSV-Trust-Token", "trust_token"),
        ] {
            if let Some(v) = h.get(header).and_then(|v| v.to_str().ok()) {
                self.session.headers.insert(key.into(), v.into());
            }
        }
    }
    async fn auth_request(
        &mut self,
        method: Method,
        path: &str,
        input: Option<Value>,
    ) -> Result<(StatusCode, HeaderMap, Value)> {
        let mut request = self
            .http
            .request(method, format!("{}{}", self.endpoints.auth, path))
            .headers(self.auth_headers()?);
        if let Some(body) = input {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("apple_network_error"))?;
        let status = response.status();
        let headers = response.headers().clone();
        self.capture(&headers);
        let body = bounded_json(response, LIMIT).await?;
        self.save()?;
        Ok((status, headers, body))
    }
    fn http_error(&mut self, status: StatusCode, credential_step: bool) -> anyhow::Error {
        let code = match status.as_u16() {
            429 => "apple_rate_limited",
            401 if credential_step => "apple_credentials_rejected",
            401 | 403 | 421 => "apple_session_expired",
            500..=599 => "apple_service_unavailable",
            _ => "apple_auth_failed",
        };
        self.set_state("error", Some(code));
        anyhow::anyhow!(code)
    }
    pub async fn login(&mut self, username: &str, password: &str) -> Result<AuthInfo> {
        let result = self.login_inner(username, password).await;
        if result.is_err() {
            self.pending_password = None;
            if self.info.error.is_none() {
                self.set_state("error", Some("apple_auth_failed"));
            }
            let _ = self.save();
        }
        result
    }
    async fn login_inner(&mut self, username: &str, password: &str) -> Result<AuthInfo> {
        ensure!(
            !username.is_empty()
                && username.len() <= 512
                && !password.is_empty()
                && password.len() <= 4096,
            "invalid_login_input"
        );
        if self.session.username != username {
            self.session = Session {
                username: username.into(),
                client_id: uuid::Uuid::new_v4().to_string(),
                ..Session::default()
            };
            self.jar
                .lock()
                .map_err(|_| anyhow::anyhow!("cookie_lock_failed"))?
                .clear();
        }
        self.pending_password = Some(Zeroizing::new(password.to_owned()));
        self.set_state("signing_in", None);
        let srp = SrpClient::random()?;
        let (status, _, body) = self
            .auth_request(
                Method::POST,
                "/signin/init",
                Some(
                    json!({"a":srp.public_a(),"accountName":username,"protocols":["s2k","s2k_fo"]}),
                ),
            )
            .await?;
        if !status.is_success() {
            return Err(self.http_error(status, false));
        }
        let challenge: Challenge =
            serde_json::from_value(body).map_err(|_| anyhow::anyhow!("invalid_apple_challenge"))?;
        let proof = srp.prove(username, password, &challenge)?;
        let tokens = self
            .session
            .headers
            .get("trust_token")
            .map(|t| vec![t.clone()])
            .unwrap_or_default();
        let(status,headers,_)=self.auth_request(Method::POST,"/signin/complete?isRememberMeEnabled=true",Some(json!({"accountName":username,"c":challenge.c,"m1":proof.m1,"m2":proof.m2,"rememberMe":true,"trustTokens":tokens}))).await?;
        if status == StatusCode::CONFLICT {
            if requires_escrow(&headers) {
                self.escrow().await?;
            } else {
                return self.begin_mfa().await;
            }
        } else if !status.is_success() {
            return Err(self.http_error(status, true));
        }
        self.setup_account().await
    }
    async fn begin_mfa(&mut self) -> Result<AuthInfo> {
        let (status, _, _) = self
            .auth_request(Method::PUT, "/verify/trusteddevice/securitycode", None)
            .await?;
        if !status.is_success() {
            return Err(self.http_error(status, false));
        }
        let info = self.set_state("mfa_required", None);
        self.save()?;
        Ok(info)
    }
    pub async fn submit_mfa(&mut self, code: &str) -> Result<AuthInfo> {
        ensure!(
            code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()),
            "invalid_mfa_code"
        );
        let (status, headers, body) = self
            .auth_request(
                Method::POST,
                "/verify/trusteddevice/securitycode",
                Some(json!({"securityCode":{"code":code}})),
            )
            .await?;
        if !status.is_success() {
            self.set_state("mfa_required", Some("invalid_mfa_code"));
            return Err(anyhow::anyhow!("invalid_mfa_code"));
        }
        ensure!(
            body.pointer("/securityCode/valid") != Some(&json!(false)),
            "invalid_mfa_code"
        );
        if requires_escrow(&headers) {
            self.escrow().await?;
        }
        let (status, headers, _) = self.auth_request(Method::GET, "/2sv/trust", None).await?;
        if requires_escrow(&headers) {
            self.escrow().await?;
        } else if !status.is_success() {
            return Err(self.http_error(status, false));
        }
        self.setup_account().await
    }
    async fn escrow(&mut self) -> Result<()> {
        let password = self
            .pending_password
            .as_ref()
            .map(|s| Zeroizing::new(s.to_string()))
            .context("apple_password_required")?;
        let srp = SrpClient::random()?;
        let (status, _, body) = self
            .auth_request(
                Method::POST,
                "/escrow/init",
                Some(json!({"a":srp.public_a(),"accountName":"","protocols":["s2k","s2k_fo"]})),
            )
            .await?;
        if !status.is_success() {
            return Err(self.http_error(status, false));
        }
        let challenge: Challenge =
            serde_json::from_value(body).map_err(|_| anyhow::anyhow!("invalid_apple_challenge"))?;
        let proof = srp.prove("", &password, &challenge)?;
        let (status, _, _) = self
            .auth_request(
                Method::POST,
                "/escrow/complete",
                Some(json!({"c":challenge.c,"m1":proof.m1,"m2":proof.m2,"k":proof.key})),
            )
            .await?;
        if !status.is_success() {
            return Err(self.http_error(status, false));
        }
        Ok(())
    }
    async fn setup_request(&mut self, path: &str, input: Value) -> Result<(StatusCode, Value)> {
        let response = self
            .http
            .post(format!("{}{path}", self.endpoints.setup))
            .header("Origin", &self.endpoints.home)
            .header("Referer", format!("{}/", self.endpoints.home))
            .json(&input)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("apple_network_error"))?;
        let status = response.status();
        self.capture(response.headers());
        let body = bounded_json(response, LIMIT).await?;
        self.save()?;
        Ok((status, body))
    }
    async fn setup_account(&mut self) -> Result<AuthInfo> {
        let input = json!({"accountCountryCode":self.session.headers.get("account_country").cloned().unwrap_or_default(),"dsWebAuthToken":self.session.headers.get("session_token").cloned().unwrap_or_default(),"extended_login":true,"trustToken":self.session.headers.get("trust_token").cloned().unwrap_or_default()});
        let (status, body) = self.setup_request("/accountLogin", input).await?;
        if !status.is_success() {
            return Err(self.http_error(status, false));
        }
        self.accept_account(body).await
    }
    async fn accept_account(&mut self, body: Value) -> Result<AuthInfo> {
        if body["isRepairNeeded"] == true || body["termsUpdateNeeded"] == true {
            self.set_state("account_action_required", Some("apple_web_action_required"));
            self.save()?;
            return Ok(self.info());
        }
        if body.pointer("/dsInfo/isWebAccessAllowed") == Some(&json!(false)) {
            self.set_state(
                "account_action_required",
                Some("icloud_web_access_unavailable"),
            );
            self.save()?;
            return Ok(self.info());
        }
        if body["hsaChallengeRequired"] == true {
            return self.begin_mfa().await;
        }
        self.session.account = body;
        self.photos_url()?;
        if self
            .session
            .account
            .pointer("/webservices/ckdatabasews/pcsRequired")
            == Some(&json!(true))
        {
            return self.authorize_device().await;
        }
        self.pending_password = None;
        let info = self.set_state("authenticated", None);
        self.save()?;
        Ok(info)
    }
    pub async fn authorize_device(&mut self) -> Result<AuthInfo> {
        // pcsRequired describes the service requirement, not a missing grant.
        // Standard-protection accounts can already have the PCS cookie and reject
        // an unnecessary requestPCS with 412. Never ask to change their security.
        if self.photos_authorized()? {
            self.pending_password = None;
            let info = self.set_state("authenticated", None);
            self.save()?;
            return Ok(info);
        }
        let (status, body) = self
            .setup_request(
                "/requestPCS",
                json!({"appName":"photos","derivedFromUserAction":true}),
            )
            .await?;
        // Never alter ADP/DRS settings. Apple may require trusted-device approval.
        if !status.is_success() || body["status"] == "failure" {
            let info = self.set_state(
                "device_approval_required",
                Some("apple_device_approval_required"),
            );
            self.save()?;
            return Ok(info);
        }
        if !self.photos_authorized()? {
            let info = self.set_state(
                "device_approval_required",
                Some("apple_device_approval_required"),
            );
            self.save()?;
            return Ok(info);
        }
        self.pending_password = None;
        let info = self.set_state("authenticated", None);
        self.save()?;
        Ok(info)
    }
    fn photos_authorized(&self) -> Result<bool> {
        let url = Url::parse(&format!(
            "{}/database/1/com.apple.photos.cloud/production/private/changes/database",
            self.photos_url()?
        ))?;
        let cookies = self
            .jar
            .lock()
            .map_err(|_| anyhow::anyhow!("cookie_lock_failed"))?;
        Ok(cookies
            .get_request_values(&url)
            .any(|(name, value)| name == "X-APPLE-WEBAUTH-PCS-Photos" && !value.is_empty()))
    }
    pub async fn resume(&mut self) -> Result<AuthInfo> {
        if !self.session.headers.contains_key("session_token") {
            return Ok(self.set_state("not_signed_in", None));
        }
        let (status, body) = self.setup_request("/validate", Value::Null).await?;
        if status.is_success() {
            self.accept_account(body).await
        } else if status == StatusCode::UNAUTHORIZED || status.as_u16() == 421 {
            self.setup_account().await
        } else {
            Err(self.http_error(status, false))
        }
    }
    pub fn photos_url(&self) -> Result<String> {
        let raw = self
            .session
            .account
            .pointer("/webservices/ckdatabasews/url")
            .and_then(Value::as_str)
            .context("icloud_photos_unavailable")?;
        let url = Url::parse(raw).map_err(|_| anyhow::anyhow!("invalid_apple_service_url"))?;
        let local = self.endpoints.home.starts_with("http://127.0.0.1:")
            && raw.starts_with(&self.endpoints.home);
        let trusted = url.scheme() == "https"
            && url.host_str().is_some_and(|h| {
                h == "icloud.com"
                    || h.ends_with(".icloud.com")
                    || h == "icloud.com.cn"
                    || h.ends_with(".icloud.com.cn")
            });
        ensure!(local || trusted, "untrusted_apple_service_url");
        Ok(raw.trim_end_matches('/').into())
    }
    pub fn dsid(&self) -> Result<String> {
        self.session
            .account
            .pointer("/dsInfo/dsid")
            .and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .context("missing_apple_account_id")
    }
    pub fn client_id(&self) -> &str {
        &self.session.client_id
    }
    pub fn logout(&mut self) -> Result<AuthInfo> {
        self.pending_password = None;
        self.jar
            .lock()
            .map_err(|_| anyhow::anyhow!("cookie_lock_failed"))?
            .clear();
        self.session = Session {
            client_id: uuid::Uuid::new_v4().to_string(),
            ..Session::default()
        };
        self.info = AuthInfo::default();
        self.save()?;
        Ok(self.info())
    }
}
fn requires_escrow(h: &HeaderMap) -> bool {
    h.contains_key("X-Apple-EDP") || h.contains_key("X-Apple-PDP")
}
pub async fn bounded_json(mut response: Response, limit: usize) -> Result<Value> {
    ensure!(
        response.content_length().is_none_or(|n| n <= limit as u64),
        "apple_response_too_large"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("apple_network_error"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= limit,
            "apple_response_too_large"
        );
        bytes.extend(chunk);
    }
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    // Non-JSON error bodies are discarded; never incorporate them in diagnostics.
    Ok(serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({})))
}
