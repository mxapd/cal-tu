# NixOS module: cal-tui as a Jarvis service (an endpoint, as smartnotes is).
#
#   services.cal-tui.jarvis = { enable = true; user = "xam"; };
#   jarvis.endpoints.calendar = {
#     socket = "/run/cal-tui/api.sock";
#     socketGroup = "calendar-api";
#     units = [ "cal-tui-sync.service" "cal-tui-api.service" "cal-tui-digest.service" ];
#     timer = "cal-tui-sync.timer";
#     maxAge = "40m";
#   };
#
# Everything runs as `user`, on that user's own cal-tui config, calendars and
# sync setup: the standalone TUI and Jarvis see the same synced copy. The
# units are system units so Jarvis can watch them, and they run whether or
# not the user is logged in.
#
#   cal-tui-sync.timer    `cal-tui sync` every few minutes: sync_command
#                         (vdirsyncer), then what changed into the journal
#   cal-tui-digest.timer  `cal-tui report` each morning: the day at a glance
#   cal-tui-api.socket    the Service API for Jarvis; starts `cal-tui serve`
#                         on the first request, which exits when idle
#
# Where entries go is the user's config: `[jarvis] report_url = ...`.
self:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.cal-tui.jarvis;
  inherit (lib) mkOption mkEnableOption mkIf types literalExpression;
  user = config.users.users.${cfg.user} or { };
  exe = lib.getExe cfg.package;

  # As the user, in their home, so ~/.config and ~/.calendars are theirs.
  asUser = {
    User = cfg.user;
    Group = user.group or "users";
    Environment = [ "HOME=${user.home or "/home/${cfg.user}"}" ];
    PrivateTmp = true;
    NoNewPrivileges = true;
    ProtectSystem = "full";
  };
in
{
  options.services.cal-tui.jarvis = {
    enable = mkEnableOption "cal-tui as a Jarvis service: synced on a timer, changes and a morning digest in the journal, the Service API on a socket";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = literalExpression "cal-tui.packages.\${system}.default";
      description = "The cal-tui package to use.";
    };

    user = mkOption {
      type = types.str;
      example = "alice";
      description = "Whose cal-tui config (~/.config/cal-tui/config.toml), calendars and sync setup to use.";
    };

    path = mkOption {
      type = types.listOf types.package;
      default = [ pkgs.vdirsyncer pkgs.coreutils ];
      defaultText = literalExpression "[ pkgs.vdirsyncer pkgs.coreutils ]";
      description = "What the config's sync_command needs on PATH.";
    };

    syncAt = mkOption {
      type = types.str;
      default = "*:0/5";
      description = "When to sync (systemd OnCalendar).";
    };

    digestAt = mkOption {
      type = types.nullOr types.str;
      default = "07:00";
      description = "When to post the day at a glance (systemd OnCalendar); null for never.";
    };

    socket = mkOption {
      type = types.str;
      default = "/run/cal-tui/api.sock";
      description = "Where the Service API listens (jarvis.endpoints.<name>.socket).";
    };

    socketGroup = mkOption {
      type = types.str;
      default = "calendar-api";
      description = "The group that may open the socket (jarvis.endpoints.<name>.socketGroup; the Jarvis server joins it).";
    };

    idle = mkOption {
      type = types.ints.unsigned;
      default = 600;
      description = "Seconds without a request before `cal-tui serve` exits; the socket starts it again.";
    };
  };

  config = mkIf cfg.enable {
    assertions = [{
      assertion = config.users.users ? ${cfg.user};
      message = "services.cal-tui.jarvis.user: no user '${cfg.user}' on this host.";
    }];

    users.groups.${cfg.socketGroup} = { };

    systemd.services.cal-tui-sync = {
      description = "cal-tui: sync the calendars, journal what changed";
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      # sync_command runs through sh.
      path = [ pkgs.bash ] ++ cfg.path;
      serviceConfig = asUser // {
        Type = "oneshot";
        ExecStart = "${exe} sync";
      };
    };
    systemd.timers.cal-tui-sync = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnCalendar = cfg.syncAt;
        OnBootSec = "1m";
        Persistent = true;
      };
    };

    systemd.services.cal-tui-digest = mkIf (cfg.digestAt != null) {
      description = "cal-tui: the day at a glance, into the journal";
      wants = [ "network-online.target" ];
      after = [ "network-online.target" "cal-tui-sync.service" ];
      serviceConfig = asUser // {
        Type = "oneshot";
        ExecStart = "${exe} report";
      };
    };
    systemd.timers.cal-tui-digest = mkIf (cfg.digestAt != null) {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnCalendar = cfg.digestAt;
        Persistent = true;
      };
    };

    systemd.sockets.cal-tui-api = {
      description = "cal-tui: the Service API for Jarvis";
      wantedBy = [ "sockets.target" ];
      listenStreams = [ cfg.socket ];
      socketConfig = {
        SocketGroup = cfg.socketGroup;
        SocketMode = "0660";
      };
    };
    systemd.services.cal-tui-api = {
      description = "cal-tui: the Service API for Jarvis";
      requires = [ "cal-tui-api.socket" ];
      after = [ "cal-tui-api.socket" ];
      # An edit is pushed to the server at once, with sync_command.
      # sync_command runs through sh.
      path = [ pkgs.bash ] ++ cfg.path;
      serviceConfig = asUser // {
        ExecStart = "${exe} serve --idle ${toString cfg.idle}";
      };
    };
  };
}
