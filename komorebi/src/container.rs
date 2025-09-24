use crate::accessibility::error::AccessibilityError;
use crate::lockable_sequence::Lockable;
use crate::ring::Ring;
use crate::window::Window;
use color_eyre::eyre;
use nanoid::nanoid;
use serde::Deserialize;
use serde::Serialize;

impl_ring_elements!(Container, Window);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]

pub struct Container {
    pub id: String,
    pub locked: bool,
    pub windows: Ring<Window>,
}

impl Default for Container {
    fn default() -> Self {
        Self {
            id: nanoid!(),
            locked: false,
            windows: Default::default(),
        }
    }
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

impl Container {
    pub fn idx_from_exe(&self, exe: &str) -> Option<usize> {
        for (idx, window) in self.windows().iter().enumerate() {
            if let Some(window_exe) = window.exe()
                && exe == window_exe
            {
                return Option::from(idx);
            }
        }

        None
    }

    pub fn contains_window(&self, window_id: u32) -> bool {
        for window in self.windows() {
            if window.id == window_id {
                return true;
            }
        }

        false
    }

    pub fn idx_for_window(&self, window_id: u32) -> Option<usize> {
        for (i, window) in self.windows().iter().enumerate() {
            if window.id == window_id {
                return Option::from(i);
            }
        }

        None
    }

    pub fn remove_focused_window(&mut self) -> Option<Window> {
        let focused_idx = self.focused_window_idx();
        self.remove_window_by_idx(focused_idx)
    }

    pub fn add_window(&mut self, window: &Window) -> eyre::Result<()> {
        self.windows_mut().push_back(window.clone());
        self.focus_window(self.windows().len().saturating_sub(1));
        let focused_window_idx = self.focused_window_idx();

        for (i, window) in self.windows_mut().iter_mut().enumerate() {
            if i != focused_window_idx {
                window.hide()?;
            }
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn focus_window(&mut self, idx: usize) {
        tracing::info!("focusing window");
        self.windows.focus(idx);
    }

    /// Hides the unfocused windows of the container and restores the focused one. This function
    /// is used to make sure we update the window that should be shown on a stack. If the container
    /// isn't a stack this function won't change anything.
    pub fn load_focused_window(&mut self) -> Result<(), AccessibilityError> {
        let focused_idx = self.focused_window_idx();

        for (i, window) in self.windows_mut().iter_mut().enumerate() {
            if i == focused_idx {
                window.restore()?;
            } else {
                window.hide()?;
            }
        }

        Ok(())
    }

    pub fn remove_window_by_idx(&mut self, idx: usize) -> Option<Window> {
        let window = self.windows_mut().remove(idx);
        self.focus_window(idx.saturating_sub(1));
        window
    }

    pub fn hide(&mut self, omit: Option<u32>) -> eyre::Result<()> {
        for window in self.windows_mut().iter_mut().rev() {
            let mut should_hide = omit.is_none();

            if !should_hide
                && let Some(omit) = omit
                && omit != window.id
            {
                should_hide = true
            }

            if should_hide {
                window.hide()?;
            }
        }

        Ok(())
    }

    pub fn restore(&mut self) -> eyre::Result<()> {
        if let Some(window) = self.focused_window_mut() {
            window.restore()?;
        }

        Ok(())
    }
}
