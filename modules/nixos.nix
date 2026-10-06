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

  # Values are quoted because these flags end up in the Exec line of a desktop
  # entry, where the Desktop Entry Specification lists ?, #, and & as reserved
  # characters. Flake URIs use them, for example git+https://host/repo?ref=main.
  flag = name: value: "--${name}=\"${value}\"";

  # One rendering of every setting, shared by the launcher entry and the tray
  # entry, so no binary can be launched with a partial set.
  cliFlags = concatStringsSep " " [
    (flag "flake-uri" cfg.flakeUri)
    (flag "system-name" systemName)
    (flag "use-nom" (boolToString cfg.useNom))
    (flag "auto-notify" (boolToString cfg.autoNotify))
    (flag "check-interval" cfg.checkInterval)
    (flag "bus-name" cfg.busName)
  ];

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
        Well known name the daemon owns on the session bus. The GUI is told the same
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

    # No D-Bus activation file. The session bus only consults those under a
    # per-user data dir, and systemd.user.services already starts the daemon at
    # login, so activation would be redundant.

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

    # A user unit, not a system one, so it runs as whoever logged in. That is
    # what lets the eval reach a private flake: nix shells out to git, which
    # reads the credentials in that user's ~/.git-credentials. default.target is
    # reached at every login, so this starts per session without a login hook.
    # No network-online.target: the user manager has no such unit, and a failed
    # eval surfaces to the GUI and is retried on the next interval anyway.
    systemd.user.services.simple-nix-update-gui-daemon = {
      description = "Simple Nix update GUI state daemon";
      wantedBy = ["default.target"];
      unitConfig = {
        StartLimitIntervalSec = "300";
        StartLimitBurst = 5;
      };
      serviceConfig = {
        Type = "dbus";
        BusName = cfg.busName;
        Restart = "on-failure";
        RestartSec = "60s";
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