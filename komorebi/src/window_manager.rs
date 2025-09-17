use crate::LibraryError;
use crate::application::Application;
use crate::macos_api::MacosApi;
use crate::rect::Rect;
use crate::ring::Ring;
use crate::window::Window;
use objc2_core_foundation::CFRetained;
use objc2_core_foundation::CFRunLoop;
use std::collections::HashMap;

#[derive(Debug)]
pub struct WindowManager {
    pub monitors: Ring<Monitor>,
    pub applications: HashMap<i32, Application>,
    pub run_loop: CFRetained<CFRunLoop>,
}

impl WindowManager {
    pub fn new(run_loop: &CFRetained<CFRunLoop>) -> Self {
        Self {
            monitors: Ring::default(),
            applications: Default::default(),
            run_loop: run_loop.clone(),
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn init(&mut self) -> Result<(), LibraryError> {
        tracing::info!("initializing");
        MacosApi::load_monitor_information(self)?;
        MacosApi::load_workspace_information(self)
    }
}

impl_ring_elements!(Monitor, Workspace);
#[derive(Debug)]
pub struct Monitor {
    pub id: u32,
    pub workspaces: Ring<Workspace>,
    pub size: Rect,
}

impl Monitor {
    pub fn new(id: u32, size: Rect) -> Self {
        let mut workspaces = Ring::default();
        workspaces.elements_mut().push_back(Workspace::default());

        Self {
            id,
            workspaces,
            size,
        }
    }
}

impl_ring_elements!(Workspace, Container);
#[derive(Debug, Default)]
pub struct Workspace {
    pub containers: Ring<Container>,
}

impl_ring_elements!(Container, Window);
#[derive(Debug)]
pub struct Container {
    pub windows: Ring<Window>,
}
