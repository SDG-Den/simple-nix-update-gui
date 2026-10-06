use anyhow::Result;
use clap::{ArgAction, Parser};
use gtk4::{self, glib, Box as GtkBox, Button, Dialog, Label, Orientation, ScrolledWindow};
use ksni::{Category, Status, TrayMethods};
use libadwaita::prelude::*;
use libadwaita::{
    Application as AdwApplication, ApplicationWindow, Banner, HeaderBar, ToolbarView,
};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use vte4::prelude::*;
use vte4::{PtyFlags, Terminal};

const ICON_NAME: &str = "system-software-update";

const SETTINGS_FILE: &str = "/etc/simple-nix-update-gui/settings.env";

#[derive(Parser, Debug, Clone)]
#[command(name = "simple-nix-update-gui")]
#[command(about = "Simple Nix update GUI")]
struct Args {
    #[arg(long, env = "SNU_FLAKE_URI")]
    flake_uri: Option<String>,
    #[arg(long, env = "SNU_SYSTEM_NAME")]
    system_name: Option<String>,
    #[arg(
        long,
        env = "SNU_USE_NOM",
        action = ArgAction::Set,
        value_parser = clap::value_parser!(bool)
    )]
    use_nom: Option<bool>,
    #[arg(long, env = "SNU_CHECK_INTERVAL")]
    check_interval: Option<String>,
    #[arg(
        long,
        env = "SNU_AUTO_NOTIFY",
        action = ArgAction::Set,
        value_parser = clap::value_parser!(bool)
    )]
    auto_notify: Option<bool>,
    #[arg(long, env = "SNU_BUS_NAME")]
    bus_name: Option<String>,
    #[arg(long)]
    tray: bool,
}

/// The values the rest of the program runs on, resolved from flag, then
/// environment, then the settings file the NixOS module writes, then builtin
/// defaults. Logging the source of every value makes a misconfiguration show
/// up in the first lines of output instead of as a wrong label in the UI.
#[derive(Debug, Clone)]
struct Settings {
    flake_uri: String,
    system_name: String,
    check_interval: String,
    bus_name: String,
    use_nom: bool,
    auto_notify: bool,
    tray: bool,
}

fn build_settings(args: &Args) -> Settings {
    let file = load_settings_file();

    let (flake_uri, flake_uri_source) =
        resolve_str(&args.flake_uri, "SNU_FLAKE_URI", &file, "path:/etc/nixos");
    tracing::info!(setting = "flake_uri", value = %flake_uri, source = flake_uri_source);

    let (system_name, system_name_source) =
        resolve_str(&args.system_name, "SNU_SYSTEM_NAME", &file, "");
    let (system_name, system_name_source) = if system_name.is_empty() {
        (get_hostname(), "builtin default")
    } else {
        (system_name, system_name_source)
    };
    tracing::info!(setting = "system_name", value = %system_name, source = system_name_source);

    let (check_interval, check_interval_source) =
        resolve_str(&args.check_interval, "SNU_CHECK_INTERVAL", &file, "1h");
    tracing::info!(setting = "check_interval", value = %check_interval, source = check_interval_source);

    let (bus_name, bus_name_source) = resolve_str(
        &args.bus_name,
        "SNU_BUS_NAME",
        &file,
        "org.simple_nix_update_gui.Daemon",
    );
    tracing::info!(setting = "bus_name", value = %bus_name, source = bus_name_source);

    let (use_nom, use_nom_source) = resolve_bool(&args.use_nom, "SNU_USE_NOM", &file, true);
    tracing::info!(
        setting = "use_nom",
        value = use_nom,
        source = use_nom_source
    );

    let (auto_notify, auto_notify_source) =
        resolve_bool(&args.auto_notify, "SNU_AUTO_NOTIFY", &file, true);
    tracing::info!(
        setting = "auto_notify",
        value = auto_notify,
        source = auto_notify_source
    );

    let tray = args.tray;
    tracing::info!(
        setting = "tray",
        value = tray,
        source = if tray { "flag" } else { "builtin default" }
    );

    Settings {
        flake_uri,
        system_name,
        check_interval,
        bus_name,
        use_nom,
        auto_notify,
        tray,
    }
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

fn resolve_bool(
    flag_env: &Option<bool>,
    env_key: &str,
    file: &HashMap<String, String>,
    default: bool,
) -> (bool, &'static str) {
    if let Some(value) = flag_env {
        let source = if std::env::var(env_key).is_ok() {
            "env"
        } else {
            "flag"
        };
        (*value, source)
    } else if let Some(raw) = file.get(env_key) {
        match parse_bool(raw) {
            Some(value) => (value, "settings file"),
            None => {
                tracing::warn!(env_key, value = %raw, "invalid bool in settings file, using default");
                (default, "builtin default")
            }
        }
    } else {
        (default, "builtin default")
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
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
            tracing::info!(
                path = SETTINGS_FILE,
                entries = map.len(),
                "loaded settings file"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(path = SETTINGS_FILE, "settings file not present");
        }
        Err(error) => {
            tracing::warn!(path = SETTINGS_FILE, error = %error, "could not read settings file");
        }
    }
    map
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateState {
    has_update: bool,
    current_system: String,
    remote_system: Option<String>,
    /// Absent when talking to a daemon that predates the field, which is why it
    /// defaults instead of being required.
    #[serde(default)]
    last_error: Option<String>,
    last_check: String,
    flake_uri: String,
    system_name: String,
}

/// What background tasks ask the GTK thread to do. The GTK thread owns every
/// widget and the background tasks own none, so this channel is the only thing
/// that crosses between them.
enum UiAction {
    State(Box<UpdateState>),
    DaemonDown(String),
    OpenWindow,
    CheckNow,
    Quit,
}

struct UpdateTray {
    has_update: Arc<AtomicBool>,
    actions: UnboundedSender<UiAction>,
}

impl ksni::Tray for UpdateTray {
    fn id(&self) -> String {
        "simple-nix-update-gui".into()
    }

    fn icon_name(&self) -> String {
        ICON_NAME.into()
    }

    fn title(&self) -> String {
        if self.has_update.load(Ordering::Relaxed) {
            "Updates available".into()
        } else {
            "Up to date".into()
        }
    }

    fn status(&self) -> Status {
        if self.has_update.load(Ordering::Relaxed) {
            Status::NeedsAttention
        } else {
            Status::Active
        }
    }

    fn category(&self) -> Category {
        Category::SystemServices
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.actions.send(UiAction::OpenWindow);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let menu: Vec<ksni::MenuItem<Self>> = vec![
            StandardItem {
                label: "Open".into(),
                icon_name: "window-new".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.actions.send(UiAction::OpenWindow);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Check now".into(),
                icon_name: "view-refresh".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.actions.send(UiAction::CheckNow);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.actions.send(UiAction::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ];
        menu
    }
}

/// The single window, kept alive for the whole process so that closing it in
/// tray mode hides it instead of destroying it.
struct Window {
    window: ApplicationWindow,
    banner: Banner,
    current_label: Label,
    remote_label: Label,
    status_label: Label,
    last_check_label: Label,
    update_button: Button,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    // fmt::init would read RUST_LOG and, when that is unset, install a filter
    // that only lets ERROR through.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // notify-rust and other native helpers can panic from deep inside their own
    // runtime. A panic hook keeps the reason in the logs instead of a UI stuck
    // on its initial labels.
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("panic: {info}");
        tracing::error!(
            backtrace = %std::backtrace::Backtrace::force_capture(),
            "panic backtrace"
        );
    }));

    let args = Args::parse();
    let settings = build_settings(&args);

    let (actions_tx, actions_rx) = unbounded_channel::<UiAction>();
    let (check_tx, check_rx) = unbounded_channel::<()>();

    let app = AdwApplication::new(Some("org.simple_nix_update_gui"), Default::default());
    // In tray mode the process has to outlive its window, and the guard has to
    // stay alive for as long as it does.
    let _hold_guard = if settings.tray {
        Some(app.hold())
    } else {
        None
    };

    let tray_handle = if settings.tray {
        let tray = UpdateTray {
            has_update: Arc::new(AtomicBool::new(false)),
            actions: actions_tx.clone(),
        };
        let handle = tray.spawn().await?;
        let keeper = handle.clone();
        tokio::spawn(async move {
            let _keeper = keeper;
            std::future::pending::<()>().await;
        });
        Some(handle)
    } else {
        None
    };

    spawn_poller(settings.clone(), actions_tx.clone(), check_rx, tray_handle);

    let ui = Rc::new(RefCell::new(None::<Window>));
    let activate_ui = ui.clone();
    let activate_app = app.clone();
    let activate_settings = settings.clone();
    let activate_check_tx = check_tx.clone();
    let start_hidden = settings.tray;

    app.connect_activate(move |_| {
        if activate_ui.borrow().is_none() {
            let window = build_window(&activate_app, &activate_settings, &activate_check_tx);
            *activate_ui.borrow_mut() = Some(window);
        }
        if !start_hidden {
            if let Some(window) = activate_ui.borrow().as_ref() {
                window.window.present();
            }
        }
    });

    let poll_ui = ui.clone();
    let poll_app = app.clone();
    let mut actions_rx = actions_rx;
    glib::timeout_add_local(Duration::from_millis(200), move || {
        while let Ok(action) = actions_rx.try_recv() {
            apply_action(action, &poll_ui, &poll_app, &check_tx);
        }
        glib::ControlFlow::Continue
    });

    app.run_with_args(&[] as &[&str]);
    Ok(())
}

fn apply_action(
    action: UiAction,
    ui: &Rc<RefCell<Option<Window>>>,
    app: &AdwApplication,
    check_tx: &UnboundedSender<()>,
) {
    match action {
        UiAction::State(state) => show_state(ui, &state),
        UiAction::DaemonDown(reason) => show_daemon_down(ui, &reason),
        UiAction::OpenWindow => {
            let borrowed = ui.borrow();
            if let Some(window) = borrowed.as_ref() {
                window.window.present();
            }
        }
        UiAction::CheckNow => {
            let _ = check_tx.send(());
        }
        UiAction::Quit => {
            app.quit();
        }
    }
}

fn show_state(ui: &Rc<RefCell<Option<Window>>>, state: &UpdateState) {
    let borrowed = ui.borrow();
    let Some(window) = borrowed.as_ref() else {
        return;
    };
    window.banner.set_revealed(false);
    window
        .current_label
        .set_text(&format!("Current: {}", state.current_system));
    match (&state.remote_system, &state.last_error) {
        (Some(remote), _) => window.remote_label.set_text(&format!("Remote: {}", remote)),
        (None, Some(reason)) => window
            .remote_label
            .set_text(&format!("Remote: check failed: {}", reason)),
        (None, None) => window.remote_label.set_text("Remote: (none)"),
    }
    window.status_label.set_text(&format!(
        "Status: {}",
        if state.remote_system.is_none() {
            "Check failed"
        } else if state.has_update {
            "Update available"
        } else {
            "Up to date"
        }
    ));
    window
        .last_check_label
        .set_text(&format!("Last check: {}", state.last_check));
    window.update_button.set_sensitive(state.has_update);
}

fn show_daemon_down(ui: &Rc<RefCell<Option<Window>>>, reason: &str) {
    let borrowed = ui.borrow();
    let Some(window) = borrowed.as_ref() else {
        return;
    };
    window.banner.set_revealed(true);
    window.status_label.set_text(&format!(
        "Status: update daemon unavailable ({reason}), using local settings"
    ));
    window.update_button.set_sensitive(false);
}

fn build_window(
    app: &AdwApplication,
    settings: &Settings,
    check_tx: &UnboundedSender<()>,
) -> Window {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Simple Nix Update GUI")
        .default_width(600)
        .default_height(400)
        .build();

    let header = HeaderBar::new();
    let banner = Banner::builder().title("Update daemon unavailable").build();
    banner.set_button_label(Some("Retry"));
    let banner_check_tx = check_tx.clone();
    banner.connect_button_clicked(move |_| {
        let _ = banner_check_tx.send(());
    });

    let vbox = GtkBox::new(Orientation::Vertical, 12);
    vbox.set_margin_top(20);
    vbox.set_margin_bottom(20);
    vbox.set_margin_start(20);
    vbox.set_margin_end(20);

    let flake_label = Label::builder()
        .label(format!("Flake URI: {}", settings.flake_uri))
        .xalign(0.0)
        .wrap(true)
        .build();

    let system_label = Label::builder()
        .label(format!("System: {}", settings.system_name))
        .xalign(0.0)
        .build();

    let current_label = Label::builder()
        .label("Current: unknown")
        .xalign(0.0)
        .wrap(true)
        .build();

    let remote_label = Label::builder()
        .label("Remote: (none)")
        .xalign(0.0)
        .wrap(true)
        .build();

    let status_label = Label::builder()
        .label("Status: waiting for first check")
        .xalign(0.0)
        .wrap(true)
        .build();

    let last_check_label = Label::builder()
        .label("Last check: never")
        .xalign(0.0)
        .build();

    let update_button = Button::builder().label("Update...").build();
    let dialog_flake = settings.flake_uri.clone();
    let dialog_system = settings.system_name.clone();
    let dialog_use_nom = settings.use_nom;
    update_button.connect_clicked(move |_| {
        show_update_dialog(&dialog_flake, &dialog_system, dialog_use_nom);
    });

    let reboot_button = Button::builder().label("Check reboot needed").build();
    reboot_button.connect_clicked(|_| check_and_prompt_reboot());

    vbox.append(&flake_label);
    vbox.append(&system_label);
    vbox.append(&current_label);
    vbox.append(&remote_label);
    vbox.append(&status_label);
    vbox.append(&last_check_label);
    vbox.append(&update_button);
    vbox.append(&reboot_button);

    let view = ToolbarView::new();
    view.add_top_bar(&header);
    view.add_top_bar(&banner);
    view.set_content(Some(&vbox));
    window.set_content(Some(&view));

    // Without a tray icon there would be no way back to a hidden window, so
    // closing only gets intercepted when a tray icon exists.
    let tray_mode = settings.tray;
    window.connect_close_request(move |window| {
        if tray_mode {
            window.set_visible(false);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });

    Window {
        window,
        banner,
        current_label,
        remote_label,
        status_label,
        last_check_label,
        update_button,
    }
}

fn show_update_dialog(flake_uri: &str, system_name: &str, use_nom: bool) {
    let dialog = Dialog::builder()
        .title("Choose update action")
        .default_width(500)
        .build();

    let content = dialog.content_area();
    let vbox = GtkBox::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let info = Label::builder()
        .label(format!("Flake: {}#{}", flake_uri, system_name))
        .xalign(0.0)
        .wrap(true)
        .build();

    let test_btn = Button::builder()
        .label("Test build (nixos-rebuild build)")
        .build();
    let boot_btn = Button::builder()
        .label("Rebuild and boot (nixos-rebuild boot)")
        .build();
    let switch_btn = Button::builder()
        .label("Rebuild and switch (nixos-rebuild switch)")
        .build();

    for (button, action) in [
        (&test_btn, "build"),
        (&boot_btn, "boot"),
        (&switch_btn, "switch"),
    ] {
        let flake_uri = flake_uri.to_string();
        let system_name = system_name.to_string();
        button.connect_clicked(move |_| {
            run_nixos_rebuild(action, &flake_uri, &system_name, use_nom);
        });
    }

    vbox.append(&info);
    vbox.append(&test_btn);
    vbox.append(&boot_btn);
    vbox.append(&switch_btn);
    content.append(&vbox);

    dialog.show();
}

fn run_nixos_rebuild(action: &str, flake_uri: &str, system_name: &str, use_nom: bool) {
    let dialog = Dialog::builder()
        .title(format!("nixos-rebuild {}", action))
        .default_width(900)
        .default_height(600)
        .build();

    let content = dialog.content_area();
    let terminal = Terminal::new();
    terminal.spawn_async(
        PtyFlags::DEFAULT,
        None,
        &[
            "/bin/sh",
            "-c",
            &build_command(action, flake_uri, system_name, use_nom),
        ],
        &[],
        gtk4::glib::SpawnFlags::DEFAULT,
        || {},
        -1,
        None::<&gtk4::gio::Cancellable>,
        |_| {},
    );

    let scroll = ScrolledWindow::builder()
        .child(&terminal)
        .hexpand(true)
        .vexpand(true)
        .build();
    content.append(&scroll);

    dialog.show();
}

/// boot and switch need root, so they run through pkexec, which raises the
/// polkit agent popup. build only evaluates and builds, so it stays unprivileged.
fn build_command(action: &str, flake_uri: &str, system_name: &str, use_nom: bool) -> String {
    let flake = format!("{}#{}", flake_uri, system_name);
    let rebuild = if action == "build" {
        format!("nixos-rebuild {} --flake '{}'", action, flake)
    } else {
        format!("pkexec nixos-rebuild {} --flake '{}'", action, flake)
    };
    if use_nom {
        format!(
            "if command -v nom >/dev/null 2>&1; then {} 2>&1 | nom; else {} 2>&1; fi",
            rebuild, rebuild
        )
    } else {
        format!("{} 2>&1", rebuild)
    }
}

fn check_and_prompt_reboot() {
    if !is_reboot_needed_sync() {
        let dialog = gtk4::MessageDialog::builder()
            .modal(true)
            .text("No reboot needed")
            .secondary_text("Booted system matches current profile.")
            .message_type(gtk4::MessageType::Info)
            .buttons(gtk4::ButtonsType::Ok)
            .build();
        dialog.connect_response(|d, _| d.close());
        dialog.show();
        return;
    }

    let dialog = gtk4::MessageDialog::builder()
        .modal(true)
        .text("Reboot needed")
        .secondary_text("A reboot is required to activate the new system.")
        .message_type(gtk4::MessageType::Question)
        .buttons(gtk4::ButtonsType::YesNo)
        .build();
    dialog.connect_response(move |d, response| {
        if response == gtk4::ResponseType::Yes {
            let _ = Command::new("pkexec")
                .arg("systemctl")
                .arg("reboot")
                .spawn();
        }
        d.close();
    });
    dialog.show();
}

fn is_reboot_needed_sync() -> bool {
    let booted = Command::new("readlink").arg("/run/booted-system").output();
    let profile = Command::new("readlink")
        .arg("-f")
        .arg("/nix/var/nix/profiles/system")
        .output();
    match (booted, profile) {
        (Ok(booted), Ok(profile)) if booted.status.success() && profile.status.success() => {
            let booted_path = String::from_utf8(booted.stdout).unwrap_or_default();
            let profile_path = String::from_utf8(profile.stdout).unwrap_or_default();
            booted_path.trim() != profile_path.trim()
        }
        _ => false,
    }
}

fn parse_interval(interval: &str) -> Duration {
    let trimmed = interval.trim();
    let split_at = trimmed
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (digits, unit) = trimmed.split_at(split_at);
    let amount: u64 = digits.parse().unwrap_or(3600);
    match unit.trim().to_lowercase().as_str() {
        "s" | "sec" | "second" | "seconds" => Duration::from_secs(amount),
        "m" | "min" | "minute" | "minutes" => Duration::from_secs(amount * 60),
        "h" | "hour" | "hours" => Duration::from_secs(amount * 3600),
        "d" | "day" | "days" => Duration::from_secs(amount * 86400),
        _ => Duration::from_secs(amount),
    }
}

async fn daemon_proxy(bus_name: &str) -> Result<zbus::Proxy<'_>> {
    let connection = zbus::Connection::session().await?;
    zbus::Proxy::new(
        &connection,
        bus_name,
        "/org/simple_nix_update_gui/Daemon",
        "org.simple_nix_update_gui.Daemon",
    )
    .await
    .map_err(Into::into)
}

async fn fetch_state(proxy: &zbus::Proxy<'_>) -> Result<UpdateState> {
    let state: String = proxy.call("GetState", &()).await?;
    Ok(serde_json::from_str(&state)?)
}

async fn trigger_check(proxy: &zbus::Proxy<'_>) -> Result<UpdateState> {
    let state: String = proxy.call("CheckForUpdates", &()).await?;
    Ok(serde_json::from_str(&state)?)
}

fn spawn_poller(
    settings: Settings,
    actions: UnboundedSender<UiAction>,
    mut check_rx: UnboundedReceiver<()>,
    tray: Option<ksni::Handle<UpdateTray>>,
) {
    tokio::spawn(async move {
        let interval = parse_interval(&settings.check_interval);
        let mut previous: Option<bool> = None;
        tracing::info!(
            bus_name = %settings.bus_name,
            interval = ?interval,
            "poller started"
        );

        // The daemon is started by dbus and systemd, so at GUI startup it may not
        // own the name yet. Every failure drops back here to look for it again.
        'connect: loop {
            let proxy = match daemon_proxy(&settings.bus_name).await {
                Ok(proxy) => {
                    tracing::info!(bus_name = %settings.bus_name, "connected to daemon");
                    proxy
                }
                Err(error) => {
                    tracing::warn!(
                        bus_name = %settings.bus_name,
                        error = %error,
                        "daemon not reachable, waiting before retry"
                    );
                    let _ = actions.send(UiAction::DaemonDown(error.to_string()));
                    wait_for_retry(&mut check_rx, interval).await;
                    continue 'connect;
                }
            };

            let mut ticker = tokio::time::interval(interval);
            loop {
                // A manual check returns the state it just computed, so it is
                // used directly rather than asking for it a second time.
                let mut checked = None;
                tokio::select! {
                    // The first tick is immediate, so state is shown on startup.
                    _ = ticker.tick() => {}
                    _ = check_rx.recv() => {
                        tracing::info!("manual check requested");
                        match trigger_check(&proxy).await {
                            Ok(state) => checked = Some(state),
                            Err(error) => {
                                tracing::error!(error = %error, "manual check failed");
                                let _ = actions.send(UiAction::DaemonDown(error.to_string()));
                                continue 'connect;
                            }
                        }
                    }
                }

                let state = match checked {
                    Some(state) => state,
                    None => {
                        tracing::info!("querying daemon for state");
                        match fetch_state(&proxy).await {
                            Ok(state) => state,
                            Err(error) => {
                                tracing::error!(error = %error, "fetch_state failed");
                                let _ = actions.send(UiAction::DaemonDown(error.to_string()));
                                continue 'connect;
                            }
                        }
                    }
                };
                tracing::debug!(
                    has_update = state.has_update,
                    current = %state.current_system,
                    remote = ?state.remote_system,
                    error = ?state.last_error,
                    last_check = %state.last_check,
                    daemon_flake_uri = %state.flake_uri,
                    daemon_system = %state.system_name,
                    "state fetched from daemon"
                );
                if state.flake_uri != settings.flake_uri {
                    tracing::warn!(
                        gui = %settings.flake_uri,
                        daemon = %state.flake_uri,
                        "GUI and daemon disagree on flake URI"
                    );
                }

                // A first sighting counts as a transition, so a tray that starts
                // up while an update is pending still announces it once.
                if state.has_update && previous != Some(true) && settings.auto_notify {
                    tracing::info!(
                        system = %state.system_name,
                        flake_uri = %state.flake_uri,
                        "update available, sending notification"
                    );
                    notify_update_available(&state).await;
                }
                previous = Some(state.has_update);

                if let Some(handle) = &tray {
                    let has_update = state.has_update;
                    handle
                        .update(move |tray: &mut UpdateTray| {
                            tray.has_update.store(has_update, Ordering::Relaxed);
                        })
                        .await;
                }

                let _ = actions.send(UiAction::State(Box::new(state)));
            }
        }
    });
}

/// Sleeps until either the interval elapses or the Retry button asks for a new
/// attempt, so a missing daemon costs one connection attempt per interval.
async fn wait_for_retry(check_rx: &mut UnboundedReceiver<()>, interval: Duration) {
    tokio::select! {
        _ = tokio::time::sleep(interval) => {}
        _ = check_rx.recv() => {}
    }
}

/// notify-rust spins up its own tokio runtime on the calling thread, which
/// panics with "Cannot start a runtime from within a runtime" when called on a
/// tokio worker. spawn_blocking runs it on a blocking thread instead.
async fn notify_update_available(state: &UpdateState) {
    let body = format!(
        "Update available for {} ({})",
        state.system_name, state.flake_uri
    );
    match tokio::task::spawn_blocking(move || {
        notify_rust::Notification::new()
            .summary("System updates")
            .body(&body)
            .icon(ICON_NAME)
            .timeout(notify_rust::Timeout::Milliseconds(10000))
            .show()
    })
    .await
    {
        Ok(Ok(_)) => tracing::info!("notification sent"),
        Ok(Err(error)) => tracing::warn!(error = %error, "notification failed"),
        Err(error) => tracing::warn!(error = %error, "notification task panicked"),
    }
}

fn get_hostname() -> String {
    hostname::get()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}
