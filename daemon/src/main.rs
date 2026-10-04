use anyhow::Result;
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::env;
use std::process::Command;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};
use zbus::{dbus_interface, ConnectionBuilder};

#[derive(Parser, Debug, Clone)]
#[command(name = "simple-nix-update-gui-daemon")]
#[command(about = "Notification daemon for simple-nix-update-gui")]
struct Args {
    #[arg(long, env = "SNU_FLAKE_URI")]
    flake_uri: String,
    #[arg(long, env = "SNU_SYSTEM_NAME")]
    system_name: Option<String>,
    #[arg(long, env = "SNU_CHECK_INTERVAL", default_value = "1h")]
    check_interval: String,
    #[arg(long, env = "SNU_AUTO_NOTIFY", env = "SNU_AUTO_NOTIFY", default_value = "true")]
    auto_notify: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateState {
    has_update: bool,
    current_system: String,
    remote_system: Option<String>,
    last_check: String,
    flake_uri: String,
    system_name: String,
}

#[derive(Clone)]
struct Daemon {
    state: std::sync::Arc<tokio::sync::RwLock<UpdateState>>,
    args: Args,
}

#[dbus_interface(name = "org.simple_nix_update_gui.Daemon")]
impl Daemon {
    async fn check_for_updates(&self) -> String {
        info!("Manual update check triggered");
        match check_updates(&self.args).await {
            Ok(new_state) => {
                *self.state.write().await = new_state.clone();
                if new_state.has_update && self.args.auto_notify {
                    let _ = notify_update_available(&new_state);
                }
                serde_json::to_string(&new_state).unwrap_or_else(|_| "{}".to_string())
            }
            Err(e) => {
                error!("Update check failed: {}", e);
                format!("{{\"error\": \"{}\"}}", e)
            }
        }
    }

    async fn get_state(&self) -> String {
        let state = self.state.read().await.clone();
        serde_json::to_string(&state).unwrap_or_else(|_| "{}".to_string())
    }

    async fn is_reboot_needed(&self) -> bool {
        is_reboot_needed().await.unwrap_or(false)
    }
}

async fn get_current_system() -> Result<String> {
    let output = Command::new("readlink")
        .arg("/run/booted-system")
        .output()?;
    if !output.status.success() {
        // Fallback to profile
        let output = Command::new("readlink")
            .arg("-f")
            .arg("/nix/var/nix/profiles/system")
            .output()?;
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    } else {
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }
}

async fn get_hostname() -> String {
    hostname::get()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

async fn check_updates(args: &Args) -> Result<UpdateState> {
    let current_system = get_current_system().await?;
    let system_name = match args.system_name.clone() {
        Some(name) => name,
        None => get_hostname().await,
    };

    info!("Checking for updates: flake={}#{}", args.flake_uri, system_name);

    let eval_cmd = format!(
        "{}#nixosConfigurations.{}.system",
        args.flake_uri.trim_end_matches('/'),
        system_name
    );

    let output = Command::new("nix")
        .arg("eval")
        .arg("--no-write-lock-file")
        .arg("--raw")
        .arg(eval_cmd)
        .output();

    let (has_update, remote_system) = match output {
        Ok(out) if out.status.success() => {
            let remote = String::from_utf8(out.stdout)?.trim().to_string();
            let has_update = remote != current_system;
            info!(
                "Update check result: has_update={}, current={}, remote={}",
                has_update, current_system, remote
            );
            (has_update, Some(remote))
        }
        Ok(out) => {
            let stderr = String::from_utf8(out.stderr)?;
            warn!("nix eval failed: {}", stderr);
            (false, None)
        }
        Err(e) => {
            warn!("Failed to run nix eval: {}", e);
            (false, None)
        }
    };

    Ok(UpdateState {
        has_update,
        current_system,
        remote_system,
        last_check: chrono::Local::now().to_rfc3339(),
        flake_uri: args.flake_uri.clone(),
        system_name,
    })
}

async fn notify_update_available(state: &UpdateState) -> Result<()> {
    let msg = format!(
        "Update available for {} ({})",
        state.system_name, state.flake_uri
    );
    let _ = notify_rust::Notification::new()
        .summary("System Updates")
        .body(&msg)
        .action("show", "Show Updates")
        .icon("system-software-update")
        .timeout(notify_rust::Timeout::Milliseconds(10000))
        .show();
    Ok(())
}

async fn is_reboot_needed() -> Result<bool> {
    let booted = Command::new("readlink")
        .arg("/run/booted-system")
        .output()?;
    let profile = Command::new("readlink")
        .arg("-f")
        .arg("/nix/var/nix/profiles/system")
        .output()?;
    if !booted.status.success() || !profile.status.success() {
        return Ok(false);
    }
    let booted_str = String::from_utf8(booted.stdout)?.trim().to_string();
    let profile_str = String::from_utf8(profile.stdout)?.trim().to_string();
    Ok(booted_str != profile_str)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let initial_state = check_updates(&args).await.unwrap_or_else(|e| {
        error!("Initial check failed: {}", e);
        UpdateState {
            has_update: false,
            current_system: "unknown".to_string(),
            remote_system: None,
            last_check: chrono::Local::now().to_rfc3339(),
            flake_uri: args.flake_uri.clone(),
            system_name: args.system_name.clone().unwrap_or_else(|| "unknown".to_string()),
        }
    });

    if initial_state.has_update && args.auto_notify {
        let _ = notify_update_available(&initial_state);
    }

    let daemon = Daemon {
        state: std::sync::Arc::new(tokio::sync::RwLock::new(initial_state)),
        args: args.clone(),
    };

    let _conn = ConnectionBuilder::session()?
        .name("org.simple_nix_update_gui.Daemon")?
        .serve_at("/org/simple_nix_update_gui/Daemon", daemon.clone())?
        .build()
        .await?;

    // Periodic checks
    let mut interval_str = args.check_interval.clone();
    let duration = parse_duration(&mut interval_str).unwrap_or(Duration::from_secs(3600));
    let mut check_interval = tokio::time::interval(duration);

    loop {
        check_interval.tick().await;
        info!("Periodic update check");
        match check_updates(&args).await {
            Ok(new_state) => {
                let mut state = daemon.state.write().await;
                let had_update = state.has_update;
                *state = new_state.clone();
                if new_state.has_update && !had_update && args.auto_notify {
                    let _ = notify_update_available(&new_state);
                }
            }
            Err(e) => {
                error!("Periodic check failed: {}", e);
            }
        }
    }
}

fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let (num_part, unit_part) = bytes
        .iter()
        .position(|&b| b < b'0' || b > b'9')
        .map(|pos| s.split_at(pos))
        .unwrap_or((s, ""));
    let num: u64 = num_part.trim().parse().ok()?;
    match unit_part.trim().to_lowercase().as_str() {
        "s" | "sec" | "second" | "seconds" => Some(Duration::from_secs(num)),
        "m" | "min" | "minute" | "minutes" => Some(Duration::from_secs(num * 60)),
        "h" | "hour" | "hours" => Some(Duration::from_secs(num * 3600)),
        "d" | "day" | "days" => Some(Duration::from_secs(num * 86400)),
        _ => Some(Duration::from_secs(num)), // default to seconds if unknown
    }
}
