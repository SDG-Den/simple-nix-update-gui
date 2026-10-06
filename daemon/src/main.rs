use anyhow::Result;
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration as StdDuration;
use tokio::time::Duration;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use zbus::{dbus_interface, ConnectionBuilder};

/// A cold eval of a large flake fetches inputs and walks the whole module tree,
/// so this is generous, but it is still a ceiling rather than no ceiling.
const EVAL_TIMEOUT: StdDuration = StdDuration::from_secs(300);

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
    #[arg(
        long,
        env = "SNU_BUS_NAME",
        default_value = "org.simple_nix_update_gui.Daemon"
    )]
    bus_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateState {
    has_update: bool,
    current_system: String,
    remote_system: Option<String>,
    /// Why the last check produced no remote system, kept so the GUI can say
    /// what went wrong instead of showing an unexplained placeholder.
    #[serde(default)]
    last_error: Option<String>,
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
                serde_json::to_string(&new_state).unwrap_or_else(|_| "{}".to_string())
            }
            Err(e) => {
                error!("Update check failed: {}", e);
                let mut failed = self.state.read().await.clone();
                failed.has_update = false;
                failed.remote_system = None;
                failed.last_error = Some(e.to_string());
                failed.last_check = chrono::Local::now().to_rfc3339();
                *self.state.write().await = failed.clone();
                serde_json::to_string(&failed).unwrap_or_else(|_| "{}".to_string())
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

    info!(
        "Checking for updates: flake={}#{}",
        args.flake_uri, system_name
    );

    // lib.nixosSystem has no `system` attribute. The built system path lives at
    // config.system.build.toplevel, which is what /run/booted-system points at.
    let eval_cmd = format!(
        "{}#nixosConfigurations.{}.config.system.build.toplevel",
        args.flake_uri.trim_end_matches('/'),
        system_name
    );

    let (has_update, remote_system, last_error) = match eval_remote_system(eval_cmd) {
        Ok(remote) => {
            let has_update = remote != current_system;
            info!(
                "Update check result: has_update={}, current={}, remote={}",
                has_update, current_system, remote
            );
            (has_update, Some(remote), None)
        }
        Err(message) => {
            warn!("nix eval failed: {}", message);
            (false, None, Some(message))
        }
    };

    Ok(UpdateState {
        has_update,
        current_system,
        remote_system,
        last_error,
        last_check: chrono::Local::now().to_rfc3339(),
        flake_uri: args.flake_uri.clone(),
        system_name,
    })
}

/// Runs the eval the check hinges on and returns the remote system store path,
/// or the reason it could not be had. Lossy decoding keeps a stray non UTF-8
/// byte in nix's output from hiding the message that matters.
///
/// This stays on std rather than tokio on purpose. zbus dispatches method calls
/// on its own executor, not on the runtime #[tokio::main] builds, so a tokio
/// timer or spawn_blocking here panics with "no reactor running" whenever the
/// check is entered over D-Bus.
fn eval_remote_system(eval_cmd: String) -> Result<String, String> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = Command::new("nix")
            .arg("eval")
            .arg("--no-write-lock-file")
            .arg("--raw")
            .arg(eval_cmd)
            .output();
        let _ = sender.send(outcome);
    });

    let output = match receiver.recv_timeout(EVAL_TIMEOUT) {
        Ok(outcome) => outcome.map_err(|e| format!("could not run nix eval: {e}"))?,
        Err(RecvTimeoutError::Timeout) => {
            return Err(format!("nix eval did not finish within {EVAL_TIMEOUT:?}"));
        }
        Err(RecvTimeoutError::Disconnected) => {
            return Err("nix eval thread ended without a result".to_string());
        }
    };

    if output.status.success() {
        Ok(text(&output.stdout))
    } else {
        Err(text(&output.stderr))
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
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
    // fmt::init would read RUST_LOG and, when that is unset, install a filter
    // that only lets ERROR through, which silences every reason a check failed.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    // The name is taken before the first nix eval. Under Type=dbus systemd waits
    // for the name, and a cold eval of a large flake can outlast that wait.
    let daemon = Daemon {
        state: std::sync::Arc::new(tokio::sync::RwLock::new(UpdateState {
            has_update: false,
            current_system: "unknown".to_string(),
            remote_system: None,
            last_error: None,
            last_check: chrono::Local::now().to_rfc3339(),
            flake_uri: args.flake_uri.clone(),
            system_name: args
                .system_name
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        })),
        args: args.clone(),
    };

    // Session bus, not system bus. The daemon runs as the logged in user in a
    // systemd user unit, and needs their HOME so git can find their credentials.
    let _conn = ConnectionBuilder::session()?
        .name(args.bus_name.clone())?
        .serve_at("/org/simple_nix_update_gui/Daemon", daemon.clone())?
        .build()
        .await?;

    let initial_state = match check_updates(&args).await {
        Ok(state) => state,
        Err(e) => {
            error!("Initial check failed: {}", e);
            daemon.state.read().await.clone()
        }
    };
    *daemon.state.write().await = initial_state;

    // Periodic checks
    let duration = parse_duration(&args.check_interval).unwrap_or(Duration::from_secs(3600));
    let mut check_interval = tokio::time::interval(duration);

    loop {
        check_interval.tick().await;
        info!("Periodic update check");
        match check_updates(&args).await {
            Ok(new_state) => {
                *daemon.state.write().await = new_state;
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
        .position(|&b| !b.is_ascii_digit())
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
