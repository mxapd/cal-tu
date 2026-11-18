# home-manager module for cal-tui.
#
#   programs.cal-tui = {
#     enable = true;
#     settings = {
#       timezone = "Europe/Stockholm";
#       default_calendar = "personal";
#       sync_command = "vdirsyncer sync";
#       calendars.uni = { color = "cyan"; read_only = true; };
#     };
#   };
#   services.cal-tui-notify.enable = true;   # reminders as desktop notifications
#   services.cal-tui-serve = {               # the calendars for Jarvis (and its MCP)
#     enable = true;
#     listen = "100.64.0.5:8487";            # your tailnet address
#     tokenFile = "/run/user/1000/secrets/cal-tui-token";
#   };
#
# `settings` is written to ~/.config/cal-tui/config.toml. The generated file
# is checked with `cal-tui check-config` at build time, so a typo or an
# invalid value fails `home-manager switch` / `nixos-rebuild` instead of the
# app. Keys without a typed option below are passed through as-is (and still
# checked), so new app settings work before this module learns about them.
self:
{ config, lib, pkgs, ... }:
let
  cfg = config.programs.cal-tui;
  notifyCfg = config.services.cal-tui-notify;
  serveCfg = config.services.cal-tui-serve;
  tomlFormat = pkgs.formats.toml { };
  inherit (lib) mkOption mkEnableOption mkIf types literalExpression;

  nullOr = t: description: mkOption {
    type = types.nullOr t;
    default = null;
    inherit description;
  };

  calendarType = types.submodule {
    freeformType = tomlFormat.type;
    options = {
      name = nullOr types.str "Display name. Defaults to the vdir `displayname` file, then the folder name.";
      color = nullOr types.str "Colour name (`red`, `cyan`, …) or `#rrggbb`. Defaults to the vdir `color` file.";
      read_only = mkOption {
        type = types.bool;
        default = false;
        description = "Refuse all edits to this calendar (e.g. a subscribed feed).";
      };
      hidden = mkOption {
        type = types.bool;
        default = false;
        description = "Do not show this calendar.";
      };
      default_reminder = nullOr types.str "Reminder for events without their own alarm, e.g. `15m`.";
      hide_matching = mkOption {
        type = types.listOf types.str;
        default = [ ];
        example = [ "^Tutorial group B" ];
        description = "Regular expressions; events whose title matches any of them are hidden.";
      };
    };
  };

  # TOML has no null; drop unset options.
  clean = lib.filterAttrsRecursive (_: v: v != null);

  rawFile = tomlFormat.generate "cal-tui-config.toml" (clean cfg.settings);

  checkedFile = pkgs.runCommand "cal-tui-config.toml" { } ''
    ${lib.getExe cfg.package} --config ${rawFile} check-config
    cp ${rawFile} $out
  '';
in
{
  options.services.cal-tui-notify = {
    enable = mkEnableOption "the cal-tui reminder service (desktop notifications for calendar reminders)";

    catchUp = mkOption {
      type = types.str;
      default = "10m";
      description = "Deliver missed reminders up to this old even if the event already started (reminders for events still ahead are always delivered).";
    };

    command = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "ntfy publish mytopic \"$CAL_TUI_SUMMARY: $CAL_TUI_BODY\"";
      description = "Run this shell command per reminder instead of a desktop notification. Details are in `$CAL_TUI_SUMMARY`, `$CAL_TUI_BODY`, `$CAL_TUI_START`, `$CAL_TUI_CALENDAR`, `$CAL_TUI_LOCATION`, `$CAL_TUI_UID`.";
    };

    target = mkOption {
      type = types.str;
      default = "graphical-session.target";
      description = "systemd user target the service is started with (and stopped with).";
    };
  };

  options.services.cal-tui-serve = {
    enable = mkEnableOption "`cal-tui serve`: the calendars over HTTP as the Jarvis Service API, for Jarvis to offer to AIs over MCP";

    listen = mkOption {
      type = types.str;
      default = "127.0.0.1:8487";
      example = "100.64.0.5:8487";
      description = "TCP address to listen on. One that is not loopback needs `tokenFile`.";
    };

    tokenFile = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/run/user/1000/secrets/cal-tui-token";
      description = "File holding the bearer token every request must carry (Jarvis: `jarvis.endpoints.<name>.tokenFile` with the same token). A path as a string, so the secret stays out of the Nix store.";
    };

    service = mkOption {
      type = types.str;
      default = "calendar";
      description = "The service name `/describe` gives; use the name of the Jarvis endpoint.";
    };

    target = mkOption {
      type = types.str;
      default = "default.target";
      description = "systemd user target the service is started with.";
    };
  };

  options.programs.cal-tui = {
    enable = mkEnableOption "cal-tui, a calcurse-style terminal calendar on vdir/CalDAV";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = literalExpression "cal-tui.packages.\${system}.default";
      description = "The cal-tui package to use.";
    };

    checkConfig = mkOption {
      type = types.bool;
      default = true;
      description = "Validate the generated config with `cal-tui check-config` at build time.";
    };

    settings = mkOption {
      default = { };
      description = "Contents of `~/.config/cal-tui/config.toml`.";
      example = literalExpression ''
        {
          timezone = "Europe/Stockholm";
          default_calendar = "personal";
          sync_command = "vdirsyncer sync";
          calendars.uni = {
            color = "cyan";
            read_only = true;
            hide_matching = [ "^Tutorial group B" ];
          };
        }
      '';
      type = types.submodule {
        freeformType = tomlFormat.type;
        options = {
          calendars_dir = nullOr types.str "Folder with one sub-folder per calendar. Default: `~/.local/share/calendars`.";
          default_calendar = nullOr types.str "Calendar (folder name) that new items go into.";
          sync_command = nullOr types.str "Command run by the `S` key, e.g. `vdirsyncer sync`.";
          timezone = nullOr types.str "IANA time zone for display and floating times. Default: the system zone.";
          calendars = mkOption {
            type = types.attrsOf calendarType;
            default = { };
            description = "Per-calendar settings, keyed by folder name.";
          };
        };
      };
    };
  };

  config = lib.mkMerge [
    (mkIf cfg.enable {
      home.packages = [ cfg.package ];
      xdg.configFile."cal-tui/config.toml".source = if cfg.checkConfig then checkedFile else rawFile;
    })

    (mkIf notifyCfg.enable {
      assertions = [{
        assertion = cfg.enable;
        message = "services.cal-tui-notify needs programs.cal-tui.enable = true (it uses the same config and package).";
      }];

      systemd.user.services.cal-tui-notify = {
        Unit = {
          Description = "cal-tui calendar reminders";
          PartOf = [ notifyCfg.target ];
          After = [ notifyCfg.target ];
          # Reload when the generated config changes.
          X-Restart-Triggers = [ config.xdg.configFile."cal-tui/config.toml".source ];
        };
        Service = {
          ExecStart = lib.escapeShellArgs (
            [ (lib.getExe cfg.package) "notify" "--catch-up" notifyCfg.catchUp ]
            ++ lib.optionals (notifyCfg.command != null) [ "--command" notifyCfg.command ]
          );
          Restart = "on-failure";
          RestartSec = 10;
        };
        Install.WantedBy = [ notifyCfg.target ];
      };
    })

    (mkIf serveCfg.enable {
      assertions = [{
        assertion = cfg.enable;
        message = "services.cal-tui-serve needs programs.cal-tui.enable = true (it uses the same config and package).";
      }];

      systemd.user.services.cal-tui-serve = {
        Unit = {
          Description = "cal-tui calendars for Jarvis (Service API over HTTP)";
          PartOf = [ serveCfg.target ];
          After = [ serveCfg.target ];
          X-Restart-Triggers = [ config.xdg.configFile."cal-tui/config.toml".source ];
        };
        Service = {
          ExecStart = lib.escapeShellArgs (
            [ (lib.getExe cfg.package) "serve" "--listen" serveCfg.listen "--service" serveCfg.service ]
            ++ lib.optionals (serveCfg.tokenFile != null) [ "--token-file" serveCfg.tokenFile ]
          );
          # A tailnet address may not be up yet at login.
          Restart = "on-failure";
          RestartSec = 10;
        };
        Install.WantedBy = [ serveCfg.target ];
      };
    })
  ];
}
