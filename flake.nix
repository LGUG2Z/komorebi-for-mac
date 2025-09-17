{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
  };

  outputs = inputs:
    with inputs;
      flake-utils.lib.eachDefaultSystem (
        system: let
          overlays = [
            (import rust-overlay)
          ];

          pkgs = (import nixpkgs) {
            inherit system overlays;
          };

          inherit (pkgs) lib;

          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
          src = craneLib.cleanCargoSource ./.;
          version = "0.1.0";

          commonArgs = {
            inherit src;
            nativeBuildInputs = with pkgs; [];
            doCheck = false;
            buildInputs = with pkgs; [
              gcc
              libiconv
            ];
          };

          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          komorebi = craneLib.buildPackage (
            commonArgs
            // {
              inherit version;
              pname = "komorebi";
              cargoExtraArgs = "-p komorebi";
              src = ./.;
            }
          );

          komorebic = craneLib.buildPackage (
            commonArgs
            // {
              inherit version;
              pname = "komorebic";
              cargoExtraArgs = "-p komorebic";
              src = ./.;
            }
          );
        in {
          devShells = flake-utils.lib.flattenTree {
            default = import ./shell.nix {inherit pkgs;};
          };

          packages = flake-utils.lib.flattenTree rec {
            inherit komorebi komorebic;
            default = komorebi;
          };
        }
      );
}