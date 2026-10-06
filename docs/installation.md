## Installation

### Using the NixOS module (recommended)

Add the flake to your inputs and enable the module:

```nix
# flake.nix
{
  inputs.simple-nix-update-gui.url = "path:/path/to/simple-nix-update-gui"; # or github:owner/repo
  # ...
}
```

```nix
# configuration.nix / host module
{
  imports = [ inputs.simple-nix-update-gui.nixosModules.default ];
  
  services.simple-nix-update-gui = {
    enable = true;
    flakeUri = "path:/etc/nixos"; # or github:owner/repo
    # systemName = "my-host"; # defaults to hostname
    checkInterval = "1h";
    autoNotify = true;
    useNom = true;
    # trayAutostart = true; # start the status icon with the session
  };
}
```

This will:
- Install the GUI and `nix-output-monitor` packages
- Add a launcher entry that opens the window, with every setting passed as a flag
- Start the tray icon as a systemd user service with the session, restarting it
  on failure, unless `trayAutostart` is set to false
- Register the D-Bus service and policy files on the system bus
- Start the daemon as a systemd service that owns the bus name, checks once at
  startup, and then checks on `checkInterval` from its own loop
- Configure polkit rules to allow wheel group to reboot

### Manual installation with flake

```bash
# Run GUI directly
nix run .#simple-nix-update-gui

# Run the tray icon only
nix run .#simple-nix-update-gui -- --tray --flake-uri path:/etc/nixos

# Run daemon directly (needs the system bus and root)
nix run .#simple-nix-update-gui-daemon -- --flake-uri path:/etc/nixos
```

### Development shell

```bash
nix develop
```

This provides `cargo`, `rustc`, `rustfmt`, `clippy`, `rust-analyzer`, and all required build dependencies.