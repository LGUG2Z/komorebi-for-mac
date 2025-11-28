{
  pkgs ? import (fetchTarball "https://nixos.org/channels/nixos-unstable/nixexprs.tar.xz") { },
}:
with pkgs;
mkShell {
  name = "komorebi";

  RUST_BACKTRACE = "full";

  buildInputs = [
    alejandra
    cargo-deny
    cargo-nextest
    cargo-udeps
    gcc
    jq
    just
    libiconv
    prettier
    taplo

    python311Packages.mkdocs-material
    python311Packages.mkdocs-macros
    python311Packages.setuptools
    python311Packages.json-schema-for-humans
  ];
}
