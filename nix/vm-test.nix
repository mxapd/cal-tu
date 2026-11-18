# The Jarvis service end to end on a real systemd: Radicale as the server,
# vdirsyncer syncing alice's ~/.calendars, the module's units, and a stand-in
# for Jarvis's /report that keeps every entry.
{ pkgs, self }:
let
  journal = pkgs.writeText "fake-jarvis.py" ''
    import http.server
    class H(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", 0))).decode()
            with open("/var/lib/fake-jarvis/journal", "a") as f:
                f.write(f"== {self.path}\n{body}\n")
            self.send_response(200); self.end_headers()
    http.server.HTTPServer(("127.0.0.1", 8485), H).serve_forever()
  '';
in
pkgs.testers.runNixOSTest {
  name = "cal-tui-jarvis";
  nodes.machine = { pkgs, ... }: {
    imports = [ self.nixosModules.default ];
    users.users.alice = { isNormalUser = true; };
    services.radicale = {
      enable = true;
      settings = {
        server.hosts = [ "127.0.0.1:5232" ];
        # Any user and password; alice's own collections are hers.
        auth.type = "none";
      };
    };
    systemd.services.fake-jarvis = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        ExecStart = "${pkgs.python3}/bin/python3 ${journal}";
        StateDirectory = "fake-jarvis";
      };
    };
    # The test starts the sync many times in a few seconds; its timer
    # never would.
    systemd.services.cal-tui-sync.unitConfig.StartLimitIntervalSec = 0;
    services.cal-tui.jarvis = {
      enable = true;
      user = "alice";
      # The test starts the units itself.
      syncAt = "yearly";
    };
    environment.systemPackages = [ pkgs.vdirsyncer pkgs.curl self.packages.${pkgs.stdenv.hostPlatform.system}.default ];
  };

  testScript = ''
    import datetime, shlex

    machine.wait_for_unit("radicale.service")
    machine.wait_for_unit("fake-jarvis.service")
    machine.wait_for_open_port(5232)
    machine.wait_for_open_port(8485)

    def as_alice(cmd):
        return machine.succeed(f"su - alice -c {shlex.quote(cmd)}")

    with subtest("alice: a calendar on the server, vdirsyncer, cal-tui with [jarvis]"):
        machine.succeed("curl -sf -u alice:x -X MKCALENDAR http://127.0.0.1:5232/alice/personal/")
        as_alice("mkdir -p ~/.config/vdirsyncer ~/.config/cal-tui ~/.calendars/personal")
        as_alice("""cat > ~/.config/vdirsyncer/config <<'EOF'
    [general]
    status_path = "~/.local/share/vdirsyncer/status"
    [pair personal]
    a = "remote"
    b = "local"
    collections = null
    [storage remote]
    type = "caldav"
    url = "http://127.0.0.1:5232/alice/personal/"
    username = "alice"
    password = "x"
    [storage local]
    type = "filesystem"
    path = "~/.calendars/personal/"
    fileext = ".ics"
    EOF""")
        as_alice("""cat > ~/.config/cal-tui/config.toml <<'EOF'
    calendars_dir = "~/.calendars"
    timezone = "UTC"
    sync_command = "vdirsyncer sync"
    [jarvis]
    report_url = "http://127.0.0.1:8485/report/calendar"
    EOF""")
        as_alice("yes | vdirsyncer discover")
        # The first sync only remembers what is there.
        machine.succeed("systemctl start cal-tui-sync.service")
        machine.succeed("test ! -s /var/lib/fake-jarvis/journal")
        # It really synced: a failure is retried quietly, so look.
        machine.fail("journalctl -u cal-tui-sync.service | grep 'sync failed'")

    day = (datetime.date.today() + datetime.timedelta(days=2)).strftime("%Y%m%d")
    with subtest("an event added on the server is synced and journaled"):
        ics = f"""BEGIN:VCALENDAR
    VERSION:2.0
    PRODID:-//test//EN
    BEGIN:VEVENT
    UID:dentist-1
    DTSTAMP:20260101T000000Z
    DTSTART:{day}T140000Z
    DTEND:{day}T150000Z
    SUMMARY:Dentist
    END:VEVENT
    END:VCALENDAR
    """.replace("\n", "\r\n")
        machine.succeed(f"curl -sf -u alice:x -X PUT -H 'Content-Type: text/calendar' --data-binary {shlex.quote(ics)} http://127.0.0.1:5232/alice/personal/dentist-1.ics")
        # A second one: vdirsyncer refuses to sync a calendar emptied
        # completely (a safety check), and the test deletes the dentist.
        keep = ics.replace("dentist-1", "keep-1").replace("Dentist", "Standup").replace("T14", "T09").replace("T15", "T10")
        machine.succeed(f"curl -sf -u alice:x -X PUT -H 'Content-Type: text/calendar' --data-binary {shlex.quote(keep)} http://127.0.0.1:5232/alice/personal/keep-1.ics")
        machine.succeed("systemctl start cal-tui-sync.service")
        machine.succeed("ls /home/alice/.calendars/personal/ | grep -q .ics")
        journal = machine.succeed("cat /var/lib/fake-jarvis/journal")
        assert "tags=changed" in journal and "2 new events." in journal and "Dentist" in journal, journal

    with subtest("Jarvis's socket starts serve; an edit is journaled and pushed to the server"):
        out = machine.succeed("curl -sf --unix-socket /run/cal-tui/api.sock http://x/describe")
        assert '"service":"calendar"' in out, out
        machine.succeed("systemctl is-active cal-tui-api.service")
        out = machine.succeed("curl -sf --unix-socket /run/cal-tui/api.sock -X POST -d '{\"from\":\"+2d\",\"days\":1}' http://x/agenda")
        assert "Dentist" in out, out
        out = machine.succeed("curl -sf --unix-socket /run/cal-tui/api.sock -X POST -d '{\"id\":\"dentist-1\"}' http://x/delete_event")
        assert "Deleted: Dentist" in out, out
        # Pushed at once, not at the next timer run.
        machine.wait_until_succeeds("test \"$(curl -s -u alice:x -o /dev/null -w '%{http_code}' http://127.0.0.1:5232/alice/personal/dentist-1.ics)\" = 404", timeout=30)
        journal = machine.succeed("cat /var/lib/fake-jarvis/journal")
        assert "tags=edited" in journal and "Deleted: Dentist" in journal, journal
        # Not reported a second time by the next sync.
        before = journal.count("==")
        machine.succeed("systemctl start cal-tui-sync.service")
        assert machine.succeed("cat /var/lib/fake-jarvis/journal").count("==") == before

    with subtest("the server gone: quiet retries, one alert, then resolved"):
        machine.succeed("systemctl stop radicale.service")
        for i in range(5):
            machine.succeed("systemctl start cal-tui-sync.service")
        machine.fail("systemctl start cal-tui-sync.service")    # the 6th: Jarvis's #failed
        # The alert names the cause, not vdirsyncer's hint about -vdebug.
        unit_log = machine.succeed("journalctl -u cal-tui-sync.service")
        assert "failed 6 times in a row" in unit_log and "Cannot connect to host" in unit_log.split("failed 6 times in a row")[1].splitlines()[0], unit_log
        machine.succeed("systemctl start cal-tui-sync.service")  # not again for the same outage
        machine.succeed("systemctl start radicale.service")
        machine.wait_for_open_port(5232)
        machine.succeed("systemctl start cal-tui-sync.service")
        journal = machine.succeed("cat /var/lib/fake-jarvis/journal")
        assert "tags=resolved" in journal and "works again, after 7 failed attempts" in journal, journal

    with subtest("the morning digest"):
        machine.succeed("systemctl start cal-tui-digest.service")
        journal = machine.succeed("cat /var/lib/fake-jarvis/journal")
        assert "tags=agenda" in journal, journal
  '';
}
