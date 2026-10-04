## Example NixOS Configuration

Here is a complete, working example of using this project in a NixOS configuration:

### flake.nix

```nix
{
  description = "NixOS configuration with simple-nix-update-gui";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    simple-nix-update-gui = {
      url = "github:your-username/simple-nix-update-gui";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, simple-nix-update-gui, ... }: {
    nixosConfigurations.my-host = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        simple-nix-update-gui.nixosModules.default
      ];
    };
  };
}
```

### configuration.nix

```nix
{ config, pkgs, ... }: {
  # ... other configuration ...

  services.simple-nix-update-gui = {
    enable = true;
    # Point to your flake containing the system config
    flakeUri = "path:/etc/nixos";
    # Or use a remote flake
    # flakeUri = "github:your-username/nixos-config";
    systemName = "my-host";
    checkInterval = "2h"; # Check every 2 hours
    autoNotify = true; # The GUI notifies, the daemon holds no session
    useNom = true;
    trayAutostart = true; # Status icon starts with the session
    # busName = "org.simple_nix_update_gui.Daemon"; # shared system bus name
  };

  # The module already registers the D-Bus service and policy files and gives the
  # daemon unit its BusName, so nothing else is needed for the bus.
}
```

### Environment variables example (manual use)

```bash
export SNU_FLAKE_URI="github:owner/nixos-configs"
export SNU_SYSTEM_NAME="desktop"
export SNU_CHECK_INTERVAL="30m"
export SNU_BUS_NAME="org.simple_nix_update_gui.Daemon"
export SNU_USE_NOM="true"
```

### Running the daemon without the module

The daemon connects to the **system** bus and the shipped policy file only lets root
own the bus name, so it has to run as root:

```ini
# /etc/systemd/system/simple-nix-update-gui-daemon.service
[Unit]
Description=Simple Nix update GUI state daemon

[Service]
Type=dbus
BusName=org.simple_nix_update_gui.Daemon
User=root
Environment=SNU_FLAKE_URI=path:/etc/nixos
Environment=SNU_SYSTEM_NAME=my-host
Environment=SNU_CHECK_INTERVAL=1h
ExecStart=/run/current-system/sw/bin/simple-nix-update-gui-daemon
Restart=on-failure
RestartSec=60
```

```bash
sudo systemctl enable --now simple-nix-update-gui-daemon.service
```

There is no timer unit: the daemon performs its initial check at startup and then
checks on `SNU_CHECK_INTERVAL` from its own loop. The D-Bus service and policy files
also have to be installed, which the module does for you.
