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

  config = mkIf cfg.enable {
    home.packages = [ cfg.package ];
    xdg.configFile."cal-tui/config.toml".source = if cfg.checkConfig then checkedFile else rawFile;
  };
}
