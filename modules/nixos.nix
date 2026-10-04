self: {
  config,
  lib,
  pkgs,
  ...
}:
with lib; let
  cfg = config.services.simple-nix-update-gui;
  flakeUri = cfg.flakeUri;
  inherit (self.packages.${pkgs.stdenv.hostPlatform.system}) gui daemon;
  systemName =
    if cfg.systemName == null
    then config.networking.hostName
    else cfg.systemName;
in {
  options.services.simple-nix-update-gui = {
    enable = mkEnableOption "Simple NixOS update GUI and notification daemon";

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

    checkInterval = mkOption {
      type = types.str;
      default = "1h";
      example = "30m";
      description = ''
        How often to check for updates (systemd timer format).
      '';
    };

    autoNotify = mkOption {
      type = types.str;
      default = "true";
      description = ''
        Send desktop notifications when updates are available.
      '';
    };

    useNom = mkOption {
      type = types.str;
      default = "true";
      description = ''
        Use nix-output-monitor (nom) for build output in integrated terminal if available.
      '';
    };
  };

  config = mkIf cfg.enable {
    environment.systemPackages = [
      gui
      pkgs.nix-output-monitor
    ];

    systemd.services.simple-nix-update-gui-daemon = {
      description = "Simple Nix update GUI notification daemon";
      wantedBy = ["multi-user.target"];
      after = ["network-online.target"];
      wants = ["network-online.target"];
      serviceConfig = {
        Type = "simple";
        Restart = "on-failure";
        RestartSec = "10s";
        User = "root";
        Environment = [
          "SNU_FLAKE_URI=${flakeUri}"
          "SNU_SYSTEM_NAME=${systemName}"
          "SNU_CHECK_INTERVAL=${cfg.checkInterval}"
          "SNU_AUTO_NOTIFY=${toString cfg.autoNotify}"
          "SNU_USE_NOM=${toString cfg.useNom}"
          "PATH=${pkgs.nix}/bin:${pkgs.git}/bin:${pkgs.nix-output-monitor}/bin:${pkgs.openssh}/bin:${pkgs.coreutils}/bin:${pkgs.bash}/bin:$PATH"
        ];
        ExecStart = "${daemon}/bin/simple-nix-update-gui-daemon";
      };
    };

    systemd.timers.simple-nix-update-gui-daemon = {
      description = "Timer for simple-nix-update-gui daemon update checks";
      wantedBy = ["timers.target"];
      timerConfig = {
        OnBootSec = "1m";
        OnUnitActiveSec = cfg.checkInterval;
        Persistent = true;
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
