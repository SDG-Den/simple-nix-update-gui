use anyhow::{bail, Context, Result};
use clap::{ArgAction, Parser};
use futures_util::StreamExt;
use gtk4::{
    self, gdk::RGBA, glib, Box as GtkBox, Button, FlowBox, Label, Orientation, ScrolledWindow,
    SelectionMode, Spinner, TextView,
};
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
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use vte4::prelude::*;
use vte4::{PtyFlags, Terminal};

const ICON_NAME: &str = "system-software-update";

/// Shown as the notification's source. The notify-rust default is the
/// filename of the running executable, which is the Nix wrapper.
const NOTIFICATION_APP_NAME: &str = "Nix Updates";

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
    #[arg(long, env = "SNU_CLONE_PATH")]
    clone_path: Option<String>,
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
    clone_path: String,
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

    let (clone_path, clone_path_source) = resolve_str(
        &args.clone_path,
        "SNU_CLONE_PATH",
        &file,
        "$HOME/repos/nix-config",
    );
    let clone_path = expand_home(&clone_path);
    tracing::info!(setting = "clone_path", value = %clone_path, source = clone_path_source);

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
        clone_path,
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

/// The module is system-wide, so a clone path default like $HOME/repos/nix-config
/// reaches the GUI unexpanded. Expanding it here keeps the value correct for
/// whichever user started the GUI, before it is single-quoted into the shell
/// command where it would no longer expand.
fn expand_home(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    if let Some(rest) = path.strip_prefix("$HOME/") {
        return format!("{}/{}", home, rest);
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return format!("{}/{}", home, rest);
    }
    if path == "$HOME" || path == "~" {
        return home;
    }
    path.to_string()
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
    /// Absent when talking to a daemon that predates the field; a check from
    /// such a daemon simply reports no progress.
    #[serde(default)]
    checking: bool,
}

/// What background tasks ask the GTK thread to do. The GTK thread owns every
/// widget and the background tasks own none, so this channel is the only thing
/// that crosses between them.
enum UiAction {
    State(Box<UpdateState>),
    StoreStats(Result<Box<StoreStats>, String>),
    DaemonDown(String),
    OpenWindow,
    CheckNow,
    CheckStarted,
    CheckProgress(String),
    RefreshStoreStats,
    Quit,
}

// Manual check and catch-up requests stay as separate inputs to the poller's
// select loop so a straggler check line can never start a new eval.
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

    /// The host switches to this one as soon as `status` is NeedsAttention and
    /// falls back to the pixmap list when it is empty, which shows a
    /// placeholder, so it has to be set even though it equals `icon_name`.
    fn attention_icon_name(&self) -> String {
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

    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        tracing::warn!(
            ?reason,
            "StatusNotifierWatcher offline, tray icon appears when it comes up"
        );
        true
    }

    fn watcher_online(&self) {
        tracing::info!("StatusNotifierWatcher online, tray icon registered");
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
    store_size_label: Label,
    disk_free_label: Label,
    disk_total_label: Label,
    check_button: Button,
    spinner: Spinner,
    progress_view: TextView,
    progress_scroller: ScrolledWindow,
    rebuild_buttons: Vec<Button>,
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
    let (catchup_tx, catchup_rx) = unbounded_channel::<()>();
    // Set by the GTK thread when the progress display opens and cleared when
    // it closes, so the progress listener knows whether a daemon initiated
    // check has already been picked up.
    let check_active = Arc::new(AtomicBool::new(false));
    let (stats_tx, stats_rx) = unbounded_channel::<()>();
    // Set by the window's map/unmap signals; the stats task uses it so the
    // du walk only happens while someone can see the number it produces.
    let window_visible = Arc::new(AtomicBool::new(false));

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
        // The session can call this at login before the desktop's
        // StatusNotifierWatcher (DMS, GNOME appindicator) exists. With
        // assume_sni_available that failure becomes a soft error: spawn
        // succeeds anyway and ksni registers itself once the watcher appears.
        let handle = tray.assume_sni_available(true).spawn().await?;
        let keeper = handle.clone();
        tokio::spawn(async move {
            let _keeper = keeper;
            std::future::pending::<()>().await;
        });
        Some(handle)
    } else {
        None
    };

    spawn_store_stats(
        actions_tx.clone(),
        stats_rx,
        window_visible.clone(),
        parse_interval(&settings.check_interval),
    );
    spawn_poller(
        settings.clone(),
        actions_tx.clone(),
        check_rx,
        catchup_rx,
        tray_handle,
        stats_tx.clone(),
    );
    spawn_progress_listener(
        settings.bus_name.clone(),
        actions_tx.clone(),
        check_active.clone(),
        catchup_tx,
    );

    let ui = Rc::new(RefCell::new(None::<Window>));
    let activate_ui = ui.clone();
    let activate_app = app.clone();
    let activate_settings = settings.clone();
    let activate_actions = actions_tx.clone();
    let activate_stats = stats_tx.clone();
    let activate_visible = window_visible.clone();
    let start_hidden = settings.tray;

    app.connect_activate(move |_| {
        if activate_ui.borrow().is_none() {
            let window = build_window(
                &activate_app,
                &activate_settings,
                &activate_actions,
                &activate_stats,
                &activate_visible,
            );
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
    let poll_active = check_active.clone();
    let mut actions_rx = actions_rx;
    glib::timeout_add_local(Duration::from_millis(200), move || {
        while let Ok(action) = actions_rx.try_recv() {
            apply_action(
                action,
                &poll_ui,
                &poll_app,
                &check_tx,
                &poll_active,
                &stats_tx,
            );
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
    check_active: &Arc<AtomicBool>,
    stats_tx: &UnboundedSender<()>,
) {
    match action {
        UiAction::State(state) => show_state(ui, &state, check_active),
        UiAction::StoreStats(stats) => show_store_stats(ui, &stats),
        UiAction::RefreshStoreStats => {
            let _ = stats_tx.send(());
        }
        UiAction::DaemonDown(reason) => {
            show_daemon_down(ui, &reason, check_active);
        }
        UiAction::OpenWindow => {
            let borrowed = ui.borrow();
            if let Some(window) = borrowed.as_ref() {
                window.window.present();
            }
        }
        UiAction::CheckNow => {
            // Opening the display here gives the click feedback before the
            // poller picks the request up, and marks the check as picked up so
            // the progress listener does not also wake the poller.
            let borrowed = ui.borrow();
            if let Some(window) = borrowed.as_ref() {
                start_check_display(window, true, check_active);
            }
            drop(borrowed);
            let _ = check_tx.send(());
        }
        UiAction::CheckStarted => {
            let borrowed = ui.borrow();
            if let Some(window) = borrowed.as_ref() {
                start_check_display(window, true, check_active);
            }
        }
        UiAction::CheckProgress(line) => {
            let borrowed = ui.borrow();
            if let Some(window) = borrowed.as_ref() {
                append_progress(window, &line, check_active);
            }
        }
        UiAction::Quit => {
            app.quit();
        }
    }
}

/// Clears the progress pane and spins the button until the check reports its
/// result. A repeated start keeps the pane as it is, so a start that follows
/// already delivered lines does not wipe them.
fn start_check_display(window: &Window, clear: bool, check_active: &Arc<AtomicBool>) {
    check_active.store(true, Ordering::Relaxed);
    if clear {
        window.progress_view.buffer().set_text("");
    }
    window.progress_scroller.set_visible(true);
    window.spinner.set_visible(true);
    window.spinner.start();
    window.check_button.set_sensitive(false);
}

fn end_check_display(window: &Window, check_active: &Arc<AtomicBool>) {
    check_active.store(false, Ordering::Relaxed);
    window.spinner.stop();
    window.spinner.set_visible(false);
    window.progress_scroller.set_visible(false);
    window.check_button.set_sensitive(true);
}

fn append_progress(window: &Window, line: &str, check_active: &Arc<AtomicBool>) {
    if !window.progress_scroller.is_visible() {
        start_check_display(window, true, check_active);
    }
    let buffer = window.progress_view.buffer();
    let mut end = buffer.end_iter();
    buffer.insert(&mut end, &format!("{line}\n"));
    let mut end = buffer.end_iter();
    window
        .progress_view
        .scroll_to_iter(&mut end, 0.0, false, 0.0, 1.0);
}

fn show_state(
    ui: &Rc<RefCell<Option<Window>>>,
    state: &UpdateState,
    check_active: &Arc<AtomicBool>,
) {
    let borrowed = ui.borrow();
    let Some(window) = borrowed.as_ref() else {
        return;
    };

    // A check in flight owns the progress display; a settled state closes it.
    if state.checking {
        start_check_display(window, false, check_active);
    } else {
        end_check_display(window, check_active);
    }

    // Both fields are None only in the initial state the daemon serves before
    // its first eval finishes, so this is the exact "no result yet" signal.
    let no_result = state.remote_system.is_none() && state.last_error.is_none();
    if no_result {
        window.banner.set_title(if state.checking {
            "Checking for updates"
        } else {
            "No daemon result yet, waiting for the first check"
        });
        window.banner.set_revealed(true);
        window
            .current_label
            .set_text(&format!("Current: {}", state.current_system));
        window.remote_label.set_text("Remote: (none)");
        window.status_label.set_text(if state.checking {
            "Status: checking for updates"
        } else {
            "Status: waiting for first check"
        });
        window.last_check_label.set_text("Last check: never");
        set_rebuild_buttons(&window.rebuild_buttons, false);
        return;
    }

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
        if state.checking {
            "checking for updates"
        } else if state.remote_system.is_none() {
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
    set_rebuild_buttons(&window.rebuild_buttons, state.has_update);
}

fn set_rebuild_buttons(buttons: &[Button], sensitive: bool) {
    for button in buttons {
        button.set_sensitive(sensitive);
    }
}

fn show_daemon_down(
    ui: &Rc<RefCell<Option<Window>>>,
    reason: &str,
    check_active: &Arc<AtomicBool>,
) {
    let borrowed = ui.borrow();
    let Some(window) = borrowed.as_ref() else {
        return;
    };
    window.banner.set_title("Update daemon unavailable");
    window.banner.set_revealed(true);
    window.status_label.set_text(&format!(
        "Status: update daemon unavailable ({reason}), using local settings"
    ));
    set_rebuild_buttons(&window.rebuild_buttons, false);
    // A failed connection also ends any check display the daemon had open.
    end_check_display(window, check_active);
}

fn show_store_stats(ui: &Rc<RefCell<Option<Window>>>, stats: &Result<Box<StoreStats>, String>) {
    let borrowed = ui.borrow();
    let Some(window) = borrowed.as_ref() else {
        return;
    };
    match stats {
        Ok(stats) => {
            let free_pct = percent_of(stats.disk_free_bytes, stats.disk_total_bytes);
            match stats.store_bytes {
                Some(store_bytes) => {
                    let store_pct = percent_of(store_bytes, stats.disk_total_bytes);
                    // The database estimate stands in until a du walk
                    // finishes, so it carries a tilde.
                    let marker = if stats.accurate { "" } else { "~" };
                    window.store_size_label.set_text(&format!(
                        "Nix store: {marker}{} ({}% of disk)",
                        format_gb(store_bytes),
                        store_pct
                    ));
                }
                None => {
                    window.store_size_label.set_text("Nix store: unavailable");
                }
            }
            window.disk_free_label.set_text(&format!(
                "Disk free: {} ({}%)",
                format_gb(stats.disk_free_bytes),
                free_pct
            ));
            window.disk_total_label.set_text(&format!(
                "Disk total: {}",
                format_gb(stats.disk_total_bytes)
            ));
        }
        Err(reason) => {
            tracing::warn!(reason = %reason, "store stats unavailable");
            window.store_size_label.set_text("Nix store: unavailable");
            window.disk_free_label.set_text("Disk free: unavailable");
            window.disk_total_label.set_text("Disk total: unavailable");
        }
    }
}

fn build_window(
    app: &AdwApplication,
    settings: &Settings,
    actions: &UnboundedSender<UiAction>,
    stats_tx: &UnboundedSender<()>,
    window_visible: &Arc<AtomicBool>,
) -> Window {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Simple Nix Update GUI")
        .default_width(760)
        .default_height(600)
        .build();

    let header = HeaderBar::new();
    let check_button = Button::builder().label("Check now").build();
    let spinner = Spinner::new();
    spinner.set_visible(false);
    header.pack_start(&check_button);
    header.pack_end(&spinner);
    let button_actions = actions.clone();
    check_button.connect_clicked(move |_| {
        let _ = button_actions.send(UiAction::CheckNow);
    });

    let banner = Banner::builder()
        .title("No daemon result yet, waiting for the first check")
        .build();
    banner.set_button_label(Some("Check now"));
    banner.set_revealed(true);
    let banner_actions = actions.clone();
    banner.connect_button_clicked(move |_| {
        let _ = banner_actions.send(UiAction::CheckNow);
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
        .wrap_mode(gtk4::pango::WrapMode::WordChar)
        .build();

    let system_label = Label::builder()
        .label(format!("System: {}", settings.system_name))
        .xalign(0.0)
        .wrap(true)
        .build();

    let current_label = Label::builder()
        .label("Current: unknown")
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk4::pango::WrapMode::WordChar)
        .build();

    let remote_label = Label::builder()
        .label("Remote: (none)")
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk4::pango::WrapMode::WordChar)
        .build();

    let status_label = Label::builder()
        .label("Status: waiting for first check")
        .xalign(0.0)
        .wrap(true)
        .build();

    let last_check_label = Label::builder()
        .label("Last check: never")
        .xalign(0.0)
        .wrap(true)
        .build();

    let store_size_label = Label::builder()
        .label("Nix store: loading...")
        .xalign(0.0)
        .wrap(true)
        .build();

    let disk_free_label = Label::builder()
        .label("Disk free: loading...")
        .xalign(0.0)
        .wrap(true)
        .build();

    let disk_total_label = Label::builder()
        .label("Disk total: loading...")
        .xalign(0.0)
        .wrap(true)
        .build();

    let action_box = FlowBox::new();
    action_box.set_selection_mode(SelectionMode::None);
    action_box.set_homogeneous(false);
    action_box.set_row_spacing(8);
    action_box.set_column_spacing(8);

    let terminal_area = GtkBox::new(Orientation::Vertical, 0);
    terminal_area.set_vexpand(true);
    terminal_area.set_visible(false);

    let mut rebuild_buttons = Vec::new();
    for (action, label) in [
        ("build", "rebuild (test rebuild only)"),
        ("boot", "rebuild (switch on next reboot)"),
        ("switch", "rebuild (switch now)"),
    ] {
        let button = Button::builder().label(label).build();
        button.set_sensitive(false);
        let flake_uri = settings.flake_uri.clone();
        let system_name = settings.system_name.clone();
        let clone_path = settings.clone_path.clone();
        let use_nom = settings.use_nom;
        let terminal_area = terminal_area.clone();
        let refresh_actions = actions.clone();
        button.connect_clicked(move |_| {
            run_nixos_rebuild(
                &terminal_area,
                action,
                &flake_uri,
                &clone_path,
                &system_name,
                use_nom,
                refresh_actions.clone(),
            );
        });
        action_box.insert(&button, -1);
        rebuild_buttons.push(button);
    }

    let clean_button = Button::builder().label("Clean nix store").build();
    {
        let terminal_area = terminal_area.clone();
        let flake_uri = settings.flake_uri.clone();
        let system_name = settings.system_name.clone();
        let clone_path = settings.clone_path.clone();
        let use_nom = settings.use_nom;
        let refresh_actions = actions.clone();
        clean_button.connect_clicked(move |_| {
            confirm_and_clean_store(
                terminal_area.clone(),
                flake_uri.clone(),
                clone_path.clone(),
                system_name.clone(),
                use_nom,
                refresh_actions.clone(),
            );
        });
    }
    action_box.insert(&clean_button, -1);

    // The check's stderr lines land here while an eval runs. It is hidden
    // outside a check so the window keeps its usual layout.
    let progress_view = TextView::builder()
        .monospace(true)
        .editable(false)
        .cursor_visible(false)
        .left_margin(8)
        .top_margin(6)
        .right_margin(8)
        .bottom_margin(6)
        .build();
    let progress_scroller = ScrolledWindow::builder()
        .child(&progress_view)
        .height_request(160)
        .hexpand(true)
        .build();
    progress_scroller.set_visible(false);

    vbox.append(&flake_label);
    vbox.append(&system_label);
    vbox.append(&current_label);
    vbox.append(&remote_label);
    vbox.append(&status_label);
    vbox.append(&last_check_label);
    vbox.append(&store_size_label);
    vbox.append(&disk_free_label);
    vbox.append(&disk_total_label);
    vbox.append(&action_box);
    vbox.append(&progress_scroller);
    vbox.append(&terminal_area);

    let view = ToolbarView::new();
    view.add_top_bar(&header);
    view.add_top_bar(&banner);

    let scroller = ScrolledWindow::builder()
        .child(&vbox)
        .hexpand(true)
        .vexpand(true)
        .build();
    view.set_content(Some(&scroller));
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

    // Showing the window is also a stats trigger: it starts the database
    // estimate right away and lets the du walk run now that someone can see
    // its result, while hiding marks the walk pointless again.
    let map_visible = window_visible.clone();
    let map_stats = stats_tx.clone();
    window.connect_map(move |_| {
        map_visible.store(true, Ordering::Relaxed);
        let _ = map_stats.send(());
    });
    let unmap_visible = window_visible.clone();
    window.connect_unmap(move |_| {
        unmap_visible.store(false, Ordering::Relaxed);
    });

    Window {
        window,
        banner,
        current_label,
        remote_label,
        status_label,
        last_check_label,
        store_size_label,
        disk_free_label,
        disk_total_label,
        check_button,
        spinner,
        progress_view,
        progress_scroller,
        rebuild_buttons,
    }
}

/// VTE paints its own canvas and ignores GTK CSS, so the active theme's
/// named colors are looked up and applied when a rebuild terminal is
/// created. Only names the theme defines are used; other palette slots
/// fall back to standard dark ANSI colors because VTE requires the full
/// 0/8/16/232/256 palette size.
#[allow(deprecated)]
fn apply_gtk_theme_colors(terminal: &Terminal) {
    let context = terminal.style_context();
    let lookup = |name: &str| context.lookup_color(name);

    let foreground = ["theme_fg_color", "view_fg_color", "window_fg_color"]
        .iter()
        .find_map(|name| lookup(name));
    let background = ["theme_bg_color", "view_bg_color", "window_bg_color"]
        .iter()
        .find_map(|name| lookup(name));

    let candidates: [&[&str]; 16] = [
        &[],
        &["red_1", "error_color", "destructive_color"],
        &["green_1", "success_color"],
        &["yellow_1", "warning_color"],
        &["blue_1", "accent_bg_color", "accent_color"],
        &["purple_1"],
        &[],
        &["light_1", "window_fg_color"],
        &["dark_2", "headerbar_bg_color"],
        &["red_1", "error_color", "destructive_color"],
        &["green_1", "success_color"],
        &["yellow_1", "warning_color"],
        &["blue_1", "accent_bg_color", "accent_color"],
        &["purple_1"],
        &[],
        &["light_5", "window_fg_color"],
    ];
    let fallbacks: [&str; 16] = [
        "#2e3436", "#cc0000", "#4e9a06", "#c4a000", //
        "#3465a4", "#75507b", "#06989a", "#d3d7cf", "#555753", "#ef2929", "#8ae234", "#fce94f",
        "#729fcf", "#ad7fa8", "#34e2e2", "#eeeeec",
    ];

    let mut from_theme = foreground.is_some() || background.is_some();
    let mut palette = Vec::with_capacity(16);
    for (names, fallback) in candidates.iter().zip(fallbacks) {
        let theme_color = names.iter().find_map(|name| lookup(name));
        if theme_color.is_some() {
            from_theme = true;
        }
        let color = match theme_color {
            Some(color) => color,
            None => match fallback.parse::<RGBA>() {
                Ok(color) => color,
                Err(error) => {
                    tracing::warn!(fallback, %error, "invalid ANSI fallback color");
                    return;
                }
            },
        };
        palette.push(color);
    }

    if !from_theme {
        tracing::info!("no GTK theme colors resolved, keeping VTE defaults");
        return;
    }

    let palette_refs: Vec<&RGBA> = palette.iter().collect();
    terminal.set_colors(foreground.as_ref(), background.as_ref(), &palette_refs);
    tracing::info!(
        foreground = foreground.is_some(),
        background = background.is_some(),
        "applied GTK theme colors to terminal"
    );
}

fn run_nixos_rebuild(
    terminal_area: &GtkBox,
    action: &str,
    flake_uri: &str,
    clone_path: &str,
    system_name: &str,
    use_nom: bool,
    refresh_actions: UnboundedSender<UiAction>,
) {
    let command = build_command(action, flake_uri, clone_path, system_name, use_nom);
    run_in_terminal(terminal_area, &command, refresh_actions);
}

fn run_in_terminal(
    terminal_area: &GtkBox,
    command: &str,
    refresh_actions: UnboundedSender<UiAction>,
) {
    while let Some(child) = terminal_area.first_child() {
        terminal_area.remove(&child);
    }

    let terminal = Terminal::new();
    apply_gtk_theme_colors(&terminal);
    terminal.connect_child_exited(move |_, _| {
        // The rebuild just changed /run/current-system, so the daemon is asked
        // for a fresh check instead of waiting for the next periodic one, and
        // the stats task gets a trigger that respects its du throttle.
        let _ = refresh_actions.send(UiAction::CheckNow);
        let _ = refresh_actions.send(UiAction::RefreshStoreStats);
    });
    terminal.spawn_async(
        PtyFlags::DEFAULT,
        None,
        &["/bin/sh", "-c", command],
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
    terminal_area.append(&scroll);
    terminal_area.set_visible(true);
}

/// Asks before chaining the two garbage collections and the switch rebuild,
/// since the first command drops profile generations older than 30 days.
fn confirm_and_clean_store(
    terminal_area: GtkBox,
    flake_uri: String,
    clone_path: String,
    system_name: String,
    use_nom: bool,
    refresh_actions: UnboundedSender<UiAction>,
) {
    let dialog = gtk4::MessageDialog::builder()
        .modal(true)
        .text("Clean Nix store?")
        .secondary_text(
            "Runs nix-collect-garbage --delete-older-than 30d, then nix-collect-garbage, \
             then rebuild (switch now). Generations older than 30 days are deleted.",
        )
        .message_type(gtk4::MessageType::Question)
        .buttons(gtk4::ButtonsType::YesNo)
        .build();
    dialog.connect_response(move |d, response| {
        if response == gtk4::ResponseType::Yes {
            let command = build_clean_command(&flake_uri, &clone_path, &system_name, use_nom);
            run_in_terminal(&terminal_area, &command, refresh_actions.clone());
        }
        d.close();
    });
    dialog.show();
}

/// The garbage collections run unprivileged, as written. The switch rebuild
/// keeps the sudo, git sync, and nom handling build_command already applies.
fn build_clean_command(
    flake_uri: &str,
    clone_path: &str,
    system_name: &str,
    use_nom: bool,
) -> String {
    format!(
        "nix-collect-garbage --delete-older-than 30d && nix-collect-garbage && {}",
        build_command("switch", flake_uri, clone_path, system_name, use_nom)
    )
}

/// A git+ flake URI names a repository that a root-run nixos-rebuild cannot
/// fetch: sudo resets HOME to /root, which holds no git credentials. So the
/// command first clones or pulls the repository into clone_path as the
/// invoking user, and the rebuild then runs from that local path. Returns the
/// shell snippet (ending in "&& ") and the flake root to rebuild from;
/// non-git URIs need no clone and come back unchanged.
fn git_sync_command(flake_uri: &str, clone_path: &str) -> (String, String) {
    let Some(rest) = flake_uri.strip_prefix("git+") else {
        return (String::new(), flake_uri.to_string());
    };
    let (location, query) = match rest.split_once('?') {
        Some((location, query)) => (location, query),
        None => (rest, ""),
    };
    let mut flake_root = clone_path.to_string();
    let mut ignored: Vec<&str> = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        match pair.split_once('=') {
            Some(("dir", dir)) => flake_root = format!("{}/{}", clone_path, dir),
            _ => ignored.push(pair),
        }
    }
    let mut sync = String::new();
    if !ignored.is_empty() {
        sync.push_str(&format!(
            "echo 'warning: ignoring flake URI params: {}' && ",
            ignored.join(" ")
        ));
    }
    sync.push_str(&format!(
        "if [ -d '{clone}/.git' ]; then git -C '{clone}' pull; else mkdir -p '{clone}' && git clone '{url}' '{clone}'; fi && ",
        clone = clone_path,
        url = location,
    ));
    (sync, flake_root)
}

/// The git sync runs first, unprivileged, so the clone/pull uses the invoking
/// user's credentials. boot and switch then need root, so they run through
/// sudo, which prompts for a password inside the integrated terminal. build
/// only evaluates and builds, so it stays unprivileged. Under nom the prompt
/// would be swallowed by nom's tui, so privileged runs first authenticate with
/// a throwaway sudo command.
fn build_command(
    action: &str,
    flake_uri: &str,
    clone_path: &str,
    system_name: &str,
    use_nom: bool,
) -> String {
    let (sync, flake_root) = git_sync_command(flake_uri, clone_path);
    let flake = format!("{}#{}", flake_root, system_name);
    let rebuild = if action == "build" {
        format!("nixos-rebuild {} --flake '{}'", action, flake)
    } else {
        format!("sudo nixos-rebuild {} --flake '{}'", action, flake)
    };
    let preauth = if action == "build" {
        String::new()
    } else {
        "sudo echo \"Starting update\" && ".to_string()
    };
    if use_nom {
        format!(
            "if command -v nom >/dev/null 2>&1; then {sync}{preauth}{rebuild} 2>&1 | nom; else {sync}{rebuild} 2>&1; fi"
        )
    } else {
        format!("{}{} 2>&1", sync, rebuild)
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
    mut catchup_rx: UnboundedReceiver<()>,
    tray: Option<ksni::Handle<UpdateTray>>,
    stats_tx: UnboundedSender<()>,
) {
    tokio::spawn(async move {
        let interval = parse_interval(&settings.check_interval);
        let mut previous: Option<bool> = None;
        // While a check runs the daemon is polled once a second so the
        // progress display closes when the eval ends rather than on the next
        // hourly tick. The fast interval is only awaited when watching.
        let mut watching = false;
        let mut fast = tokio::time::interval(Duration::from_secs(1));
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
                let mut request = None;
                tokio::select! {
                    // The first tick is immediate, so state is shown on startup.
                    _ = ticker.tick() => {}
                    _ = fast.tick(), if watching => {
                        tracing::debug!("watching a running check, querying state");
                        match fetch_state(&proxy).await {
                            Ok(state) => checked = Some(state),
                            Err(error) => {
                                tracing::error!(error = %error, "fetch_state failed");
                                let _ = actions.send(UiAction::DaemonDown(error.to_string()));
                                continue 'connect;
                            }
                        }
                    }
                    Some(received) = check_rx.recv() => {
                        request = Some(received);
                        tracing::info!("manual check requested");
                        let _ = actions.send(UiAction::CheckStarted);
                        match trigger_check(&proxy).await {
                            Ok(state) => checked = Some(state),
                            Err(error) => {
                                tracing::error!(error = %error, "manual check failed");
                                let _ = actions.send(UiAction::DaemonDown(error.to_string()));
                                continue 'connect;
                            }
                        }
                    }
                    // The progress listener asks for a sync when a daemon
                    // started a check on its own; reading state follows that
                    // check without triggering a second one.
                    _ = catchup_rx.recv() => {
                        tracing::info!("daemon check in progress, catching up");
                        match fetch_state(&proxy).await {
                            Ok(state) => checked = Some(state),
                            Err(error) => {
                                tracing::error!(error = %error, "fetch_state failed");
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

                // The first sighting of a running check opens the progress
                // display and seeds it with the lines produced so far; the
                // seed covers lines emitted before the listener subscribed.
                if state.checking && !watching {
                    tracing::info!("daemon check in progress, watching for completion");
                    let _ = actions.send(UiAction::CheckStarted);
                    seed_progress(&proxy, &actions).await;
                } else if request.is_some() && state.checking {
                    // A manual check on an already running eval: the listener
                    // has the live lines, the seed covers what it missed.
                    seed_progress(&proxy, &actions).await;
                }
                watching = state.checking;

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
                    notify_update_available(&state, actions.clone()).await;
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

                let settled = !state.checking;
                let _ = actions.send(UiAction::State(Box::new(state)));

                // One store stats trigger per completed check keeps the
                // refresh on the poller's schedule instead of its own timer;
                // the stats task decides whether a du walk is due. States
                // that only watch a running check do not count as completed.
                if settled {
                    let _ = stats_tx.send(());
                }
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

/// Feeds the lines of an already running check into the UI channel so a
/// display that opens mid-check starts with what it missed.
async fn seed_progress(proxy: &zbus::Proxy<'_>, actions: &UnboundedSender<UiAction>) {
    let json: String = match proxy.call("GetProgress", &()).await {
        Ok(json) => json,
        Err(error) => {
            tracing::warn!(%error, "could not fetch check progress");
            return;
        }
    };
    match serde_json::from_str::<Vec<String>>(&json) {
        Ok(lines) => {
            for line in lines {
                let _ = actions.send(UiAction::CheckProgress(line));
            }
        }
        Err(error) => tracing::warn!(%error, "invalid progress payload"),
    }
}

/// Forwards the daemon's CheckProgress signals to the GTK thread. The first
/// line of a daemon initiated check also wakes the poller, which follows the
/// check to its end; lines that arrive while a display is already open need no
/// wake-up.
fn spawn_progress_listener(
    bus_name: String,
    actions: UnboundedSender<UiAction>,
    check_active: Arc<AtomicBool>,
    catchup_tx: UnboundedSender<()>,
) {
    tokio::spawn(async move {
        loop {
            let proxy = match daemon_proxy(&bus_name).await {
                Ok(proxy) => proxy,
                Err(error) => {
                    tracing::debug!(%error, "progress listener waiting for the daemon");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };
            let mut stream = match proxy.receive_signal("CheckProgress").await {
                Ok(stream) => stream,
                Err(error) => {
                    tracing::warn!(%error, "could not subscribe to check progress");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };
            tracing::info!("listening for check progress signals");
            while let Some(message) = stream.next().await {
                match message.body::<String>() {
                    Ok(line) => {
                        if !check_active.swap(true, Ordering::Relaxed) {
                            let _ = catchup_tx.send(());
                        }
                        let _ = actions.send(UiAction::CheckProgress(line));
                    }
                    Err(error) => tracing::debug!(%error, "undecodable progress signal"),
                }
            }
        }
    });
}

/// notify-rust spins up its own tokio runtime on the calling thread, which
/// panics with "Cannot start a runtime from within a runtime" when called on a
/// tokio worker. spawn_blocking runs it on a blocking thread instead.
async fn notify_update_available(state: &UpdateState, actions: UnboundedSender<UiAction>) {
    let body = format!(
        "Update available for {} ({})",
        state.system_name, state.flake_uri
    );
    match tokio::task::spawn_blocking(move || -> Result<()> {
        let handle = notify_rust::Notification::new()
            .summary("System updates")
            .body(&body)
            .icon(ICON_NAME)
            .appname(NOTIFICATION_APP_NAME)
            .action("open", "Open")
            .timeout(notify_rust::Timeout::Milliseconds(10000))
            .show()?;
        // Waiting for the action blocks until the button is pressed or the
        // timeout closes the notification, so it gets a plain thread: on a
        // tokio worker it would stall the poller for that whole time.
        std::thread::spawn(move || {
            handle.wait_for_action(|action| {
                if action == "open" {
                    let _ = actions.send(UiAction::OpenWindow);
                }
            });
        });
        Ok(())
    })
    .await
    {
        Ok(Ok(())) => tracing::info!("notification sent"),
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

#[derive(Clone, Copy)]
struct StoreStats {
    /// None while neither the database estimate nor a du walk has produced a
    /// number; the disk rows do not depend on it.
    store_bytes: Option<u64>,
    disk_total_bytes: u64,
    disk_free_bytes: u64,
    /// False while the number came from the database estimate rather than a
    /// du walk, so the label can mark it as approximate.
    accurate: bool,
}

/// df reports the size of the filesystem /nix/store lives on, which is not
/// necessarily the root filesystem. It is cheap enough for every refresh.
fn collect_disk_stats() -> Result<(u64, u64)> {
    let df = Command::new("df")
        .args(["-B1", "--output=size,avail", "/nix/store"])
        .output()
        .context("running df")?;
    if !df.status.success() {
        bail!("df failed: {}", text(&df.stderr));
    }
    // Line 0 is the header, line 1 is the padded data row.
    let df_output = text(&df.stdout);
    let row = df_output
        .lines()
        .nth(1)
        .context("df produced no data line")?;
    let mut columns = row.split_whitespace();
    let disk_total_bytes: u64 = columns
        .next()
        .context("df size column missing")?
        .parse()
        .context("df size was not a number")?;
    let disk_free_bytes: u64 = columns
        .next()
        .context("df avail column missing")?
        .parse()
        .context("df avail was not a number")?;
    Ok((disk_total_bytes, disk_free_bytes))
}

/// True when `program` is somewhere on PATH, checked without spawning it.
fn have_program(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| directory.join(program).is_file())
    })
}

/// Blocks while du walks the store, so it is only ever called from
/// spawn_blocking. The walk runs at the lowest CPU priority and, when
/// ionice is installed, at idle I/O priority, so reading the whole store
/// cannot compete with the desktop. du reports on-disk usage (block
/// allocation).
fn du_store_bytes() -> Result<u64> {
    let mut command = if have_program("ionice") {
        let mut command = Command::new("ionice");
        command.args([
            "-c",
            "3",
            "nice",
            "-n",
            "19",
            "du",
            "-s",
            "-B1",
            "/nix/store",
        ]);
        command
    } else {
        let mut command = Command::new("nice");
        command.args(["-n", "19", "du", "-s", "-B1", "/nix/store"]);
        command
    };
    let du = command.output().context("running du")?;
    if !du.status.success() {
        bail!("du failed: {}", text(&du.stderr));
    }
    let store_bytes = text(&du.stdout)
        .split_whitespace()
        .next()
        .context("du produced no output")?
        .parse()
        .context("du output was not a byte count")?;
    Ok(store_bytes)
}

/// The accurate numbers: du for the store plus a fresh df. The slow half of
/// the refresh, so spawn_store_stats gates it on visibility and a throttle.
fn collect_store_stats() -> Result<StoreStats> {
    let store_bytes = du_store_bytes()?;
    let (disk_total_bytes, disk_free_bytes) = collect_disk_stats()?;
    Ok(StoreStats {
        store_bytes: Some(store_bytes),
        disk_total_bytes,
        disk_free_bytes,
        accurate: true,
    })
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

fn format_gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1024f64.powi(3))
}

fn percent_of(part: u64, total: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    part.saturating_mul(100) / total
}

/// df plus a nix database estimate of the store: nix reports the summed
/// `narSize` of every registered path straight from its database, which is
/// seconds where a du walk of the store takes minutes. The estimate counts
/// only registered paths, so it can differ from what du reports.
async fn approximate_store_bytes() -> Result<u64> {
    let output = tokio::process::Command::new("nix")
        .args(["path-info", "--all", "--json"])
        .output()
        .await
        .context("running nix path-info")?;
    if !output.status.success() {
        bail!("nix path-info failed: {}", text(&output.stderr));
    }
    // nix prints a JSON object keyed by store path; older releases print an
    // array of the same info objects.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Report {
        Paths(HashMap<String, Info>),
        List(Vec<Info>),
    }
    #[derive(Deserialize)]
    struct Info {
        #[serde(rename = "narSize", default)]
        nar_size: u64,
    }
    let report: Report =
        serde_json::from_slice(&output.stdout).context("nix path-info output was not JSON")?;
    Ok(match report {
        Report::Paths(paths) => paths.values().map(|info| info.nar_size).sum(),
        Report::List(list) => list.iter().map(|info| info.nar_size).sum(),
    })
}

/// The fast half of a refresh: a fresh df and the database estimate, sent
/// immediately. When the estimate fails, the last store number is kept if
/// there is one, so a missing nix or an unreadable database only leaves the
/// store row stale rather than taking the disk rows with it.
async fn fast_store_stats(previous: Option<StoreStats>) -> Result<StoreStats> {
    let (disk_total_bytes, disk_free_bytes) =
        match tokio::task::spawn_blocking(collect_disk_stats).await {
            Ok(disk) => disk?,
            Err(error) => bail!("disk stats task panicked: {error}"),
        };
    let (store_bytes, accurate) = match approximate_store_bytes().await {
        Ok(store_bytes) => (Some(store_bytes), false),
        Err(error) => match previous {
            Some(previous) => {
                tracing::warn!(%error, "store estimate failed, reusing the last number");
                (previous.store_bytes, previous.accurate)
            }
            None => {
                tracing::warn!(
                    %error,
                    "store estimate failed, showing the disk numbers only"
                );
                (None, false)
            }
        },
    };
    Ok(StoreStats {
        store_bytes,
        disk_total_bytes,
        disk_free_bytes,
        accurate,
    })
}

/// Sends one fast refresh and returns the stats it showed, so the next call
/// can reuse the store number if the estimate fails again.
async fn refresh_fast_stats(
    actions: &UnboundedSender<UiAction>,
    previous: Option<StoreStats>,
) -> Option<StoreStats> {
    match fast_store_stats(previous).await {
        Ok(stats) => {
            let _ = actions.send(UiAction::StoreStats(Ok(Box::new(stats))));
            Some(stats)
        }
        Err(reason) => {
            let _ = actions.send(UiAction::StoreStats(Err(reason.to_string())));
            None
        }
    }
}

/// Sends the accurate du-based numbers. A failed walk keeps whatever the fast
/// refresh already showed, so a store that du cannot read still gets the
/// estimate.
async fn refresh_accurate_stats(actions: &UnboundedSender<UiAction>) -> Option<StoreStats> {
    match tokio::task::spawn_blocking(collect_store_stats).await {
        Ok(Ok(stats)) => {
            let _ = actions.send(UiAction::StoreStats(Ok(Box::new(stats))));
            Some(stats)
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "du walk failed, keeping the current store number");
            None
        }
        Err(error) => {
            tracing::warn!(error = %error, "store stats task panicked");
            None
        }
    }
}

/// Waits for a trigger, then refreshes only while the window is visible:
/// nobody can see the numbers otherwise, and the expensive half is the point
/// of the gating. Every trigger updates the disk numbers and the database
/// estimate right away; the accurate du walk follows only when at least one
/// check interval has passed since the last one, because it walks the whole
/// store. Showing the window sends its own trigger, so reopening catches up
/// immediately. A trigger that lands while a walk is running is served by the
/// run already in progress, so it is dropped rather than queued up for an
/// immediate second walk.
fn spawn_store_stats(
    actions: UnboundedSender<UiAction>,
    mut stats_rx: UnboundedReceiver<()>,
    window_visible: Arc<AtomicBool>,
    du_interval: Duration,
) {
    tokio::spawn(async move {
        let mut latest: Option<StoreStats> = None;
        let mut last_du: Option<Instant> = None;
        loop {
            if window_visible.load(Ordering::Relaxed) {
                if let Some(stats) = refresh_fast_stats(&actions, latest).await {
                    latest = Some(stats);
                }
                let du_due = last_du.is_none_or(|start| start.elapsed() >= du_interval);
                if du_due {
                    last_du = Some(Instant::now());
                    if let Some(stats) = refresh_accurate_stats(&actions).await {
                        latest = Some(stats);
                    }
                }
            }
            while stats_rx.try_recv().is_ok() {}
            if stats_rx.recv().await.is_none() {
                break;
            }
        }
    });
}
