use anyhow::Result;
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration as StdDuration, Instant};
use tokio::time::Duration;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;
use zbus::{dbus_interface, ConnectionBuilder};

/// A cold eval of a large flake fetches inputs and walks the whole module tree,
/// so this is generous, but it is still a ceiling rather than no ceiling.
const EVAL_TIMEOUT: StdDuration = StdDuration::from_secs(300);

const SETTINGS_FILE: &str = "/etc/simple-nix-update-gui/settings.env";

const DAEMON_PATH: &str = "/org/simple_nix_update_gui/Daemon";

/// Keeps the progress buffer bounded when a noisy eval runs right up to the
/// 300s timeout.
const MAX_PROGRESS_LINES: usize = 500;

#[derive(Parser, Debug, Clone)]
#[command(name = "simple-nix-update-gui-daemon")]
#[command(about = "Notification daemon for simple-nix-update-gui")]
struct Args {
    #[arg(long, env = "SNU_FLAKE_URI")]
    flake_uri: Option<String>,
    #[arg(long, env = "SNU_SYSTEM_NAME")]
    system_name: Option<String>,
    #[arg(long, env = "SNU_CHECK_INTERVAL")]
    check_interval: Option<String>,
    #[arg(long, env = "SNU_BUS_NAME")]
    bus_name: Option<String>,
}

/// Same resolution order as the GUI: flag, then env, then the settings file the
/// NixOS module writes, then builtin defaults. Every value logs its source.
#[derive(Debug, Clone)]
struct Settings {
    flake_uri: String,
    system_name: Option<String>,
    check_interval: String,
    bus_name: String,
}

fn build_settings(args: &Args) -> anyhow::Result<Settings> {
    let file = load_settings_file();

    let (flake_uri, flake_uri_source) = resolve_str(&args.flake_uri, "SNU_FLAKE_URI", &file, "");
    if flake_uri.is_empty() {
        anyhow::bail!(
            "missing flake URI: pass --flake-uri, set SNU_FLAKE_URI, or create {SETTINGS_FILE}"
        );
    }
    info!(setting = "flake_uri", value = %flake_uri, source = flake_uri_source);

    let (system_name, system_name_source) =
        resolve_str(&args.system_name, "SNU_SYSTEM_NAME", &file, "");
    let system_name = if system_name.is_empty() {
        None
    } else {
        Some(system_name)
    };
    info!(setting = "system_name", value = ?system_name, source = system_name_source);

    let (check_interval, check_interval_source) =
        resolve_str(&args.check_interval, "SNU_CHECK_INTERVAL", &file, "1h");
    info!(setting = "check_interval", value = %check_interval, source = check_interval_source);

    let (bus_name, bus_name_source) = resolve_str(
        &args.bus_name,
        "SNU_BUS_NAME",
        &file,
        "org.simple_nix_update_gui.Daemon",
    );
    info!(setting = "bus_name", value = %bus_name, source = bus_name_source);

    Ok(Settings {
        flake_uri,
        system_name,
        check_interval,
        bus_name,
    })
}

fn resolve_str(
    flag_env: &Option<String>,
    env_key: &str,
    file: &HashMap<String, String>,
    default: &str,
) -> (String, &'static str) {
    if let Some(value) = flag_env {
        let source = if std::env::var(env_key).is_ok() {
            "env"
        } else {
            "flag"
        };
        (value.clone(), source)
    } else if let Some(value) = file.get(env_key) {
        (value.clone(), "settings file")
    } else {
        (default.to_string(), "builtin default")
    }
}

fn load_settings_file() -> HashMap<String, String> {
    let mut map = HashMap::new();
    match std::fs::read_to_string(SETTINGS_FILE) {
        Ok(content) => {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    map.insert(key.trim().to_string(), value.trim().to_string());
                }
            }
            info!(
                path = SETTINGS_FILE,
                entries = map.len(),
                "loaded settings file"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            info!(path = SETTINGS_FILE, "settings file not present");
        }
        Err(error) => {
            warn!(path = SETTINGS_FILE, error = %error, "could not read settings file");
        }
    }
    map
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
    /// True while an eval runs, so the GUI can show the check in progress.
    /// Absent when talking to a daemon that predates the field.
    #[serde(default)]
    checking: bool,
}

#[derive(Clone)]
struct Daemon {
    state: std::sync::Arc<std::sync::RwLock<UpdateState>>,
    settings: Settings,
    /// The stderr lines of the running check, so a GUI that attaches late can
    /// catch up through GetProgress.
    progress: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[dbus_interface(name = "org.simple_nix_update_gui.Daemon")]
impl Daemon {
    /// Starts a check on a background thread and returns the current state
    /// immediately. The caller watches `checking` instead of blocking for the
    /// whole eval, which also keeps the executor free to serve GetState. A
    /// check that is already running is left alone.
    async fn check_for_updates(
        &self,
        #[zbus(signal_context)] ctxt: zbus::SignalContext<'_>,
    ) -> String {
        info!("Manual update check triggered");
        if self.try_begin_check() {
            let daemon = self.clone();
            let ctxt = ctxt.to_owned();
            std::thread::spawn(move || daemon.run_check(Some(ctxt), true));
        } else {
            info!("a check is already running, returning the current state");
        }
        self.state_json()
    }

    fn get_state(&self) -> String {
        debug!("GetState request served");
        self.state_json()
    }

    /// The stderr lines of the running or last check, as a JSON array.
    fn get_progress(&self) -> String {
        let progress = self.progress.lock().unwrap();
        serde_json::to_string(&*progress).unwrap_or_else(|_| "[]".to_string())
    }

    async fn is_reboot_needed(&self) -> bool {
        is_reboot_needed().await.unwrap_or(false)
    }

    /// One line of the eval's stderr, pushed while the check runs.
    #[dbus_interface(signal)]
    async fn check_progress(ctxt: &zbus::SignalContext<'_>, line: &str) -> zbus::Result<()>;
}

impl Daemon {
    fn state_json(&self) -> String {
        let state = self.state.read().unwrap();
        serde_json::to_string(&*state).unwrap_or_else(|_| "{}".to_string())
    }

    /// Marks a check as started unless one is already running. The buffer is
    /// cleared only when a check actually begins, so an already running check
    /// keeps the lines it has produced.
    fn try_begin_check(&self) -> bool {
        let mut state = self.state.write().unwrap();
        if state.checking {
            return false;
        }
        state.checking = true;
        self.progress.lock().unwrap().clear();
        true
    }

    /// Blocks until the eval finishes. Runs on a plain thread: emitting the
    /// progress signals needs a block_on, which would panic on the zbus
    /// executor and on the tokio runtime.
    fn run_check(&self, ctxt: Option<zbus::SignalContext<'static>>, surface_errors: bool) {
        let progress = self.progress.clone();
        let outcome = check_updates(&self.settings, |line| {
            {
                let mut buffer = progress.lock().unwrap();
                buffer.push(line.clone());
                if buffer.len() > MAX_PROGRESS_LINES {
                    buffer.remove(0);
                }
            }
            if let Some(ctxt) = &ctxt {
                if let Err(error) = zbus::block_on(Self::check_progress(ctxt, &line)) {
                    debug!(%error, "could not emit progress signal");
                }
            }
        });
        let mut state = self.state.write().unwrap();
        state.checking = false;
        match outcome {
            Ok(new_state) => *state = new_state,
            Err(error) => {
                error!("Update check failed: {}", error);
                // Periodic and startup failures only get logged, as before;
                // manual checks record the reason for the GUI.
                if surface_errors {
                    state.has_update = false;
                    state.remote_system = None;
                    state.last_error = Some(error.to_string());
                    state.last_check = chrono::Local::now().to_rfc3339();
                }
            }
        }
    }
}

fn check_signal_context(conn: &zbus::Connection) -> Option<zbus::SignalContext<'static>> {
    match zbus::SignalContext::new(conn, DAEMON_PATH) {
        Ok(ctxt) => Some(ctxt.into_owned()),
        Err(error) => {
            warn!(%error, "could not create progress signal context");
            None
        }
    }
}

fn get_current_system() -> Result<String> {
    // /run/booted-system is frozen at boot and ignores `nixos-rebuild switch`,
    // so the active system, the one nixos-rebuild just activated, is read here.
    let output = Command::new("readlink")
        .arg("/run/current-system")
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

fn get_hostname() -> String {
    hostname::get()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

fn check_updates(settings: &Settings, mut on_line: impl FnMut(String)) -> Result<UpdateState> {
    let current_system = get_current_system()?;
    let system_name = match settings.system_name.clone() {
        Some(name) => name,
        None => get_hostname(),
    };

    info!(
        "Checking for updates: flake={}#{}",
        settings.flake_uri, system_name
    );

    // lib.nixosSystem has no `system` attribute. The built system path lives at
    // config.system.build.toplevel, which is what /run/current-system points at.
    let eval_cmd = format!(
        "{}#nixosConfigurations.{}.config.system.build.toplevel",
        settings.flake_uri.trim_end_matches('/'),
        system_name
    );

    let (has_update, remote_system, last_error) = match eval_remote_system(eval_cmd, &mut on_line)
    {
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
        flake_uri: settings.flake_uri.clone(),
        system_name,
        checking: false,
    })
}

/// Runs the eval the check hinges on and returns the remote system store path,
/// or the reason it could not be had. Every stderr line is handed to `on_line`
/// as it arrives, so a caller can show the eval while it runs. Lossy decoding
/// keeps a stray non UTF-8 byte in nix's output from hiding the message that
/// matters.
///
/// This stays on std rather than tokio on purpose. zbus dispatches method calls
/// on its own executor, not on the runtime #[tokio::main] builds, so a tokio
/// timer or spawn_blocking here panics with "no reactor running" whenever the
/// check is entered over D-Bus.
fn eval_remote_system(
    eval_cmd: String,
    mut on_line: impl FnMut(String),
) -> Result<String, String> {
    let (result_tx, result_rx) =
        mpsc::channel::<Result<(ExitStatus, Vec<u8>, String), String>>();
    let (line_tx, line_rx) = mpsc::channel::<String>();

    std::thread::spawn(move || {
        let mut child = match Command::new("nix")
            .arg("eval")
            .arg("--no-write-lock-file")
            // An unlocked git+ URL is re-resolved only once per tarball-ttl
            // (3600s by default), so a fresh push would stay invisible to the
            // check for up to an hour without this.
            .arg("--option")
            .arg("tarball-ttl")
            .arg("0")
            .arg("--raw")
            .arg(eval_cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                let _ = result_tx.send(Err(format!("could not run nix eval: {error}")));
                return;
            }
        };

        // stdout holds only the resulting store path, so reading stderr to
        // completion first cannot deadlock on a full stdout pipe.
        let mut stderr_lines = Vec::new();
        if let Some(stderr) = child.stderr.take() {
            for line in BufReader::new(stderr).lines() {
                match line {
                    Ok(line) => {
                        let _ = line_tx.send(line.clone());
                        stderr_lines.push(line);
                    }
                    Err(error) => {
                        stderr_lines.push(format!("unreadable stderr: {error}"));
                        break;
                    }
                }
            }
        }
        let mut stdout = Vec::new();
        if let Some(mut piped) = child.stdout.take() {
            let _ = piped.read_to_end(&mut stdout);
        }
        match child.wait() {
            Ok(status) => {
                let _ = result_tx.send(Ok((status, stdout, stderr_lines.join("\n"))));
            }
            Err(error) => {
                let _ = result_tx.send(Err(format!("waiting for nix eval: {error}")));
            }
        }
    });

    let deadline = Instant::now() + EVAL_TIMEOUT;
    loop {
        match result_rx.recv_timeout(StdDuration::from_millis(50)) {
            Ok(Ok((status, stdout, stderr))) => {
                drain_progress(&line_rx, &mut on_line);
                return if status.success() {
                    Ok(text(&stdout))
                } else {
                    Err(text(stderr.as_bytes()))
                };
            }
            Ok(Err(error)) => {
                drain_progress(&line_rx, &mut on_line);
                return Err(error);
            }
            Err(RecvTimeoutError::Timeout) => {
                drain_progress(&line_rx, &mut on_line);
                if Instant::now() >= deadline {
                    return Err(format!("nix eval did not finish within {EVAL_TIMEOUT:?}"));
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                drain_progress(&line_rx, &mut on_line);
                return Err("nix eval thread ended without a result".to_string());
            }
        }
    }
}

fn drain_progress(line_rx: &mpsc::Receiver<String>, on_line: &mut impl FnMut(String)) {
    while let Ok(line) = line_rx.try_recv() {
        on_line(line);
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

    std::panic::set_hook(Box::new(|info| {
        error!("panic: {info}");
        error!(
            backtrace = %std::backtrace::Backtrace::force_capture(),
            "panic backtrace"
        );
    }));

    let args = Args::parse();
    let settings = build_settings(&args)?;

    // The name is taken before the first nix eval. Under Type=dbus systemd waits
    // for the name, and a cold eval of a large flake can outlast that wait.
    let daemon = Daemon {
        state: std::sync::Arc::new(std::sync::RwLock::new(UpdateState {
            has_update: false,
            current_system: "unknown".to_string(),
            remote_system: None,
            last_error: None,
            last_check: chrono::Local::now().to_rfc3339(),
            flake_uri: settings.flake_uri.clone(),
            system_name: settings
                .system_name
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            checking: false,
        })),
        settings: settings.clone(),
        progress: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    };

    // Session bus, not system bus. The daemon runs as the logged in user in a
    // systemd user unit, and needs their HOME so git can find their credentials.
    let conn = ConnectionBuilder::session()?
        .name(settings.bus_name.clone())?
        .serve_at(DAEMON_PATH, daemon.clone())?
        .build()
        .await?;
    info!(bus_name = %settings.bus_name, "daemon owns bus name");

    // Checks run on plain threads: run_check emits progress signals, which
    // needs a block_on that would panic on both the zbus executor and here.
    let startup_daemon = daemon.clone();
    let startup_ctxt = check_signal_context(&conn);
    std::thread::spawn(move || {
        if startup_daemon.try_begin_check() {
            startup_daemon.run_check(startup_ctxt, false);
        }
    });

    // Periodic checks
    let duration = parse_duration(&settings.check_interval).unwrap_or(Duration::from_secs(3600));
    let mut check_interval = tokio::time::interval(duration);
    info!(interval = ?duration, "periodic checks scheduled");

    loop {
        check_interval.tick().await;
        info!("Periodic update check");
        if daemon.try_begin_check() {
            let ctxt = check_signal_context(&conn);
            let daemon = daemon.clone();
            std::thread::spawn(move || daemon.run_check(ctxt, false));
        } else {
            info!("a check is already running, skipping this tick");
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
