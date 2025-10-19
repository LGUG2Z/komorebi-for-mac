{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
  };

  outputs = inputs:
    with inputs; let
      komorebiBuild = system: let
        overlays = [
          (import rust-overlay)
        ];

        pkgs = (import nixpkgs) {
          inherit system overlays;
        };

        inherit (pkgs) lib;

        toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

        src = lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            (craneLib.filterCargoSources path type)
            || (lib.hasInfix "/docs/" path)
            || (builtins.match ".*/docs/.*" path != null);
        };

        version = "0.1.0";
        pname = "komorebi";

        commonArgs = {
          inherit src version pname;
          nativeBuildInputs = with pkgs; [];
          doCheck = false;
          buildInputs = with pkgs; [
            gcc
            libiconv
          ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
      in {
        komorebi = craneLib.buildPackage (
          commonArgs
          // {
            cargoExtraArgs = "-p komorebi";
          }
        );

        komorebic = craneLib.buildPackage (
          commonArgs
          // {
            cargoExtraArgs = "-p komorebic";
          }
        );

        komorebi-bar = craneLib.buildPackage (
          commonArgs
          // {
            cargoExtraArgs = "-p komorebi-bar";
          }
        );

        komorebi-full = craneLib.buildPackage (
          commonArgs
          // {
            cargoExtraArgs = "-p komorebi -p komorebic -p komorebi-bar";
          }
        );
      };
    in
      (flake-utils.lib.eachDefaultSystem (
        system: let
          packages = komorebiBuild system;
        in {
          devShells = flake-utils.lib.flattenTree {
            default = import ./shell.nix {
              pkgs = (import nixpkgs) {
                inherit system;
                overlays = [(import rust-overlay)];
              };
            };
          };

          packages = flake-utils.lib.flattenTree {
            inherit (packages) komorebi komorebic komorebi-bar komorebi-full;
            default = packages.komorebi-full;
          };
        }
      ))
      // {
        overlays.default = final: prev: let
          packages = komorebiBuild final.system;
        in {
          inherit (packages) komorebi komorebic komorebi-bar komorebi-full;
        };

        overlays.komorebi = final: prev: let
          packages = komorebiBuild final.system;
        in {
          inherit (packages) komorebi komorebic komorebi-bar komorebi-full;
        };
      };
}
