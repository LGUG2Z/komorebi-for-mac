{
  description = "Build komorebi workspace";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    flake-utils,
    rust-overlay,
    ...
  }: let
    komorebiBuild = system: let
      overlays = [(import rust-overlay)];
      pkgs = import nixpkgs {
        inherit system overlays;
      };

      inherit (pkgs) lib;

      toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
      version = "0.1.0";

      src = lib.cleanSourceWith {
        src = ./.;
        filter = path: type:
          (craneLib.filterCargoSources path type)
          || (lib.hasInfix "/docs/" path)
          || (builtins.match ".*/docs/.*" path != null);
      };

      commonArgs = {
        inherit src version;
        strictDeps = true;

        COMMIT_HASH = self.rev or (lib.removeSuffix "-dirty" self.dirtyRev);

        buildInputs = [
          pkgs.gcc
          pkgs.libiconv
        ];
      };

      cargoArtifacts = craneLib.buildDepsOnly commonArgs;

      individualCrateArgs =
        commonArgs
        // {
          inherit cargoArtifacts;
          doCheck = false;
          doDoc = false;
        };

      packages = let
        fullBuild = craneLib.buildPackage (
          individualCrateArgs
          // {
            pname = "komorebi-workspace";
          }
        );

        extractBinary = binaryName:
          pkgs.runCommand "komorebi-${binaryName}"
          {
            meta = fullBuild.meta // {};
          }
          ''
            mkdir -p $out/bin
            cp ${fullBuild}/bin/${binaryName} $out/bin/
          '';
      in {
        komorebi-full = fullBuild;
        komorebi = extractBinary "komorebi";
        komorebic = extractBinary "komorebic";
        komorebi-bar = extractBinary "komorebi-bar";
      };
    in
      packages
      // {
        inherit
          pkgs
          craneLib
          commonArgs
          cargoArtifacts
          individualCrateArgs
          src
          ;
      };
  in
    flake-utils.lib.eachSystem ["aarch64-darwin"] (
      system: let
        buildResult = komorebiBuild system;
        inherit
          (buildResult)
          individualCrateArgs
          komorebi
          komorebic
          komorebi-bar
          komorebi-full
          pkgs
          craneLib
          src
          ;
      in {
        checks = {
          komorebi-workspace-clippy = craneLib.cargoClippy individualCrateArgs;

          komorebi-workspace-fmt = craneLib.cargoFmt {
            inherit src;
          };

          komorebi-workspace-toml-fmt = craneLib.taploFmt {
            src = pkgs.lib.sources.sourceFilesBySuffices src [".toml"];
          };

          komorebi-workspace-deny = craneLib.cargoDeny {
            inherit src;
          };

          komorebi-workspace-nextest = craneLib.cargoNextest individualCrateArgs;
        };

        packages = {
          inherit
            komorebi
            komorebic
            komorebi-bar
            komorebi-full
            ;
          default = komorebi-full;
        };

        apps = {
          komorebi = flake-utils.lib.mkApp {
            drv = komorebi;
          };
          komorebic = flake-utils.lib.mkApp {
            drv = komorebic;
          };
          komorebi-bar = flake-utils.lib.mkApp {
            drv = komorebi-bar;
          };
          default = flake-utils.lib.mkApp {
            drv = komorebi-full;
          };
        };

        devShells.default = import ./shell.nix {
          inherit pkgs;
        };
      }
    )
    // {
      overlays.default = final: _: let
        buildResult = komorebiBuild final.system;
      in {
        inherit
          (buildResult)
          komorebi
          komorebic
          komorebi-bar
          komorebi-full
          ;
      };

      overlays.komorebi = final: _: let
        buildResult = komorebiBuild final.system;
      in {
        inherit
          (buildResult)
          komorebi
          komorebic
          komorebi-bar
          komorebi-full
          ;
      };
    };
}
