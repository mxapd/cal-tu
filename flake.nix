{
  description = "cal-tui: calcurse-style terminal calendar on vdir/CalDAV";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    # Only used by `nix flake check` to test the home-manager module.
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, home-manager }:
    let
      hmModule = import ./nix/hm-module.nix self;
    in
    {
      homeManagerModules.default = hmModule;
      homeModules.default = hmModule;
    }
    // flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        cal-tui = pkgs.rustPlatform.buildRustPackage {
          pname = "cal-tui";
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          # `cargo test` (unit, fixture, CLI and screen snapshot tests) runs
          # in the check phase, so a failing test fails the build.
          meta = {
            description = "Calcurse-style terminal calendar and todo manager on vdir/CalDAV";
            mainProgram = "cal-tui";
            license = pkgs.lib.licenses.mit;
          };
        };

        # Evaluate the home-manager module like a user would.
        hmConfig = extra:
          (home-manager.lib.homeManagerConfiguration {
            inherit pkgs;
            modules = [
              self.homeManagerModules.default
              {
                home = {
                  username = "test";
                  homeDirectory = "/home/test";
                  stateVersion = "25.05";
                };
                programs.cal-tui.enable = true;
              }
              extra
            ];
          }).config;

        # The config file the module generates for `settings`.
        hmConfigFile = settings:
          (hmConfig { programs.cal-tui.settings = settings; }).xdg.configFile."cal-tui/config.toml".source;
      in
      {
        packages.default = cal-tui;

        checks = {
          # Builds and runs the whole test suite.
          package = cal-tui;

          clippy = cal-tui.overrideAttrs (old: {
            pname = "cal-tui-clippy";
            nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
            buildPhase = "cargo clippy --workspace --all-targets --offline -- -D warnings";
            doCheck = false;
            installPhase = "touch $out";
          });

          fmt = pkgs.runCommand "cal-tui-fmt" { nativeBuildInputs = [ pkgs.rustfmt ]; } ''
            find ${./crates} -name '*.rs' -print0 | xargs -0 rustfmt --check --edition 2021
            touch $out
          '';

          # A good config evaluates, passes check-config and contains what we set.
          hm-module =
            let
              file = hmConfigFile {
                timezone = "Europe/Stockholm";
                default_calendar = "personal";
                sync_command = "vdirsyncer sync";
                calendars.uni = {
                  color = "cyan";
                  read_only = true;
                  hide_matching = [ "^Tutorial group B" ];
                };
              };
            in
            pkgs.runCommand "cal-tui-hm-module" { } ''
              cat ${file}
              grep -q '^timezone = "Europe/Stockholm"' ${file}
              grep -q '^read_only = true' ${file}
              touch $out
            '';

          # The reminder service becomes a systemd user unit.
          hm-notify-service =
            let
              unit = (hmConfig {
                services.cal-tui-notify = {
                  enable = true;
                  catchUp = "30m";
                };
              }).xdg.configFile."systemd/user/cal-tui-notify.service".source;
            in
            pkgs.runCommand "cal-tui-hm-notify" { } ''
              cat ${unit}
              grep -q "^ExecStart=.*/bin/cal-tui notify --catch-up 30m\$" ${unit}
              grep -q "^WantedBy=graphical-session.target" ${unit}
              grep -q "^Restart=on-failure" ${unit}
              touch $out
            '';

          # A bad config must fail the build, naming the problem.
          hm-module-rejects-bad-config =
            let
              failed = pkgs.testers.testBuildFailure (hmConfigFile {
                timezone = "Mars/Olympus";
                calendars.uni.color = "chartreuse-ish";
              });
            in
            pkgs.runCommand "cal-tui-hm-module-bad" { } ''
              grep -q "unknown time zone 'Mars/Olympus'" ${failed}/testBuildFailure.log
              grep -q "unknown colour 'chartreuse-ish'" ${failed}/testBuildFailure.log
              touch $out
            '';
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            rust-analyzer
            vdirsyncer
            radicale # for running a local test server
          ];
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };
      });
}
