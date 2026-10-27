//! The viewer: a window for playing and scrubbing a recording with everything that happened in it
//! laid out on a timeline.

mod app;
mod composer;
mod inspector;
pub mod keymap;
mod lanes;
pub mod loaded;
pub mod prefs;
pub mod theme;
pub mod timeline;
mod video;

use color_eyre::eyre;
use eframe::egui;
use std::path::PathBuf;

pub use app::Open;
pub use app::Screen;
pub use app::ViewerApp;

/// Open the viewer window, with `session` open if given.
pub fn run(session: Option<PathBuf>) -> eyre::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // no title: komorebi leaves untitled windows alone, so opening the viewer does not
            // move the layout of the machine whose behavior is being investigated
            .with_title("")
            .with_title_shown(false)
            .with_titlebar_shown(false)
            .with_fullsize_content_view(true)
            .with_inner_size([1500.0, 920.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "komorebi-recorder",
        options,
        Box::new(move |creation| {
            let mut visuals = egui::Visuals::dark();
            visuals.panel_fill = theme::BACKGROUND;
            visuals.window_fill = theme::BACKGROUND;
            creation.egui_ctx.set_visuals(visuals);

            let mut app = ViewerApp::new(prefs::directory());
            if let Some(dir) = &session {
                app.open(dir);
            }
            Ok(Box::new(Shell(app)))
        }),
    )
    .map_err(|error| eyre::Report::msg(format!("the viewer window failed: {error}")))
}

/// Adapts [`ViewerApp`] to eframe.
struct Shell(ViewerApp);

impl eframe::App for Shell {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.0.ui(context);
    }
}
