use crate::CoreFoundationRunLoop;
use crate::DATA_DIR;
use crate::LibraryError;
use crate::application::Application;
use crate::core::default_layout::DefaultLayout;
use crate::core::layout::Layout;
use crate::core::operation_direction::OperationDirection;
use crate::core::rect::Rect;
use crate::lockable_sequence::Lockable;
use crate::macos_api::MacosApi;
use crate::monitor::Monitor;
use crate::ring::Ring;
use crate::window::Window;
use crate::workspace::Workspace;
use color_eyre::eyre;
use color_eyre::eyre::eyre;
use objc2_core_foundation::CFRetained;
use objc2_core_foundation::CFRunLoop;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

#[derive(Debug)]
pub struct WindowManager {
    pub monitors: Ring<Monitor>,
    pub applications: HashMap<i32, Application>,
    pub run_loop: CoreFoundationRunLoop,
    pub command_listener: UnixListener,
    pub is_paused: bool,
    pub mouse_follows_focus: bool,
    pub work_area_offset: Option<Rect>,
}

impl_ring_elements!(WindowManager, Monitor);

impl WindowManager {
    pub fn new(
        run_loop: &CFRetained<CFRunLoop>,
        custom_socket_path: Option<PathBuf>,
    ) -> eyre::Result<Self> {
        let socket = custom_socket_path.unwrap_or_else(|| DATA_DIR.join("komorebi.sock"));

        match std::fs::remove_file(&socket) {
            Ok(()) => {}
            Err(error) => match error.kind() {
                ErrorKind::NotFound => {}
                _ => {
                    return Err(error.into());
                }
            },
        };

        let listener = UnixListener::bind(&socket)?;

        Ok(Self {
            monitors: Ring::default(),
            applications: Default::default(),
            run_loop: CoreFoundationRunLoop(run_loop.clone()),
            command_listener: listener,
            is_paused: false,
            mouse_follows_focus: true,
            work_area_offset: None,
        })
    }

    #[tracing::instrument(skip(self))]
    pub fn init(&mut self) -> Result<(), LibraryError> {
        tracing::info!("initializing");
        MacosApi::load_monitor_information(self)?;
        MacosApi::load_workspace_information(self)
    }
}

impl WindowManager {
    #[tracing::instrument(skip(self))]
    pub fn focus_container_in_direction(
        &mut self,
        direction: OperationDirection,
    ) -> eyre::Result<()> {
        let workspace = self.focused_workspace()?;

        tracing::info!("focusing container");

        match workspace.new_idx_for_direction(direction) {
            None => {}
            Some(idx) => {
                let workspace = self.focused_workspace_mut()?;
                workspace.focus_container(idx);
            }
        }

        if let Ok(focused_window) = self.focused_window() {
            focused_window.focus(self.mouse_follows_focus)?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn move_container_in_direction(
        &mut self,
        direction: OperationDirection,
    ) -> eyre::Result<()> {
        let workspace = self.focused_workspace()?;

        tracing::info!("moving container");

        let origin_container_idx = workspace.focused_container_idx();
        let target_container_idx = workspace.new_idx_for_direction(direction);

        match target_container_idx {
            None => {}
            Some(new_idx) => {
                let workspace = self.focused_workspace_mut()?;
                workspace.swap_containers(origin_container_idx, new_idx);
                workspace.focus_container(new_idx);
            }
        }

        self.update_focused_workspace(self.mouse_follows_focus, true)?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn update_focused_workspace(
        &mut self,
        follow_focus: bool,
        trigger_focus: bool,
    ) -> eyre::Result<()> {
        tracing::info!("updating");

        let offset = self.work_area_offset;
        let mouse_follows_focus = self.mouse_follows_focus;

        self.focused_monitor_mut()
            .ok_or_else(|| eyre!("there is no monitor"))?
            .update_focused_workspace(offset)?;

        if follow_focus
            && let Ok(window) = self.focused_window_mut()
            && trigger_focus
        {
            window.focus(mouse_follows_focus)?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn change_workspace_layout_default(&mut self, layout: DefaultLayout) -> eyre::Result<()> {
        tracing::info!("changing layout");

        let monitor_count = self.monitors().len();
        let workspace = self.focused_workspace_mut()?;

        if monitor_count > 1 && matches!(layout, DefaultLayout::Scrolling) {
            tracing::warn!(
                "scrolling layout is only supported for a single monitor; not changing layout"
            );
            return Ok(());
        }

        workspace.layout = Layout::Default(layout);
        self.update_focused_workspace(self.mouse_follows_focus, false)
    }

    pub fn focused_workspace_idx(&self) -> eyre::Result<usize> {
        Ok(self
            .focused_monitor()
            .ok_or_else(|| eyre!("there is no monitor"))?
            .focused_workspace_idx())
    }

    pub fn focused_workspace(&self) -> eyre::Result<&Workspace> {
        self.focused_monitor()
            .ok_or_else(|| eyre!("there is no monitor"))?
            .focused_workspace()
            .ok_or_else(|| eyre!("there is no workspace"))
    }

    pub fn focused_workspace_mut(&mut self) -> eyre::Result<&mut Workspace> {
        self.focused_monitor_mut()
            .ok_or_else(|| eyre!("there is no monitor"))?
            .focused_workspace_mut()
            .ok_or_else(|| eyre!("there is no workspace"))
    }

    pub fn focused_workspace_idx_for_monitor_idx(&self, idx: usize) -> eyre::Result<usize> {
        Ok(self
            .monitors()
            .get(idx)
            .ok_or_else(|| eyre!("there is no monitor at this index"))?
            .focused_workspace_idx())
    }

    pub fn focused_workspace_for_monitor_idx(&self, idx: usize) -> eyre::Result<&Workspace> {
        self.monitors()
            .get(idx)
            .ok_or_else(|| eyre!("there is no monitor at this index"))?
            .focused_workspace()
            .ok_or_else(|| eyre!("there is no workspace"))
    }

    pub fn focused_workspace_for_monitor_idx_mut(
        &mut self,
        idx: usize,
    ) -> eyre::Result<&mut Workspace> {
        self.monitors_mut()
            .get_mut(idx)
            .ok_or_else(|| eyre!("there is no monitor at this index"))?
            .focused_workspace_mut()
            .ok_or_else(|| eyre!("there is no workspace"))
    }

    pub fn focused_container(&self) -> eyre::Result<&Container> {
        self.focused_workspace()?
            .focused_container()
            .ok_or_else(|| eyre!("there is no container"))
    }

    pub fn focused_container_idx(&self) -> eyre::Result<usize> {
        Ok(self.focused_workspace()?.focused_container_idx())
    }

    pub fn focused_container_mut(&mut self) -> eyre::Result<&mut Container> {
        self.focused_workspace_mut()?
            .focused_container_mut()
            .ok_or_else(|| eyre!("there is no container"))
    }

    pub fn focused_window(&self) -> eyre::Result<&Window> {
        self.focused_container()?
            .focused_window()
            .ok_or_else(|| eyre!("there is no window"))
    }

    fn focused_window_mut(&mut self) -> eyre::Result<&mut Window> {
        self.focused_container_mut()?
            .focused_window_mut()
            .ok_or_else(|| eyre!("there is no window"))
    }
}

impl_ring_elements!(Container, Window);
#[derive(Debug, Default)]
pub struct Container {
    pub windows: Ring<Window>,
    pub locked: bool,
}

impl Lockable for Container {
    fn locked(&self) -> bool {
        self.locked
    }

    fn set_locked(&mut self, locked: bool) -> &mut Self {
        self.locked = locked;
        self
    }
}
