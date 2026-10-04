use anyhow::Result;
use clap::Parser;
use gtk4::prelude::*;
use gtk4::{self, ApplicationWindow, Box, Button, Dialog, Label, Orientation, ScrolledWindow};
use libadwaita::prelude::*;
use libadwaita::Application as AdwApplication;
use serde::{Deserialize, Serialize};
use std::env;
use std::process::Command;
use vte4::prelude::*;
use vte4::{Terminal, PtyFlags};
use chrono;

#[derive(Parser, Debug, Clone)]
#[command(name = "simple-nix-update-gui")]
#[command(about = "Simple Nix update GUI")]
struct Args {
    #[arg(long, env = "SNU_FLAKE_URI")]
    flake_uri: Option<String>,
    #[arg(long, env = "SNU_SYSTEM_NAME")]
    system_name: Option<String>,
    #[arg(long, env = "SNU_USE_NOM", default_value = "true")]
    use_nom: bool,
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

fn get_hostname() -> String {
    hostname::get()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let flake_uri = args.flake_uri.clone().unwrap_or_else(|| {
        env::var("SNU_FLAKE_URI").unwrap_or_else(|_| "path:/etc/nixos".to_string())
    });
    let system_name = args.system_name.clone().unwrap_or_else(|| {
        env::var("SNU_SYSTEM_NAME").unwrap_or_else(|_| get_hostname())
    });
    let use_nom = args.use_nom;

    // Get state from daemon if available
    let state = get_daemon_state().await.unwrap_or_else(|_| UpdateState {
        has_update: false,
        current_system: get_current_system().unwrap_or_else(|_| "unknown".to_string()),
        remote_system: None,
        last_check: chrono::Local::now().to_rfc3339(),
        flake_uri: flake_uri.clone(),
        system_name: system_name.clone(),
    });

    let app = AdwApplication::new(Some("org.simple_nix_update_gui"), Default::default());
    let flake_uri_clone = flake_uri.clone();
    let system_name_clone = system_name.clone();
    let state_clone = state.clone();
    let use_nom_clone = use_nom;

    app.connect_activate(move |app| {
        build_ui(
            app,
            &flake_uri_clone,
            &system_name_clone,
            &state_clone,
            use_nom_clone,
        );
    });

    app.run_with_args(&[] as &[&str]);
    Ok(())
}

fn get_hostname() -> String {
    hostname::get()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

fn get_current_system() -> Result<String> {
    let output = Command::new("readlink")
        .arg("/run/booted-system")
        .output()?;
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

async fn get_daemon_state() -> Result<UpdateState> {
    let conn = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(
        &conn,
        "org.simple_nix_update_gui.Daemon",
        "/org/simple_nix_update_gui/Daemon",
        "org.simple_nix_update_gui.Daemon",
    )
    .await?;
    let state_str: String = proxy.call("GetState", &()).await?;
    let state: UpdateState = serde_json::from_str(&state_str)?;
    Ok(state)
}

fn build_ui(
    app: &AdwApplication,
    flake_uri: &str,
    system_name: &str,
    state: &UpdateState,
    use_nom: bool,
) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Simple Nix Update GUI")
        .default_width(600)
        .default_height(400)
        .build();

    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(20);
    vbox.set_margin_bottom(20);
    vbox.set_margin_start(20);
    vbox.set_margin_end(20);

    let flake_label = Label::builder()
        .label(&format!("Flake URI: {}", flake_uri))
        .xalign(0.0)
        .wrap(true)
        .build();

    let system_label = Label::builder()
        .label(&format!("System: {}", system_name))
        .xalign(0.0)
        .build();

    let current_label = Label::builder()
        .label(&format!("Current: {}", state.current_system))
        .xalign(0.0)
        .wrap(true)
        .build();

    let remote_label = if let Some(ref r) = state.remote_system {
        Label::builder()
            .label(&format!("Remote: {}", r))
            .xalign(0.0)
            .wrap(true)
            .build()
    } else {
        Label::builder()
            .label("Remote: (check failed or no data)")
            .xalign(0.0)
            .build()
    };

    let status_label = Label::builder()
        .label(&format!("Status: {}", if state.has_update { "Update available" } else { "Up to date" }))
        .xalign(0.0)
        .build();

    let last_check_label = Label::builder()
        .label(&format!("Last check: {}", state.last_check))
        .xalign(0.0)
        .build();

    let update_btn = Button::builder()
        .label("Update...")
        .sensitive(state.has_update)
        .build();
    let flake_uri_c = flake_uri.to_string();
    let system_name_c = system_name.to_string();
    let use_nom_c = use_nom;
    update_btn.connect_clicked(move |_| {
        show_update_dialog(&flake_uri_c, &system_name_c, use_nom_c);
    });

    let reboot_btn = Button::builder().label("Check reboot needed").build();
    reboot_btn.connect_clicked(|_| {
        check_and_prompt_reboot();
    });

    vbox.append(&flake_label);
    vbox.append(&system_label);
    vbox.append(&current_label);
    vbox.append(&remote_label);
    vbox.append(&status_label);
    vbox.append(&last_check_label);
    vbox.append(&update_btn);
    vbox.append(&reboot_btn);

    window.set_child(Some(&vbox));
    window.present();
}

fn show_update_dialog(flake_uri: &str, system_name: &str, use_nom: bool) {
    let dialog = Dialog::builder()
        .title("Choose update action")
        .default_width(500)
        .build();

    let content = dialog.content_area();
    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let info = Label::builder()
        .label(&format!("Flake: {}#{}", flake_uri, system_name))
        .xalign(0.0)
        .wrap(true)
        .build();

    let test_btn = Button::builder().label("Test build (nixos-rebuild build)").build();
    let boot_btn = Button::builder().label("Rebuild & boot (nixos-rebuild boot)").build();
    let switch_btn = Button::builder().label("Rebuild & switch (nixos-rebuild switch)").build();

    let flake_uri_c = flake_uri.to_string();
    let system_name_c = system_name.to_string();
    let use_nom_c = use_nom;
    test_btn.connect_clicked(move |_| {
        run_nixos_rebuild("build", &flake_uri_c, &system_name_c, use_nom_c);
    });

    let flake_uri_c = flake_uri.to_string();
    let system_name_c = system_name.to_string();
    let use_nom_c = use_nom;
    boot_btn.connect_clicked(move |_| {
        run_nixos_rebuild("boot", &flake_uri_c, &system_name_c, use_nom_c);
    });

    let flake_uri_c = flake_uri.to_string();
    let system_name_c = system_name.to_string();
    let use_nom_c = use_nom;
    switch_btn.connect_clicked(move |_| {
        run_nixos_rebuild("switch", &flake_uri_c, &system_name_c, use_nom_c);
    });

    vbox.append(&info);
    vbox.append(&test_btn);
    vbox.append(&boot_btn);
    vbox.append(&switch_btn);
    content.append(&vbox);

    dialog.show();
}

fn run_nixos_rebuild(action: &str, flake_uri: &str, system_name: &str, use_nom: bool) {
    let dialog = Dialog::builder()
        .title(&format!("nixos-rebuild {}", action))
        .default_width(900)
        .default_height(600)
        .build();

    let content = dialog.content_area();
    let terminal = Terminal::new();
    let pty_flags = PtyFlags::DEFAULT;
    terminal.spawn_async(
        pty_flags,
        None,
        &["/bin/sh", "-c", &build_command(action, flake_uri, system_name, use_nom)],
        &[],
        glib::SpawnFlags::DEFAULT,
        || {},
        -1,
        None::<&gio::Cancellable>,
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

fn build_command(action: &str, flake_uri: &str, system_name: &str, use_nom: bool) -> String {
    let rebuild = format!(
        "nixos-rebuild {} --flake '{}#{}'",
        action, flake_uri, system_name
    );
    if use_nom {
        // Try nom, fallback to cat if not available
        format!("if command -v nom >/dev/null 2>&1; then {} 2>&1 | nom; else {} 2>&1; fi", rebuild, rebuild)
    } else {
        format!("{} 2>&1", rebuild)
    }
}

fn check_and_prompt_reboot() {
    // Check if reboot needed
    let is_needed = is_reboot_needed_sync();
    if !is_needed {
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
    // Show reboot dialog
    let dialog = gtk4::MessageDialog::builder()
        .modal(true)
        .text("Reboot needed")
        .secondary_text("A reboot is required to activate the new system.")
        .message_type(gtk4::MessageType::Question)
        .buttons(gtk4::ButtonsType::YesNo)
        .build();
    dialog.connect_response(move |d, resp| {
        if resp == gtk4::ResponseType::Yes {
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
    let profile = Command::new("readlink").arg("-f").arg("/nix/var/nix/profiles/system").output();
    match (booted, profile) {
        (Ok(b), Ok(p)) if b.status.success() && p.status.success() => {
            let bstr = String::from_utf8(b.stdout).unwrap_or_default();
            let pstr = String::from_utf8(p.stdout).unwrap_or_default();
            bstr.trim() != pstr.trim()
        }
        _ => false,
    }
}
