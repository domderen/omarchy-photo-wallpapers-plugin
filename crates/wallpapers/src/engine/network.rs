//! Serialized Apple authentication owner; catalog and helper ingestion run off controller.
use super::{Message, ProviderSlot};
use crate::{
    enrichment::{self, Binding, HelperConfig},
    icloud::{
        auth::{AuthInfo, ICloudClient},
        photos::PhotosProvider,
    },
    ipc::Request,
    storage::Store,
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;
pub(super) enum Job {
    Auth {
        operation: String,
        username: String,
        password: Zeroizing<String>,
        code: Zeroizing<String>,
    },
    Pair {
        url: String,
        token: Zeroizing<String>,
    },
    Sync(String),
}
fn take(input: &mut Value, key: &str) -> String {
    input
        .get_mut(key)
        .map(Value::take)
        .and_then(|v| {
            if let Value::String(s) = v {
                Some(s)
            } else {
                None
            }
        })
        .unwrap_or_default()
}
impl Job {
    pub fn resume() -> Self {
        Self::Auth {
            operation: "resume".into(),
            username: String::new(),
            password: Zeroizing::new(String::new()),
            code: Zeroizing::new(String::new()),
        }
    }

    pub fn from_request(request: &mut Request) -> Result<Self> {
        if request.command == "pair" {
            return Ok(Self::Pair {
                url: take(&mut request.input, "helper_url"),
                token: Zeroizing::new(take(&mut request.input, "token")),
            });
        }
        ensure!(
            ["login", "mfa", "authorize", "resume", "logout"].contains(&request.operation.as_str()),
            "invalid_operation"
        );
        Ok(Self::Auth {
            operation: request.operation.clone(),
            username: take(&mut request.input, "username"),
            password: Zeroizing::new(take(&mut request.input, "password")),
            code: Zeroizing::new(take(&mut request.input, "code")),
        })
    }
}
pub(super) fn start(
    store: Store,
    tx: mpsc::Sender<Message>,
    slot: ProviderSlot,
    stop: CancellationToken,
) -> mpsc::Sender<Job> {
    let (sender, mut receiver) = mpsc::channel(8);
    let boot = sender.clone();
    let (helper_wake, helper_updates) = tokio::sync::watch::channel(0u64);
    helper_loop(store.clone(), tx.clone(), stop.clone(), helper_updates);
    tokio::spawn(async move {
        let mut recovered = false;
        let mut client = match ICloudClient::new(&store.paths) {
            Ok(client) => client,
            Err(error) => {
                if ["invalid_saved_apple_session", "invalid_saved_apple_cookies"]
                    .contains(&error.to_string().as_str())
                {
                    let backups = store.paths.data.join("session-backups");
                    let _ = crate::storage::private_dir(&backups);
                    let source = store.paths.config.join("apple-session.json");
                    let target = backups.join(format!("{}.json", uuid::Uuid::new_v4()));
                    // A rejected symlink/foreign object must not be quarantined
                    // and then chmodded through its target. Keep a validated
                    // regular-file descriptor for the corrupt-session backup.
                    if let Ok(file) =
                        crate::storage::private_file(&source, rustix::fs::OFlags::RDONLY)
                        && std::fs::rename(source, &target).is_ok()
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
                        recovered = true;
                    }
                }
                match ICloudClient::new(&store.paths) {
                    Ok(client) => client,
                    Err(_) => {
                        let _ = tx
                            .send(Message::Auth {
                                info: AuthInfo {
                                    state: "error".into(),
                                    error: Some("invalid_saved_apple_session".into()),
                                    ..AuthInfo::default()
                                },
                                busy: false,
                                scope: None,
                            })
                            .await;
                        return;
                    }
                }
            }
        };
        let mut session_epoch = store
            .meta("sync_authorization_epoch")
            .ok()
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let mut pending = None;
        *slot.write().await = PhotosProvider::new(&client).ok();
        let saved = client.info().state != "not_signed_in";
        let _ = tx
            .send(Message::Auth {
                info: if recovered {
                    AuthInfo {
                        state: "error".into(),
                        error: Some("invalid_saved_apple_session".into()),
                        ..AuthInfo::default()
                    }
                } else {
                    client.info()
                },
                busy: false,
                scope: client.dsid().ok(),
            })
            .await;
        if saved {
            let _ = boot.try_send(Job::Auth {
                operation: "resume".into(),
                username: String::new(),
                password: Zeroizing::new(String::new()),
                code: Zeroizing::new(String::new()),
            });
        }
        loop {
            let job = if let Some(job) = pending.take() {
                job
            } else {
                tokio::select! {_=stop.cancelled()=>break,job=receiver.recv()=>match job{Some(job)=>job,None=>break}}
            };
            match job {
                Job::Auth {
                    operation,
                    username,
                    password,
                    code,
                } => {
                    session_epoch += 1;
                    let _ = store.set_meta(
                        "sync_authorization_epoch",
                        &serde_json::json!(session_epoch),
                    );
                    let _ = tx
                        .send(Message::Auth {
                            info: client.info(),
                            busy: true,
                            scope: client.dsid().ok(),
                        })
                        .await;
                    let result = tokio::select! {_=stop.cancelled()=>break,result=async{match operation.as_str(){"login"=>client.login(&username,&password).await,"mfa"=>client.submit_mfa(&code).await,"authorize"=>client.authorize_device().await,"resume"=>client.resume().await,"logout"=>client.logout(),_=>Err(anyhow::anyhow!("invalid_operation"))}}=>result};
                    let info = match result {
                        Ok(info) => info,
                        Err(error) => {
                            let mut info = client.info();
                            info.error = Some(super::public_error(&error).into());
                            if ["authenticated", "session_saved"].contains(&info.state.as_str()) {
                                info.state = "offline".into();
                            }
                            info
                        }
                    };
                    *slot.write().await = PhotosProvider::new(&client).ok();
                    let _ = tx
                        .send(Message::Auth {
                            info,
                            busy: false,
                            scope: client.dsid().ok(),
                        })
                        .await;
                }
                Job::Sync(job) => {
                    // Refresh optional metadata immediately, independently of
                    // the cloud scan, and again after a new base is published.
                    helper_wake.send_modify(|v| *v += 1);
                    let result = tokio::select! {_=stop.cancelled()=>break,r=sync(&store,&mut client,&slot,session_epoch,tx.clone())=>r,next=receiver.recv()=>{pending=next;Err(anyhow::anyhow!("command_cancelled"))}};
                    if result.is_ok() {
                        helper_wake.send_modify(|v| *v += 1);
                    }
                    let _ = tx.send(Message::Synced { job, result }).await;
                }
                Job::Pair { url, token } => {
                    let result = tokio::select! {_=stop.cancelled()=>break,r=pair(&store,&client,url,token)=>r};
                    let _ = tx.send(Message::Paired { result }).await;
                }
            }
        }
    });
    sender
}
async fn sync(
    store: &Store,
    client: &mut ICloudClient,
    slot: &ProviderSlot,
    epoch: u64,
    tx: mpsc::Sender<Message>,
) -> Result<()> {
    ensure!(
        client.info().state == "authenticated",
        "apple_sign_in_required"
    );
    let provider = PhotosProvider::new(client)?;
    let snapshot = provider
        .clone()
        .with_progress(move |progress| {
            let _ = tx.try_send(Message::SyncProgress(progress));
        })
        .catalog()
        .await?;
    let worker = store.clone();
    let account = client.dsid()?;
    tokio::task::spawn_blocking(move || {
        worker.activate_catalog_checked(
            &snapshot.assets,
            &snapshot.sources,
            &snapshot.memberships,
            &uuid::Uuid::new_v4().to_string(),
            Some((epoch, account)),
        )
    })
    .await??;
    *slot.write().await = Some(provider);
    Ok(())
}
async fn pair(
    store: &Store,
    client: &ICloudClient,
    url: String,
    token: Zeroizing<String>,
) -> Result<()> {
    let account = client.dsid().context("apple_sign_in_required")?;
    let mut config = HelperConfig {
        helper_url: url,
        token: token.to_string(),
        binding: Binding::for_account(store, &account, "")?,
    };
    let doc = config.fetch().await?;
    config.binding.namespace = doc.provider_namespace.clone();
    let worker = store.clone();
    let binding = config.binding.clone();
    tokio::task::spawn_blocking(move || {
        let report = enrichment::import(&worker, &doc, &binding)?;
        crate::migration::resolve_legacy(&worker)?;
        Ok::<_, anyhow::Error>(report)
    })
    .await??;
    config.save(store)?;
    Ok(())
}

fn helper_loop(
    store: Store,
    tx: mpsc::Sender<Message>,
    stop: CancellationToken,
    mut wake: tokio::sync::watch::Receiver<u64>,
) {
    tokio::spawn(async move {
        loop {
            // Legacy credentials are optional and never consulted by the Apple provider.
            // Once a verified cloud base exists, reuse the already authorized helper pairing.
            let configured = HelperConfig::load(&store).ok().flatten();
            let config = if configured.is_some() {
                configured
            } else {
                legacy_helper_config(&store).ok().flatten()
            };
            if let Some(mut config) = config {
                if store.meta("base-account-id").ok().as_ref()
                    == Some(&serde_json::json!(config.binding.account_id))
                    && let Ok(binding) = Binding::for_account(
                        &store,
                        &config.binding.account_id,
                        &config.binding.namespace,
                    )
                {
                    config.binding = binding;
                }
                let result = tokio::select! {_=stop.cancelled()=>break,r=config.fetch()=>r};
                let info = match result {
                    Ok(doc) => {
                        if config.binding.namespace.is_empty() {
                            if store.meta("legacy_namespace").ok().as_ref()
                                != Some(&serde_json::json!(doc.provider_namespace))
                            {
                                let _=tx.send(Message::Helper(serde_json::json!({"url":config.helper_url,"stale":true,"error":"legacy_helper_namespace_mismatch"}))).await;
                                tokio::select! {_=stop.cancelled()=>break,_=tokio::time::sleep(std::time::Duration::from_secs(900))=>{},result=wake.changed()=>{if result.is_err(){break;}}};
                                continue;
                            }
                            config.binding.namespace = doc.provider_namespace.clone();
                        }
                        let created = doc.created_at.clone();
                        let worker = store.clone();
                        let binding = config.binding.clone();
                        let report = tokio::task::spawn_blocking(move || {
                            let report = enrichment::import(&worker, &doc, &binding)?;
                            crate::migration::resolve_legacy(&worker)?;
                            Ok::<_, anyhow::Error>(report)
                        })
                        .await;
                        match report {
                            Ok(Ok(report)) => {
                                let _ = config.save(&store);
                                serde_json::json!({"url":config.helper_url,"namespace":config.binding.namespace,"stale":false,"changed":report.changed,"last_refresh":created,"matched":report.matched,"unmatched":report.unmatched,"ambiguous":report.ambiguous})
                            }
                            _ => {
                                serde_json::json!({"url":config.helper_url,"stale":true,"error":"enrichment_identity_mismatch"})
                            }
                        }
                    }
                    Err(_) => {
                        let _ = enrichment::mark_stale(&store, &config.binding.namespace);
                        serde_json::json!({"url":config.helper_url,"stale":true,"error":"helper_offline"})
                    }
                };
                let _ = tx.send(Message::Helper(info)).await;
            }
            tokio::select! {_=stop.cancelled()=>break,_=tokio::time::sleep(std::time::Duration::from_secs(900))=>{},result=wake.changed()=>{if result.is_err(){break;}}}
        }
    });
}

fn legacy_helper_config(store: &Store) -> Result<Option<HelperConfig>> {
    if store.meta("legacy_imported")? != true {
        return Ok(None);
    }
    let Some(account) = store.meta("base-account-id")?.as_str().map(str::to_owned) else {
        return Ok(None);
    };
    let path = store.paths.config.join("credentials.json");
    if !path.exists() {
        return Ok(None);
    }
    ensure!(
        std::fs::metadata(&path)?.len() <= 65536,
        "invalid_helper_configuration"
    );
    let value: Value = serde_json::from_slice(&crate::storage::read_bounded(&path, 65536)?)?;
    let config = HelperConfig {
        helper_url: value["helper_url"]
            .as_str()
            .context("invalid_helper_configuration")?
            .into(),
        token: value["token"]
            .as_str()
            .context("invalid_helper_configuration")?
            .into(),
        binding: Binding::for_account(store, &account, "")?,
    };
    config.validate()?;
    Ok(Some(config))
}
