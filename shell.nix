{pkgs ? import (fetchTarball "https://nixos.org/channels/nixos-unstable/nixexprs.tar.xz") {}}:
with pkgs;
  mkShell {
    name = "komorebi";

    buildInputs = [
      just
      gcc
      libiconv
      cargo-deny
      jq
      prettier
      cargo-udeps
    ];
  }
