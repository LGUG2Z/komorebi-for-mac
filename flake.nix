{
  description = "Build komorebi workspace";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = {
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

      cargoArtifacts = craneLib.buildDepsOnly commonArgs;

      commonArgs = {
        inherit src version cargoArtifacts;
        strictDeps = true;

        buildInputs = [
          pkgs.gcc
          pkgs.libiconv
        ];
      };

      individualCrateArgs =
        commonArgs
        // {
          doCheck = false;
        };

      packages = {
        komorebi = craneLib.buildPackage (
          individualCrateArgs
          // {
            inherit version;
            pname = "komorebi";
            cargoExtraArgs = "-p komorebi";
          }
        );

        komorebic = craneLib.buildPackage (
          individualCrateArgs
          // {
            inherit version;
            pname = "komorebic";
            cargoExtraArgs = "-p komorebic";
          }
        );

        komorebi-bar = craneLib.buildPackage (
          individualCrateArgs
          // {
            inherit version;
            pname = "komorebi-bar";
            cargoExtraArgs = "-p komorebi-bar";
          }
        );

        komorebi-full = craneLib.buildPackage (
          individualCrateArgs
          // {
            inherit version;
            pname = "komorebi-full";
            cargoExtraArgs = "-p komorebi -p komorebic -p komorebi-bar";
          }
        );
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
          komorebi
          komorebic
          komorebi-bar
          komorebi-full
          pkgs
          craneLib
          commonArgs
          src
          ;
      in {
        checks = {
          komorebi-workspace-clippy = craneLib.cargoClippy (
            commonArgs
            // {
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            }
          );

          komorebi-workspace-fmt = craneLib.cargoFmt {
            inherit src;
          };

          komorebi-workspace-toml-fmt = craneLib.taploFmt {
            src = pkgs.lib.sources.sourceFilesBySuffices src [".toml"];
          };

          komorebi-workspace-deny = craneLib.cargoDeny {
            inherit src;
          };

          komorebi-workspace-nextest = craneLib.cargoNextest commonArgs;
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
