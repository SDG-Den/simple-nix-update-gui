self: {
  config,
  lib,
  pkgs,
  ...
}:
with lib; let
  cfg = config.services.simple-nix-update-gui;
  inherit (self.packages.${pkgs.stdenv.hostPlatform.system}) gui daemon;

  systemName =
    if cfg.systemName == null
    then config.networking.hostName
    else cfg.systemName;

  # One rendering of every setting, shared by the daemon unit, the launcher entry
  # and the tray entry, so no binary can be launched with a partial set.
  cliFlags = concatStringsSep " " (
    [
      "--flake-uri=${cfg.flakeUri}"
      "--system-name=${systemName}"
      "--use-nom=${boolToString cfg.useNom}"
      "--auto-notify=${boolToString cfg.autoNotify}"
      "--check-interval=${cfg.checkInterval}"
      "--bus-name=${cfg.busName}"
    ]
  );

  # Booleans are always written out, including when false, because both binaries
  # fall back to true when a variable is absent.
  settingsEnv = [
    "SNU_FLAKE_URI=${cfg.flakeUri}"
    "SNU_SYSTEM_NAME=${systemName}"
    "SNU_CHECK_INTERVAL=${cfg.checkInterval}"
    "SNU_BUS_NAME=${cfg.busName}"
    "SNU_AUTO_NOTIFY=${boolToString cfg.autoNotify}"
    "SNU_USE_NOM=${boolToString cfg.useNom}"
  ];

  # The daemon owns a name on the system bus, which needs a service file so the bus
  # knows the executable, and a policy file so only root may own the name while any
  # caller may invoke the interface. Both are named after cfg.busName, and
  # services.dbus.packages picks up share/dbus-1/system-services and
  # share/dbus-1/system.d from a package.
  dbusFiles = pkgs.runCommand "simple-nix-update-gui-dbus" {
    serviceFile = pkgs.writeText "${cfg.busName}.service" ''
      [D-BUS Service]
      Name=${cfg.busName}
      Exec=${daemon}/bin/simple-nix-update-gui-daemon
      User=root
      SystemdService=simple-nix-update-gui-daemon.service
    '';

    policyFile = pkgs.writeText "${cfg.busName}.conf" ''
      <?xml version="1.0" encoding="UTF-8"?>
      <!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN"
       "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
      <busconfig>
        <policy user="root">
          <allow own="${cfg.busName}"/>
        </policy>
        <policy context="default">
          <allow send_destination="${cfg.busName}"/>
          <allow send_interface="org.simple_nix_update_gui.Daemon"/>
        </policy>
      </busconfig>
    '';
  } ''
    mkdir -p $out/share/dbus-1/system-services $out/share/dbus-1/system.d
    cp $serviceFile $out/share/dbus-1/system-services/
    cp $policyFile $out/share/dbus-1/system.d/
  '';

  daemonPath = concatStringsSep ":" [
    "${pkgs.nix}/bin"
    "${pkgs.git}/bin"
    "${pkgs.nix-output-monitor}/bin"
    "${pkgs.openssh}/bin"
    "${pkgs.coreutils}/bin"
    "${pkgs.bash}/bin"
    "$PATH"
  ];
in
{
  options.services.simple-nix-update-gui = {
    enable = mkEnableOption "Simple NixOS update GUI and state daemon";

    flakeUri = mkOption {
      type = types.str;
      description = ''
        Flake URI to check against (any nix flake URI format).
        Example: github:owner/repo, git+https://github.com/owner/repo, path:/etc/nixos
      '';
    };

    systemName = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = ''
        System configuration name to check/update.
        If null, defaults to the current hostname (matching nixos-rebuild --flake behavior).
      '';
    };

    busName = mkOption {
      type = types.str;
      default = "org.simple_nix_update_gui.Daemon";
      description = ''
        Well known name the daemon owns on the system bus. The GUI is told the same
        name, so changing it here needs no change elsewhere.
      '';
    };

    checkInterval = mkOption {
      type = types.str;
      default = "1h";
      example = "30m";
      description = ''
        How often to check for updates. Used by the daemon's own interval and by the
        GUI's poll interval. Supports s, m, h, d.
      '';
    };

    autoNotify = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Send a desktop notification when an update becomes available. The GUI sends
        it, since only the GUI runs inside the session that owns the tray.
      '';
    };

    useNom = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Use nix-output-monitor (nom) for build output in integrated terminal if available.
      '';
    };

    trayAutostart = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Start the tray icon when the desktop session comes up. Set to false to only
        get the launcher entry.
      '';
    };
  };

  config = mkIf cfg.enable {
    environment.systemPackages = [
      gui
      pkgs.nix-output-monitor
      (pkgs.makeDesktopItem {
        name = "simple-nix-update-gui";
        exec = "${gui}/bin/simple-nix-update-gui ${cliFlags}";
        desktopName = "Simple Nix Update GUI";
        icon = "system-software-update";
        comment = "Simple NixOS update GUI";
        categories = [
          "System"
          "Utility"
        ];
        terminal = false;
      })
    ];

    services.dbus.packages = [ dbusFiles ];

    environment.etc = optionalAttrs cfg.trayAutostart {
      "xdg/autostart/simple-nix-update-gui-tray.desktop".source = pkgs.makeDesktopItem {
        name = "simple-nix-update-gui-tray";
        exec = "${gui}/bin/simple-nix-update-gui ${cliFlags} --tray";
        desktopName = "Simple Nix Update GUI";
        icon = "system-software-update";
        comment = "Tray icon for the Simple NixOS update GUI";
        terminal = false;
      };
    };

    systemd.services.simple-nix-update-gui-daemon = {
      description = "Simple Nix update GUI state daemon";
      wantedBy = ["multi-user.target"];
      after = ["network-online.target"];
      wants = ["network-online.target"];
      unitConfig = {
        StartLimitIntervalSec = "300";
        StartLimitBurst = 5;
      };
      serviceConfig = {
        Type = "dbus";
        BusName = cfg.busName;
        Restart = "on-failure";
        RestartSec = "60s";
        User = "root";
        Environment =
          settingsEnv
          ++ [
            "PATH=${daemonPath}"
            "RUST_BACKTRACE=1"
          ];
        ExecStart = "${daemon}/bin/simple-nix-update-gui-daemon";
      };
    };

    security.polkit.extraConfig = ''
      polkit.addRule(function(action, subject) {
        if (subject.isInGroup("wheel")) {
          if (action.id == "org.freedesktop.login1.reboot" ||
              action.id == "org.freedesktop.login1.reboot-multiple-sessions" ||
              action.id == "org.freedesktop.login1.power-off" ||
              action.id == "org.freedesktop.login1.power-off-multiple-sessions") {
            return polkit.Result.YES;
          }
        }
      });
    '';
  };
}