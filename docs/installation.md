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
  };
}
```

This will:
- Install the GUI and `nix-output-monitor` packages
- Start the daemon as a systemd service with a timer for periodic checks
- Configure polkit rules to allow wheel group to reboot

### Manual installation with flake

```bash
# Run GUI directly
nix run .#simple-nix-update-gui

# Run daemon directly
nix run .#simple-nix-update-gui-daemon -- --flake-uri path:/etc/nixos
```

### Development shell

```bash
nix develop
```

This provides `cargo`, `rustc`, `rustfmt`, `clippy`, `rust-analyzer`, and all required build dependencies.