#![warn(clippy::all)]

use color_eyre::eyre;
use color_eyre::eyre::eyre;
use komorebi::window_manager::WindowManager;
use objc2::rc::autoreleasepool;
use objc2_application_services::AXIsProcessTrusted;
use objc2_core_foundation::CFRunLoop;
use objc2_core_foundation::kCFRunLoopDefaultMode;
use objc2_core_graphics::CGDisplayBounds;
use objc2_core_graphics::CGMainDisplayID;
use objc2_core_graphics::CGPreflightScreenCaptureAccess;
use objc2_core_graphics::CGRequestScreenCaptureAccess;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use tracing_subscriber::EnvFilter;

fn check_permissions() -> eyre::Result<()> {
    unsafe {
        // check for screen capture access - this is needed to read window titles
        if !CGPreflightScreenCaptureAccess() {
            // if providing the dialog box failed, exit
            if !CGRequestScreenCaptureAccess() {
                eyre::bail!("failed to request screen capability");
            }
        }

        if !AXIsProcessTrusted() {
            eyre::bail!("komorebi needs to be added as a trusted accessibility process");
        }

        Ok(())
    }
}

pub fn setup() -> eyre::Result<()> {
    if std::env::var("RUST_LIB_BACKTRACE").is_err() {
        unsafe {
            std::env::set_var("RUST_LIB_BACKTRACE", "1");
        }
    }

    color_eyre::install()?;

    if std::env::var("RUST_LOG").is_err() {
        unsafe {
            std::env::set_var("RUST_LOG", "info");
        }
    }
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt::Subscriber::builder()
            .with_env_filter(EnvFilter::from_default_env())
            .finish(),
    )?;

    // https://github.com/tokio-rs/tracing/blob/master/examples/examples/panic_hook.rs
    // Set a panic hook that records the panic as a `tracing` event at the
    // `ERROR` verbosity level.
    //
    // If we are currently in a span when the panic occurred, the logged event
    // will include the current span, allowing the context in which the panic
    // occurred to be recorded.
    std::panic::set_hook(Box::new(|panic| {
        // If the panic has a source location, record it as structured fields.
        panic.location().map_or_else(
            || {
                tracing::error!(message = %panic);
            },
            |location| {
                // On nightly Rust, where the `PanicInfo` type also exposes a
                // `message()` method returning just the message, we could record
                // just the message instead of the entire `fmt::Display`
                // implementation, avoiding the duplciated location
                tracing::error!(
                    message = %panic,
                    panic.file = location.file(),
                    panic.line = location.line(),
                    panic.column = location.column(),
                );
            },
        );
    }));

    Ok(())
}

fn main() -> eyre::Result<()> {
    setup()?;
    check_permissions()?;

    let display_size = unsafe { CGDisplayBounds(CGMainDisplayID()) };
    tracing::info!("display size for main display is: {:?}", display_size);

    let run_loop = CFRunLoop::current().ok_or(eyre!("couldn't get CFRunLoop::current"))?;
    let mut wm = WindowManager::new(&run_loop);
    wm.init()?;

    wm.monitors
        .focused_mut()
        .unwrap()
        .focused_workspace_mut()
        .unwrap()
        .update()?;

    let quit_ctrlc = Arc::new(AtomicBool::new(false));
    let quit_thread = quit_ctrlc.clone();

    std::thread::spawn(move || {
        let (ctrlc_sender, ctrlc_receiver) = mpsc::channel();
        ctrlc::set_handler(move || {
            ctrlc_sender
                .send(())
                .expect("could not send signal on ctrl-c channel");
        })
        .expect("could not set ctrl-c handler");

        ctrlc_receiver
            .recv()
            .expect("could not receive signal on ctrl-c channel");

        tracing::info!("ctrl-c signal received");
        quit_ctrlc.store(true, Ordering::Relaxed);
    });

    tracing::info!("starting CFRunLoop to receive observer notifications");

    loop {
        if quit_thread.load(Ordering::Relaxed) {
            tracing::info!("stopping CFRunLoop");
            break;
        }

        // this gets our observer notification callbacks firing
        autoreleasepool(|_| unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, 2.0, false) });
    }

    Ok(())
}
