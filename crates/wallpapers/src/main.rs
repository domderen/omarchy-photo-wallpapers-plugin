use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use omarchy_photo_wallpapers::{
    engine::{Engine, NativePipeline},
    ipc::{self, Request},
    platform::{self, Omarchy},
    storage::{Paths, Store},
};
use serde_json::{Value, json};
use std::{io::Read, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;
#[derive(Parser)]
#[command(version, about = "Independent iCloud Photos wallpapers for Omarchy")]
struct Cli {
    #[arg(long, global = true)]
    profile: Option<PathBuf>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    #[command(hide = true)]
    PrivacyWorker,
    Daemon {
        #[arg(long, help = "Disable networking for isolated offline diagnostics")]
        no_network: bool,
    },
    Status,
    /// Enter or leave durable Meeting Mode; never a blind toggle.
    Meeting {
        #[arg(default_value="status", value_parser=["on","off","status"])]
        operation: String,
    },
    /// Install verified local screening data and native runtime, or inspect/cancel installation.
    Models {
        #[arg(default_value="status", value_parser=["install","status","cancel"])]
        operation: String,
    },
    /// Check local daemon readiness without exposing account or media state.
    Health,
    Sources,
    Config {
        #[arg(default_value = "get")]
        operation: String,
        #[arg(long)]
        expected_revision: Option<u64>,
        #[arg(long)]
        stdin_json: bool,
    },
    Auth {
        #[arg(default_value = "status")]
        operation: String,
        #[arg(long)]
        stdin_json: bool,
    },
    Pair {
        #[arg(long)]
        stdin_json: bool,
    },
    Next {
        #[arg(long)]
        request_id: Option<String>,
    },
    Previous {
        #[arg(long)]
        request_id: Option<String>,
    },
    Refresh {
        #[arg(long)]
        request_id: Option<String>,
    },
    Pause,
    Resume,
    Pin,
    Unpin,
    Install {
        #[arg(long)]
        stage: bool,
        #[arg(
            long,
            help = "Install only backend/service; preserve Omarchy's Git-managed plugin"
        )]
        backend_only: bool,
    },
    Uninstall {
        #[arg(long)]
        purge: bool,
    },
    AdoptSession {
        #[arg(long)]
        from_config: PathBuf,
        #[arg(long)]
        from_data: PathBuf,
        #[arg(long)]
        from_runtime: PathBuf,
    },
    AdoptCatalog {
        #[arg(long)]
        from_config: PathBuf,
        #[arg(long)]
        from_data: PathBuf,
        #[arg(long)]
        from_runtime: PathBuf,
    },
    Feedback {
        #[arg(long)]
        action: String,
        #[arg(long)]
        asset_id: Option<String>,
        #[arg(long)]
        stdin_json: bool,
    },
}
fn input(enabled: bool) -> Result<Value> {
    if !enabled {
        return Ok(Value::Null);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    std::io::stdin().take(65537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, "request_too_large");
    serde_json::from_slice(&bytes).context("invalid_input_json")
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let cli = Cli::parse();
    let result = run(cli).await;
    match result {
        Ok(Some(value)) => println!("{}", value),
        Ok(None) => {}
        Err(error) => {
            println!(
                "{}",
                json!({"error":match error.to_string().as_str(){"engine_unavailable"=>"engine_unavailable","daemon_already_running"=>"daemon_already_running","request_too_large"=>"request_too_large","stdin_json_required"=>"stdin_json_required","invalid_input_json"=>"invalid_input_json","installation_failed_rolled_back"=>"installation_failed_rolled_back","installation_failed_rollback_required"=>"installation_failed_rollback_required","unowned_installation_path"=>"unowned_installation_path","git_managed_plugin_requires_backend_only"=>"git_managed_plugin_requires_backend_only",_=>omarchy_photo_wallpapers::engine::public_error(&error)}})
            );
            std::process::exit(1);
        }
    }
}
async fn run(cli: Cli) -> Result<Option<Value>> {
    if matches!(cli.command, Command::PrivacyWorker) {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 2 * 1024 * 1024, "privacy_worker_input_limit");
        let input = serde_json::from_slice(&bytes).context("invalid_privacy_worker_input")?;
        return Ok(Some(serde_json::to_value(
            omarchy_photo_wallpapers::privacy::inference::screen(input)?,
        )?));
    }

    let paths = if let Some(profile) = cli
        .profile
        .or_else(|| std::env::var_os("PHOTO_WALLPAPERS_PROFILE").map(PathBuf::from))
    {
        Paths::under(&profile)
    } else {
        Paths::system()?
    };
    if let Command::Daemon { no_network } = cli.command {
        let _lock = platform::lock(&paths)?;
        let store = Store::open(&paths)?;
        let listener = ipc::bind(&paths)?;
        let engine = Engine::start(
            store,
            Arc::new(Omarchy),
            Arc::new(NativePipeline::default()),
            !no_network,
        )
        .await?;
        let stop = CancellationToken::new();
        platform::monitor_geometry(engine.clone(), stop.clone());
        let server = tokio::spawn(ipc::serve(listener, engine.clone(), stop.clone()));
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await?;
        engine.shutdown().await;
        stop.cancel();
        server.await??;
        let _ = std::fs::remove_file(paths.socket());
        return Ok(None);
    }
    if let Command::Install {
        stage,
        backend_only,
    } = cli.command
    {
        let home = dirs::home_dir().context("home_unavailable")?;
        let installer = omarchy_photo_wallpapers::installer::Installer::new(
            &home,
            &paths,
            Arc::new(omarchy_photo_wallpapers::installer::SystemRunner),
        );
        let prepared = installer.stage(&std::env::current_exe()?)?;
        return Ok(Some(if stage {
            json!({"staged":true,"path":prepared})
        } else if backend_only {
            installer.activate_backend(&prepared)?
        } else {
            installer.activate(&prepared)?
        }));
    }
    if let Command::Uninstall { purge } = cli.command {
        let home = dirs::home_dir().context("home_unavailable")?;
        let installer = omarchy_photo_wallpapers::installer::Installer::new(
            &home,
            &paths,
            Arc::new(omarchy_photo_wallpapers::installer::SystemRunner),
        );
        return Ok(Some(installer.remove(purge)?));
    }
    if let Command::AdoptSession {
        from_config,
        from_data,
        from_runtime,
    } = cli.command
    {
        return Ok(Some(omarchy_photo_wallpapers::migration::adopt_session(
            &Paths {
                config: from_config,
                data: from_data,
                runtime: from_runtime,
            },
            &paths,
        )?));
    }
    if let Command::AdoptCatalog {
        from_config,
        from_data,
        from_runtime,
    } = cli.command
    {
        return Ok(Some(omarchy_photo_wallpapers::migration::adopt_catalog(
            &Paths {
                config: from_config,
                data: from_data,
                runtime: from_runtime,
            },
            &paths,
        )?));
    }
    let mut request = Request {
        id: uuid::Uuid::new_v4().to_string(),
        ..Request::default()
    };
    let mut auth_status = false;
    let health = matches!(cli.command, Command::Health);
    match cli.command {
        Command::Status | Command::Health => request.command = "status".into(),
        Command::Sources => request.command = "sources".into(),
        Command::Models { operation } => {
            request.command = "models".into();
            request.operation = operation;
        }
        Command::Meeting { operation } => {
            request.command = "meeting".into();
            request.operation = operation;
        }
        Command::Config {
            operation,
            expected_revision,
            stdin_json,
        } => {
            ensure!(
                ["get", "apply"].contains(&operation.as_str()),
                "invalid_operation"
            );
            ensure!(operation != "apply" || stdin_json, "stdin_json_required");
            request.command = "config".into();
            request.operation = operation;
            request.expected_revision = expected_revision;
            request.input = input(stdin_json)?;
        }
        Command::Auth {
            operation,
            stdin_json,
        } => {
            auth_status = operation == "status";
            ensure!(
                !["login", "mfa"].contains(&operation.as_str()) || stdin_json,
                "stdin_json_required"
            );
            request.command = if auth_status { "status" } else { "auth" }.into();
            request.operation = operation;
            request.input = input(stdin_json)?;
        }
        Command::Pair { stdin_json } => {
            ensure!(stdin_json, "stdin_json_required");
            request.command = "pair".into();
            request.input = input(true)?;
        }
        Command::Next { request_id } => {
            request.command = "next".into();
            if let Some(id) = request_id {
                request.id = id;
            }
        }
        Command::Previous { request_id } => {
            request.command = "previous".into();
            if let Some(id) = request_id {
                request.id = id;
            }
        }
        Command::Refresh { request_id } => {
            request.command = "refresh".into();
            if let Some(id) = request_id {
                request.id = id;
            }
        }
        Command::Pause => request.command = "pause".into(),
        Command::Resume => request.command = "resume".into(),
        Command::Pin => request.command = "pin".into(),
        Command::Unpin => request.command = "unpin".into(),
        Command::Feedback {
            action,
            asset_id,
            stdin_json,
        } => {
            request.command = "feedback".into();
            request.input = if stdin_json { input(true)? } else { json!({}) };
            request.input["action"] = json!(action);
            if let Some(id) = asset_id {
                request.input["asset_id"] = json!(id);
            }
        }
        Command::PrivacyWorker
        | Command::Daemon { .. }
        | Command::Install { .. }
        | Command::Uninstall { .. }
        | Command::AdoptSession { .. }
        | Command::AdoptCatalog { .. } => {
            unreachable!()
        }
    }
    let result = if health {
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            ipc::call(&paths, request),
        )
        .await
        .context("engine_unavailable")??
    } else {
        ipc::call(&paths, request).await?
    };
    if health {
        ensure!(
            result["daemon_running"] == true
                && result["backend_version"] == env!("CARGO_PKG_VERSION")
                && result["protocol_version"] == ipc::PROTOCOL_VERSION,
            "engine_unavailable"
        );
        return Ok(Some(
            json!({"daemon_running":true,"backend_version":env!("CARGO_PKG_VERSION"),"protocol_version":ipc::PROTOCOL_VERSION}),
        ));
    }
    if result.get("error").is_some_and(|v| !v.is_null()) {
        println!("{}", result);
        std::process::exit(1);
    }
    Ok(Some(if auth_status {
        result["auth"].clone()
    } else {
        result
    }))
}
