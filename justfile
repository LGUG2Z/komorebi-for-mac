export RUST_BACKTRACE := "full"

fmt:
    test -z "$(rg 'eyre!' --type rust)" || (echo "eyre! macro not allowed"; false)
    test -z "$(rg 'dbg!' --type rust)" || (echo "dbg! macro not allowed"; false)
    test -z "$(rg 'println!' --type rust ./komorebi)" || (echo "println! macro not allowed"; false)
    cargo +nightly fmt
    cargo +stable clippy
    prettier --write README.md

fix:
    cargo clippy --fix --allow-dirty

clean:
    cargo clean

install target:
    cargo +stable install --path {{ target }} --locked --target-dir ~/.cargo/bin

build target:
    cargo +stable build --package {{ target }} --locked --release

run target:
    cargo +stable run --bin {{ target }} --locked

warn target $RUST_LOG="warn":
    just run {{ target }}

info target $RUST_LOG="info":
    just run {{ target }}

debug target $RUST_LOG="debug":
    just run {{ target }}

trace target $RUST_LOG="trace":
    just run {{ target }}

depgen:
    cargo deny check
    cargo deny list --format json | jq 'del(.unlicensed)' > dependencies.json

push:
    git push origin master
    git push komocorp master