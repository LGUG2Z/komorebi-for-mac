export RUST_BACKTRACE := "full"

fmt:
    test -z "$(rg 'eyre!' --type rust)" || (echo "eyre! macro not allowed"; false)
    test -z "$(rg 'dbg!' --type rust)" || (echo "dbg! macro not allowed"; false)
    test -z "$(rg 'println!' --type rust ./komorebi)" || (echo "println! macro not allowed"; false)
    cargo +stable clippy
    prettier --write README.md
    nix fmt

fix:
    cargo clippy --fix --allow-dirty

clean:
    cargo clean

install-with-jsonschema target:
    cargo +stable install --path {{ target }} --locked --target-dir ~/.cargo/bin

install target:
    cargo +stable install --path {{ target }} --locked --target-dir ~/.cargo/bin --no-default-features

build target:
    cargo +stable build --package {{ target }} --locked --release

run target:
    cargo +stable run --bin {{ target }} --locked

error target $RUST_LOG="komorebi=error":
    just run {{ target }}

warn target $RUST_LOG="komorebi=warn":
    just run {{ target }}

info target $RUST_LOG="komorebi=info":
    just run {{ target }}

debug target $RUST_LOG="komorebi=debug":
    just run {{ target }}

trace target $RUST_LOG="komorebi=trace":
    just run {{ target }}

deadlock $RUST_LOG="trace":
    cargo +stable run --bin komorebi --locked --no-default-features --features deadlock_detection

docgen:
    cargo run --package komorebic -- docgen
    find docs/cli -type f -exec sed -i.bak 's/Usage: /Usage: komorebic /g' {} \; && find docs/cli -name "*.bak" -delete

jsonschema:
    cargo run --package komorebic -- static-config-schema > schema.json
    cargo run --package komorebic -- application-specific-configuration-schema > schema.asc.json
    cargo run --package komorebi-bar -- --schema > schema.bar.json

version := `cargo metadata --format-version 1 --no-deps | jq -r '.packages[] | select(.name == "komorebi") | .version'`

schemagen:
    mkdir -p komorebi-schema
    schemars-docgen schema.json -o komorebi-schema/komorebi.html
    schemars-docgen schema.bar.json -o komorebi-schema/bar.html
    cp schema.json komorebi-schema/komorebi.{{ version }}.schema.json
    cp schema.bar.json komorebi-schema/komorebi.bar.{{ version }}.schema.json

schemapub:
    wrangler pages deploy --project-name komorebi-for-mac --branch main ./komorebi-schema

depgen:
    cargo deny check
    cargo deny list --format json | jq 'del(.unlicensed)' > dependencies.json

push:
    git push origin master
    git push komocorp master
