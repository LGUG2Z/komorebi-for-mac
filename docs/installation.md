# Getting started

`komorebi` is a tiling window manager for macOS that is comprised of two
main binaries, `komorebi`, which contains the window manager itself,
and `komorebic`, which is the main way to send commands to the tiling
window manager.

It is important to note that neither `komorebi` nor `komorebic` handle
key bindings, because `komorebi` is a tiling window manager and not a hotkey
daemon.

This getting started guide suggests the installation of
[`skhd`](https://github.com/koekeishiya/skhd) to allow you to bind `komorebic`
commands to hotkeys to allow you to communicate with the tiling window manager
using keyboard shortcuts.

## Installation

At this time users must compile `komorebi` from source.

## Building from source

Make sure you have installed [`rustup`](https://rustup.rs), and a stable `rust`
compiler toolchain.

Clone the git repository, enter the directory, and build the following binaries:

```powershell
cargo +stable install --path komorebi --locked --target-dir ~/.cargo/bin
cargo +stable install --path komorebic --locked --target-dir ~/.cargo/bin
```

If the binaries have been built and added to your `$PATH` correctly, you should
see some output when running `komorebi --help` and `komorebic --help`

Once komorebi is installed, proceed to get
the [example configurations](example-configurations.md).

## Uninstallation

Before uninstalling, first run `komorebic stop` to make sure that
the `komorebi` processes have been stopped.

Finally, you can run the following commands to clean up files created by the
`quickstart` command and any other runtime files:

```powershell
rm -rf ~/.cargo/bin/komorebi
rm -rf ~/.cargo/bin/komorebic
rm -rf ~/.config/komorebi
rm -rf "~/Library/Application Support/komorebi"
```
