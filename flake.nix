{
  description = "Build komorebi workspace";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    crane.url = "github:ipetkov/crane";
    rust-overlay.url = "github:oxalica/rust-overlay";
    treefmt-nix.url = "github:numtide/treefmt-nix";
  };

  outputs =
    inputs@{
      self,
      nixpkgs,
      flake-parts,
      crane,
      rust-overlay,
      ...
    }:
    let
      mkKomorebiPackages =
        { pkgs }:
        let
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
          version = "0.1.0";

          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              (craneLib.filterCargoSources path type)
              || (pkgs.lib.hasInfix "/docs/" path)
              || (builtins.match ".*/docs/.*" path != null);
          };

          commonArgs = {
            inherit src version;
            strictDeps = true;
            COMMIT_HASH = self.rev or (pkgs.lib.removeSuffix "-dirty" self.dirtyRev);
            nativeBuildInputs = [
              pkgs.darwin.cctools
            ];
            postFixup = ''
              for bin in $out/bin/*; do
                if [ -f "$bin" ] && [ -x "$bin" ]; then
                  # Get all linked libraries and fix libiconv references
                  otool -L "$bin" | grep -o '/nix/store/[^/]*/lib/libiconv[^ ]*' | while read -r lib; do
                    install_name_tool -change "$lib" /usr/lib/libiconv.dylib "$bin"
                  done
                fi
              done
            '';
          };

          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          individualCrateArgs = commonArgs // {
            inherit cargoArtifacts;
            doCheck = false;
            doDoc = false;
          };

          fullBuild = craneLib.buildPackage (
            individualCrateArgs
            // {
              pname = "komorebi-workspace";
            }
          );

          extractBinary =
            binaryName:
            pkgs.runCommand "komorebi-${binaryName}"
              {
                meta = fullBuild.meta // { };
              }
              ''
                mkdir -p $out/bin
                cp ${fullBuild}/bin/${binaryName} $out/bin/
              '';
        in
        {
          inherit
            craneLib
            src
            individualCrateArgs
            fullBuild
            ;
          komorebi = extractBinary "komorebi";
          komorebic = extractBinary "komorebic";
          komorebi-bar = extractBinary "komorebi-bar";
        };

      mkPkgs =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
    in
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
      ];

      imports = [
        inputs.treefmt-nix.flakeModule
      ];

      perSystem =
        { system, ... }:
        let
          pkgs = mkPkgs system;
          build = mkKomorebiPackages { inherit pkgs; };
        in
        {
          treefmt = {
            projectRootFile = "flake.nix";
            programs = {
              deadnix.enable = true;
              just.enable = true;
              nixfmt.enable = true;
              taplo.enable = true;
              rustfmt = {
                enable = true;
                package = pkgs.rust-bin.nightly.latest.rustfmt;
              };
            };
          };

          checks = {
            komorebi-workspace-clippy = build.craneLib.cargoClippy build.individualCrateArgs;

            komorebi-workspace-fmt = build.craneLib.cargoFmt {
              inherit (build) src;
            };

            komorebi-workspace-toml-fmt = build.craneLib.taploFmt {
              src = pkgs.lib.sources.sourceFilesBySuffices build.src [ ".toml" ];
            };

            komorebi-workspace-deny = build.craneLib.cargoDeny {
              inherit (build) src;
            };

            komorebi-workspace-nextest = build.craneLib.cargoNextest build.individualCrateArgs;
          };

          packages = {
            inherit (build) komorebi komorebic komorebi-bar;
            komorebi-full = build.fullBuild;
            default = build.fullBuild;
          };

          apps = {
            komorebi = {
              type = "app";
              program = "${build.komorebi}/bin/komorebi";
            };
            komorebic = {
              type = "app";
              program = "${build.komorebic}/bin/komorebic";
            };
            komorebi-bar = {
              type = "app";
              program = "${build.komorebi-bar}/bin/komorebi-bar";
            };
            default = {
              type = "app";
              program = "${build.fullBuild}/bin/komorebi";
            };
          };

          devShells.default = import ./shell.nix {
            inherit pkgs;
          };
        };

      flake = {
        overlays.default =
          final: _:
          let
            pkgs = mkPkgs final.system;
            build = mkKomorebiPackages {
              inherit pkgs;
            };
          in
          {
            inherit (build) komorebi komorebic komorebi-bar;
            komorebi-full = build.fullBuild;
          };

        overlays.komorebi =
          final: _:
          let
            pkgs = mkPkgs final.system;
            build = mkKomorebiPackages {
              inherit pkgs;
            };
          in
          {
            inherit (build) komorebi komorebic komorebi-bar;
            komorebi-full = build.fullBuild;
          };
      };
    };
}
