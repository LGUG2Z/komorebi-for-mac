use crate::AccessibilityObserver;
use crate::AccessibilityUiElement;
use crate::FLOATING_APPLICATIONS;
use crate::FLOATING_WINDOW_TOGGLE_ASPECT_RATIO;
use crate::IGNORE_IDENTIFIERS;
use crate::LibraryError;
use crate::MANAGE_IDENTIFIERS;
use crate::PERMAIGNORE_CLASSES;
use crate::REGEX_IDENTIFIERS;
use crate::WINDOW_RESTORE_POSITIONS;
use crate::accessibility::AccessibilityApi;
use crate::accessibility::action_constants::kAXPressAction;
use crate::accessibility::attribute_constants::kAXCloseButtonAttribute;
use crate::accessibility::attribute_constants::kAXFocusedAttribute;
use crate::accessibility::attribute_constants::kAXMainAttribute;
use crate::accessibility::attribute_constants::kAXMinimizedAttribute;
use crate::accessibility::attribute_constants::kAXParentAttribute;
use crate::accessibility::attribute_constants::kAXPositionAttribute;
use crate::accessibility::attribute_constants::kAXRoleAttribute;
use crate::accessibility::attribute_constants::kAXSizeAttribute;
use crate::accessibility::attribute_constants::kAXSubroleAttribute;
use crate::accessibility::attribute_constants::kAXTitleAttribute;
use crate::accessibility::error::AccessibilityCustomError;
use crate::accessibility::error::AccessibilityError;
use crate::accessibility::notification_constants::AccessibilityNotification;
use crate::accessibility::notification_constants::kAXTitleChangedNotification;
use crate::accessibility::notification_constants::kAXWindowDeminiaturizedNotification;
use crate::accessibility::notification_constants::kAXWindowMiniaturizedNotification;
use crate::accessibility::notification_constants::kAXWindowMovedNotification;
use crate::accessibility::notification_constants::kAXWindowResizedNotification;
use crate::animation::ANIMATION_DURATION_GLOBAL;
use crate::animation::ANIMATION_DURATION_PER_ANIMATION;
use crate::animation::ANIMATION_ENABLED_GLOBAL;
use crate::animation::ANIMATION_ENABLED_PER_ANIMATION;

use crate::accessibility::private::with_enhanced_ui_disabled;
use crate::animation::ANIMATION_STYLE_GLOBAL;
use crate::animation::ANIMATION_STYLE_PER_ANIMATION;
use crate::animation::AnimationEngine;
use crate::animation::RenderDispatcher;
use crate::animation::lerp::Lerp;
use crate::animation::prefix::AnimationPrefix;
use crate::animation::prefix::new_animation_key;
use crate::application::Application;
use crate::cf_dictionary_value;
use crate::core::ApplicationIdentifier;
use crate::core::WindowHidingPosition;
use crate::core::animation::AnimationStyle;
use crate::core::config_generation::IdWithIdentifier;
use crate::core::config_generation::MatchingRule;
use crate::core::config_generation::MatchingStrategy;
use crate::core::rect::Rect;
use crate::core_graphics::CoreGraphicsApi;
use crate::hidden_frame_bottom_left;
use crate::hidden_frame_bottom_right;
use crate::macos_api::MacosApi;
use crate::reaper;
use crate::reaper::ReaperNotification;
use crate::window_manager_event::SystemNotification;
use crate::window_manager_event::WindowManagerEvent;
use crate::window_manager_event_listener;
use color_eyre::eyre;
use objc2::__framework_prelude::Retained;
use objc2_app_kit::NSApplicationActivationOptions;
use objc2_app_kit::NSRunningApplication;
use objc2_application_services::AXObserver;
use objc2_application_services::AXUIElement;
use objc2_application_services::AXValueType;
use objc2_core_foundation::CFBoolean;
use objc2_core_foundation::CFDictionary;
use objc2_core_foundation::CFNumber;
use objc2_core_foundation::CFRetained;
use objc2_core_foundation::CFRunLoop;
use objc2_core_foundation::CFString;
use objc2_core_foundation::CGFloat;
use objc2_core_foundation::CGPoint;
use objc2_core_foundation::CGSize;
use objc2_core_graphics::kCGWindowAlpha;
use objc2_core_graphics::kCGWindowBounds;
use objc2_core_graphics::kCGWindowName;
use objc2_core_graphics::kCGWindowOwnerName;
use objc2_core_graphics::kCGWindowOwnerPID;
use objc2_foundation::NSBundle;
use objc2_foundation::NSString;
use regex::Regex;
use serde::Deserialize;
use serde::Serialize;
use serde::Serializer;
use serde::ser::SerializeStruct;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::c_void;
use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Write;
use std::path::Path;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::time::Duration;
use strum::Display;
use strum::EnumString;
use tracing::instrument;

const NOTIFICATIONS: &[&str] = &[
    kAXWindowMiniaturizedNotification,
    kAXWindowDeminiaturizedNotification,
    kAXWindowMovedNotification,
    kAXWindowResizedNotification,
    kAXTitleChangedNotification,
];

/// Render dispatcher for window movement animations
pub struct MovementRenderDispatcher {
    window_id: u32,
    element: AccessibilityUiElement,
    observer: AccessibilityObserver,
    start_rect: Rect,
    target_rect: Rect,
    style: AnimationStyle,
}

impl MovementRenderDispatcher {
    pub const PREFIX: AnimationPrefix = AnimationPrefix::Movement;

    pub fn new(
        window_id: u32,
        element: AccessibilityUiElement,
        observer: AccessibilityObserver,
        start_rect: Rect,
        target_rect: Rect,
        style: AnimationStyle,
    ) -> Self {
        Self {
            window_id,
            element,
            observer,
            start_rect,
            target_rect,
            style,
        }
    }
}

impl RenderDispatcher for MovementRenderDispatcher {
    fn get_animation_key(&self) -> String {
        new_animation_key(MovementRenderDispatcher::PREFIX, self.window_id.to_string())
    }

    fn pre_render(&self) -> eyre::Result<()> {
        // Remove move/resize notifications during animation to prevent
        // flooding the event channel with notifications we generated ourselves
        if let Some(observer) = &self.observer.0 {
            let _ = AccessibilityApi::remove_notification_from_observer(
                observer,
                &self.element,
                kAXWindowMovedNotification,
            );
            let _ = AccessibilityApi::remove_notification_from_observer(
                observer,
                &self.element,
                kAXWindowResizedNotification,
            );
        }
        Ok(())
    }

    fn render(&self, progress: f64) -> eyre::Result<()> {
        let new_rect = self.start_rect.lerp(self.target_rect, progress, self.style);

        // Use with_enhanced_ui_disabled for better performance during animation
        with_enhanced_ui_disabled(&self.element, || {
            // Set position
            let _ = AccessibilityApi::set_attribute_ax_value(
                &self.element,
                kAXPositionAttribute,
                AXValueType::CGPoint,
                CGPoint::new(new_rect.left as CGFloat, new_rect.top as CGFloat),
            );

            // Set size
            let _ = AccessibilityApi::set_attribute_ax_value(
                &self.element,
                kAXSizeAttribute,
                AXValueType::CGSize,
                CGSize::new(new_rect.right as CGFloat, new_rect.bottom as CGFloat),
            );
        });

        Ok(())
    }

    fn post_render(&self) -> eyre::Result<()> {
        // Ensure final position is exact
        with_enhanced_ui_disabled(&self.element, || {
            let _ = AccessibilityApi::set_attribute_ax_value(
                &self.element,
                kAXPositionAttribute,
                AXValueType::CGPoint,
                CGPoint::new(
                    self.target_rect.left as CGFloat,
                    self.target_rect.top as CGFloat,
                ),
            );

            let _ = AccessibilityApi::set_attribute_ax_value(
                &self.element,
                kAXSizeAttribute,
                AXValueType::CGSize,
                CGSize::new(
                    self.target_rect.right as CGFloat,
                    self.target_rect.bottom as CGFloat,
                ),
            );
        });

        // Restore move/resize notifications after animation completes
        if let Some(observer) = &self.observer.0 {
            let _ = AccessibilityApi::add_notification_to_observer(
                observer,
                &self.element,
                kAXWindowMovedNotification,
                None,
            );
            let _ = AccessibilityApi::add_notification_to_observer(
                observer,
                &self.element,
                kAXWindowResizedNotification,
                None,
            );
        }

        Ok(())
    }
}

#[instrument(skip_all)]
unsafe extern "C-unwind" fn window_observer_callback(
    _observer: NonNull<AXObserver>,
    element: NonNull<AXUIElement>,
    notification: NonNull<CFString>,
    _context: *mut c_void,
) {
    unsafe {
        let name =
            AccessibilityApi::copy_attribute_value::<CFString>(element.as_ref(), kAXTitleAttribute)
                .map(|s| s.to_string());

        if let Some(name) = name
            && !name.is_empty()
        {
            let mut process_id = 0;
            element.as_ref().pid(NonNull::from_mut(&mut process_id));

            let window_id = AccessibilityApi::window_id(element.as_ref()).ok();

            if let Ok(notification) =
                AccessibilityNotification::from_str(&notification.as_ref().to_string())
                && let Some(event) = WindowManagerEvent::from_system_notification(
                    SystemNotification::Accessibility(notification),
                    process_id,
                    window_id,
                )
            {
                tracing::debug!(
                    "notification: {}, process: {process_id}, name: \"{name}\"",
                    notification,
                );

                window_manager_event_listener::send_notification(event);
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct WindowInfo {
    name: Option<String>,
    pub owner_pid: i32,
    owner_name: String,
    alpha: f32,
    bounds: WindowBounds,
}

impl WindowInfo {
    pub fn new(entry: NonNull<CFDictionary>) -> Self {
        WindowInfo::from(unsafe { entry.as_ref() })
    }
}

#[derive(Debug, Default)]
#[allow(unused)]
pub struct ValidWindowInfo {
    pub name: String,
    pub owner_pid: i32,
    owner_name: String,
    alpha: f32,
    pub bounds: WindowBounds,
}

impl WindowInfo {
    pub fn validated(self) -> Option<ValidWindowInfo> {
        if let Some(name) = self.name
            && self.alpha != 0.0
            && self.bounds.y != 0.0
            && self.bounds.height != 0.0
            && !name.is_empty()
        {
            return Some(ValidWindowInfo {
                name,
                owner_pid: self.owner_pid,
                owner_name: self.owner_name,
                alpha: self.alpha,
                bounds: self.bounds,
            });
        }

        None
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Window {
    pub id: u32,
    #[serde(skip_deserializing)]
    pub element: AccessibilityUiElement,
    #[serde(skip_deserializing)]
    pub application: Application,
    #[serde(skip_deserializing)]
    observer: AccessibilityObserver,
    pub details: Option<WindowDetails>,
}

#[cfg(test)]
impl From<u32> for Window {
    fn from(id: u32) -> Self {
        Self {
            id,
            element: Default::default(),
            application: Default::default(),
            observer: Default::default(),
            details: None,
        }
    }
}

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowDetails {
    pub title: String,
    pub exe: String,
    pub role: String,
    pub subrole: String,
    pub icon_path: PathBuf,
}

impl From<&Window> for WindowDetails {
    fn from(value: &Window) -> Self {
        Self {
            title: value.title().unwrap_or_default(),
            exe: value.exe().unwrap_or_default(),
            role: value.role().unwrap_or_default(),
            subrole: value.subrole().unwrap_or_default(),
            icon_path: value.icon_path().unwrap_or_default(),
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // this gets called when a cloned Window is dropped, so we need to make sure it only
        // invalidates the observer if the Window is no longer open
        if !self.is_valid() {
            tracing::info!(
                "invalidating window observer for {}",
                self.title()
                    .unwrap_or_else(|| String::from("<NO TITLE FOUND>"))
            );

            // Messages wouldn't close on click which is annoying, this handles that edge case
            // for now - hopefully it doesn't break anything else
            reaper::send_notification(ReaperNotification::InvalidWindow(self.id));

            // make sure the observer gets removed from any run loops
            AccessibilityApi::invalidate_observer(&self.observer);
        }
    }
}

impl Display for Window {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut display = format!(
            "(window_id: {}, process_id: {}",
            self.id, self.application.process_id
        );

        if let Some(title) = self.title() {
            write!(display, ", title: {title}")?;
        }

        if let Some(exe) = self.exe() {
            write!(display, ", exe: {exe}")?;
        }

        if let Some(role) = self.role() {
            write!(display, ", role: {role}")?;
        }

        if let Some(subrole) = self.subrole() {
            write!(display, ", subrole: {subrole}")?;
        }

        write!(display, ")")?;

        write!(f, "{display}")
    }
}

impl Serialize for Window {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("Window", 6)?;
        state.serialize_field("id", &self.id)?;
        state.serialize_field(
            "rect",
            &Rect::from(MacosApi::window_rect(&self.element).unwrap_or_default()),
        )?;
        state.serialize_field("details", &WindowDetails::from(self))?;
        state.end()
    }
}

#[cfg(feature = "schemars")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
struct SerializedWindow {
    id: u32,
    title: String,
    exe: String,
    role: String,
    subrole: String,
    rect: Rect,
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for Window {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Window")
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <SerializedWindow as schemars::JsonSchema>::json_schema(generator)
    }
}

impl Window {
    pub fn new(
        element: CFRetained<AXUIElement>,
        application: Application,
    ) -> Result<Self, AccessibilityError> {
        let observer = AccessibilityApi::create_observer(
            application.process_id,
            Some(window_observer_callback),
        )?;

        Ok(Self {
            id: AccessibilityApi::window_id(&element)?,
            element: AccessibilityUiElement(element),
            application,
            observer: AccessibilityObserver(Some(observer)),
            details: None,
        })
    }

    pub fn is_valid(&self) -> bool {
        AccessibilityApi::copy_attribute_names(&self.element).is_some()
    }

    pub fn is_focused(&self) -> bool {
        MacosApi::foreground_window_id().unwrap_or_default() == self.id
    }

    #[tracing::instrument(skip_all)]
    pub fn observe(
        &self,
        run_loop: &CFRunLoop,
        refcon: Option<*mut c_void>,
    ) -> Result<(), AccessibilityError> {
        tracing::info!("registering observer for {self}");

        AccessibilityApi::add_observer_to_run_loop(
            &self.observer,
            &self.element,
            NOTIFICATIONS,
            run_loop,
            refcon,
        )
    }

    #[tracing::instrument(skip_all)]
    pub fn hide(
        &mut self,
        hiding_position: WindowHidingPosition,
    ) -> Result<(), AccessibilityError> {
        let rect = MacosApi::window_rect(&self.element)?;

        let mut window_restore_positions = WINDOW_RESTORE_POSITIONS.lock();
        if let Entry::Vacant(entry) = window_restore_positions.entry(self.id) {
            entry.insert(rect);
            drop(window_restore_positions);
        }

        if let Some(monitor_size) = CoreGraphicsApi::display_bounds_for_window_rect(rect) {
            // I don't love this, but it's basically what Aerospace does in lieu of an actual "Hide" API
            let hidden_rect = match hiding_position {
                WindowHidingPosition::BottomLeft => {
                    hidden_frame_bottom_left(monitor_size, rect.size)
                }
                WindowHidingPosition::BottomRight => {
                    hidden_frame_bottom_right(monitor_size, rect.size)
                }
            };

            tracing::debug!(
                "hiding {} and setting restore point to {},{}",
                self.title()
                    .unwrap_or_else(|| String::from("<NO TITLE FOUND>")),
                rect.origin.x,
                rect.origin.y,
            );

            self.set_point(hidden_rect.origin, true)?;
            self.set_size(hidden_rect.size, true)?;
        }

        Ok(())
    }

    pub fn minimize(&mut self) -> Result<(), AccessibilityError> {
        let cf_boolean = CFBoolean::new(true);
        let value = &**cf_boolean;
        AccessibilityApi::set_attribute_cf_value(&self.element, kAXMinimizedAttribute, value)
    }

    pub fn unminimize(&mut self) -> Result<(), AccessibilityError> {
        let cf_boolean = CFBoolean::new(false);
        let value = &**cf_boolean;
        AccessibilityApi::set_attribute_cf_value(&self.element, kAXMinimizedAttribute, value)
    }

    #[tracing::instrument(skip_all)]
    pub fn restore(&mut self) -> Result<(), AccessibilityError> {
        let mut should_remove_restore_position = false;
        let mut window_restore_positions = WINDOW_RESTORE_POSITIONS.lock();
        if let Some(cg_rect) = window_restore_positions.get(&self.id) {
            tracing::debug!(
                "restoring {:?} to {cg_rect:?}",
                self.title()
                    .unwrap_or_else(|| String::from("<NO TITLE FOUND>"))
            );

            self.set_point(cg_rect.origin, true)?;
            self.set_size(cg_rect.size, true)?;
            should_remove_restore_position = true;
        }

        if should_remove_restore_position {
            window_restore_positions.remove(&self.id);
        }

        Ok(())
    }

    pub fn title(&self) -> Option<String> {
        AccessibilityApi::copy_attribute_value::<CFString>(&self.element, kAXTitleAttribute)
            .map(|s| s.to_string())
    }

    pub fn exe(&self) -> Option<String> {
        self.application.name()
    }

    pub fn bundle_identifier(&self) -> Option<String> {
        if let Ok(Some(identifier)) = self.running_application().map(|app| app.bundleIdentifier()) {
            Some(identifier.to_string())
        } else {
            None
        }
    }

    pub fn bundle_path(&self) -> Option<PathBuf> {
        if let Ok(Some(path)) = self
            .running_application()
            .map(|app| app.bundleURL())
            .map(|url| url.map(|url| url.to_file_path()))
        {
            path
        } else {
            None
        }
    }

    pub fn path(&self) -> Option<PathBuf> {
        if let Ok(Some(path)) = self
            .running_application()
            .map(|app| app.executableURL())
            .map(|ns_url| ns_url.map(|url| url.to_file_path()))
        {
            path
        } else {
            None
        }
    }

    pub fn icon_path(&self) -> Option<PathBuf> {
        if let Some(path) = self.bundle_path()
            && let Some(bundle) =
                NSBundle::bundleWithPath(&NSString::from_str(&path.to_string_lossy()))
            && let Some(icon_file) =
                bundle.objectForInfoDictionaryKey(&NSString::from_str("CFBundleIconFile"))
            && let Ok(icon_name) = icon_file.downcast::<NSString>()
        {
            let mut icon_path = format!("{}/Contents/Resources/{}", path.display(), icon_name);

            if !icon_path.ends_with(".icns") {
                icon_path.push_str(".icns");
            }

            let path = Path::new(&icon_path);

            if path.exists() {
                return Some(PathBuf::from(path));
            }
        }

        None
    }

    pub fn role(&self) -> Option<String> {
        AccessibilityApi::copy_attribute_value::<CFString>(&self.element, kAXRoleAttribute)
            .map(|s| s.to_string())
    }

    pub fn subrole(&self) -> Option<String> {
        AccessibilityApi::copy_attribute_value::<CFString>(&self.element, kAXSubroleAttribute)
            .map(|s| s.to_string())
    }

    fn running_application(&self) -> Result<Retained<NSRunningApplication>, AccessibilityError> {
        NSRunningApplication::runningApplicationWithProcessIdentifier(self.application.process_id)
            .ok_or(AccessibilityError::Custom(
                AccessibilityCustomError::NSRunningApplication(self.application.process_id),
            ))
    }

    pub fn set_position(&self, rect: &Rect) -> Result<(), AccessibilityError> {
        // Check if animation is enabled (per-animation or global)
        let animation_enabled = {
            let per_animation = ANIMATION_ENABLED_PER_ANIMATION.lock();
            per_animation
                .get(&MovementRenderDispatcher::PREFIX)
                .copied()
                .unwrap_or_else(|| ANIMATION_ENABLED_GLOBAL.load(Ordering::SeqCst))
        };

        if animation_enabled {
            self.set_position_animated(rect)
        } else {
            self.set_position_direct(rect)
        }
    }

    fn set_position_direct(&self, rect: &Rect) -> Result<(), AccessibilityError> {
        self.set_point(
            CGPoint::new(rect.left as CGFloat, rect.top as CGFloat),
            true,
        )?;
        self.set_size(
            CGSize::new(rect.right as CGFloat, rect.bottom as CGFloat),
            true,
        )
    }

    fn set_position_animated(&self, target_rect: &Rect) -> Result<(), AccessibilityError> {
        // Get current window position
        let current_rect = Rect::from(MacosApi::window_rect(&self.element)?);

        // If already at target position, skip animation
        if current_rect == *target_rect {
            return Ok(());
        }

        // Get animation style (per-animation or global)
        let style = {
            let per_animation = ANIMATION_STYLE_PER_ANIMATION.lock();
            per_animation
                .get(&MovementRenderDispatcher::PREFIX)
                .copied()
                .unwrap_or_else(|| *ANIMATION_STYLE_GLOBAL.lock())
        };

        // Get animation duration (per-animation or global)
        let duration = {
            let per_animation = ANIMATION_DURATION_PER_ANIMATION.lock();
            per_animation
                .get(&MovementRenderDispatcher::PREFIX)
                .copied()
                .unwrap_or_else(|| ANIMATION_DURATION_GLOBAL.load(Ordering::SeqCst))
        };

        // Create render dispatcher
        let dispatcher = MovementRenderDispatcher::new(
            self.id,
            self.element.clone(),
            self.observer.clone(),
            current_rect,
            *target_rect,
            style,
        );

        // Run animation (AnimationEngine handles cancellation and registration internally)
        let duration = Duration::from_millis(duration);
        if let Err(e) = AnimationEngine::animate(dispatcher, duration) {
            tracing::warn!("Animation failed for window {}: {}", self.id, e);
            // Fall back to direct positioning
            return self.set_position_direct(target_rect);
        }

        Ok(())
    }

    pub fn focus(&self, mouse_follows_focus: bool) -> Result<(), LibraryError> {
        match self.running_application() {
            Ok(running_application) => {
                running_application.activateWithOptions(NSApplicationActivationOptions::empty());
            }
            Err(error) => {
                tracing::warn!(
                    "failed to get running application for {} ({:?}): {error}",
                    self.application.process_id,
                    self.application.name()
                );
            }
        }

        let cf_boolean = CFBoolean::new(true);
        let value = &**cf_boolean;
        AccessibilityApi::set_attribute_cf_value(&self.element, kAXMainAttribute, value)?;

        if mouse_follows_focus {
            MacosApi::center_cursor_in_rect(&MacosApi::window_rect(&self.element)?.into())?
        }

        Ok(())
    }

    pub fn raise(&self) -> Result<(), AccessibilityError> {
        let cf_boolean = CFBoolean::new(true);
        let value = &**cf_boolean;
        AccessibilityApi::set_attribute_cf_value(&self.element, kAXMainAttribute, value)?;
        AccessibilityApi::set_attribute_cf_value(&self.element, kAXFocusedAttribute, value)
    }

    pub fn set_point(&self, point: CGPoint, should_reap: bool) -> Result<(), AccessibilityError> {
        let result = AccessibilityApi::set_attribute_ax_value(
            &self.element,
            kAXPositionAttribute,
            AXValueType::CGPoint,
            point,
        );

        if should_reap {
            reaper::notify_on_error(self, result)
        } else {
            result
        }
    }

    pub fn set_size(&self, size: CGSize, should_reap: bool) -> Result<(), AccessibilityError> {
        let result = AccessibilityApi::set_attribute_ax_value(
            &self.element,
            kAXSizeAttribute,
            AXValueType::CGSize,
            size,
        );

        if should_reap {
            reaper::notify_on_error(self, result)
        } else {
            result
        }
    }

    pub fn center(&mut self, work_area: &Rect, resize: bool) -> Result<(), AccessibilityError> {
        let (target_width, target_height) = if resize {
            let (aspect_ratio_width, aspect_ratio_height) = FLOATING_WINDOW_TOGGLE_ASPECT_RATIO
                .lock()
                .width_and_height();
            let target_height = work_area.bottom / 2;
            let target_width = (target_height * aspect_ratio_width) / aspect_ratio_height;
            (target_width, target_height)
        } else {
            let current_rect = Rect::from(MacosApi::window_rect(&self.element)?);
            (current_rect.right, current_rect.bottom)
        };

        let x = work_area.left + ((work_area.right - target_width) / 2);
        let y = work_area.top + ((work_area.bottom - target_height) / 2);

        self.set_position(&Rect {
            left: x,
            top: y,
            right: target_width,
            bottom: target_height,
        })
    }

    pub fn move_to_area(
        &self,
        current_area: &Rect,
        target_area: &Rect,
    ) -> Result<(), AccessibilityError> {
        let current_rect = Rect::from(MacosApi::window_rect(&self.element)?);
        let x_diff = target_area.left - current_area.left;
        let y_diff = target_area.top - current_area.top;
        let x_ratio = f32::abs((target_area.right as f32) / (current_area.right as f32));
        let y_ratio = f32::abs((target_area.bottom as f32) / (current_area.bottom as f32));
        let window_relative_x = current_rect.left - current_area.left;
        let window_relative_y = current_rect.top - current_area.top;
        let corrected_relative_x = (window_relative_x as f32 * x_ratio) as i32;
        let corrected_relative_y = (window_relative_y as f32 * y_ratio) as i32;
        let window_x = current_area.left + corrected_relative_x;
        let window_y = current_area.top + corrected_relative_y;
        let left = x_diff + window_x;
        let top = y_diff + window_y;

        let corrected_width = (current_rect.right as f32 * x_ratio) as i32;
        let corrected_height = (current_rect.bottom as f32 * y_ratio) as i32;

        let new_rect = Rect {
            left,
            top,
            right: corrected_width,
            bottom: corrected_height,
        };

        // TODO: figure out what to do about maximized windows on macOS
        // let is_maximized = &new_rect == target_area;
        // if is_maximized {
        //     windows_api::WindowsApi::unmaximize_window(self.hwnd);
        //     let animation_enabled = ANIMATION_ENABLED_PER_ANIMATION.lock();
        //     let move_enabled = animation_enabled
        //         .get(&MovementRenderDispatcher::PREFIX)
        //         .is_some_and(|v| *v);
        //     drop(animation_enabled);
        //
        //     if move_enabled || ANIMATION_ENABLED_GLOBAL.load(Ordering::SeqCst) {
        //         let anim_count = ANIMATION_MANAGER
        //             .lock()
        //             .count_in_progress(MovementRenderDispatcher::PREFIX);
        //         self.set_position(&new_rect, true)?;
        //         let hwnd = self.hwnd;
        //         // Wait for the animation to finish before maximizing the window again, otherwise
        //         // we would be maximizing the window on the current monitor anyway
        //         thread::spawn(move || {
        //             let mut new_anim_count = ANIMATION_MANAGER
        //                 .lock()
        //                 .count_in_progress(MovementRenderDispatcher::PREFIX);
        //             let mut max_wait = 2000; // Max waiting time. No one will be using an animation longer than 2s, right? RIGHT??? WHY?
        //             while new_anim_count > anim_count && max_wait > 0 {
        //                 thread::sleep(Duration::from_millis(10));
        //                 new_anim_count = ANIMATION_MANAGER
        //                     .lock()
        //                     .count_in_progress(MovementRenderDispatcher::PREFIX);
        //                 max_wait -= 1;
        //             }
        //             windows_api::WindowsApi::maximize_window(hwnd);
        //         });
        //     } else {
        //         self.set_position(&new_rect, true)?;
        //         windows_api::WindowsApi::maximize_window(self.hwnd);
        //     }
        // } else {
        self.set_position(&new_rect)?;
        // }

        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub fn should_manage(
        &self,
        event: Option<WindowManagerEvent>,
        debug: &mut RuleDebug,
    ) -> eyre::Result<bool> {
        if !self.is_valid() {
            return Ok(false);
        }

        debug.is_window = true;

        // let rect = Rect::from(MacosApi::window_rect(&self.element).unwrap_or_default());
        //
        // if rect.right < MINIMUM_WIDTH.load(Ordering::SeqCst) {
        //     return Ok(false);
        // }
        //
        // debug.has_minimum_width = true;
        //
        // if rect.bottom < MINIMUM_HEIGHT.load(Ordering::SeqCst) {
        //     return Ok(false);
        // }
        //
        // debug.has_minimum_height = true;

        if self.title().is_none() {
            return Ok(false);
        }

        debug.has_title = true;

        // let is_cloaked = self.is_cloaked().unwrap_or_default();
        //
        // debug.is_cloaked = is_cloaked;

        // let mut allow_cloaked = false;

        // if let Some(event) = event {
        //     if matches!(
        //         event,
        //         WindowManagerEvent::Hide(_, _) | WindowManagerEvent::Cloak(_, _)
        //     ) {
        //         allow_cloaked = true;
        //     }
        // }

        // debug.allow_cloaked = allow_cloaked;

        // match (allow_cloaked, is_cloaked) {
        //     // If allowing cloaked windows, we don't need to check the cloaked status
        //     (true, _) |
        //     // If not allowing cloaked windows, we need to ensure the window is not cloaked
        //     (false, false) => {
        if let (Some(title), Some(exe_name), Some(role), Some(subrole), Some(path)) = (
            self.title(),
            self.exe(),
            self.role(),
            self.subrole(),
            self.path(),
        ) {
            debug.title = Some(title.clone());
            debug.exe_name = Some(exe_name.clone());
            debug.role = Some(role.clone());
            debug.subrole = Some(subrole.clone());
            debug.path = Some(path.to_string_lossy().to_string());
            // calls for styles can fail quite often for events with windows that aren't really "windows"
            // since we have moved up calls of should_manage to the beginning of the process_event handler,
            // we should handle failures here gracefully to be able to continue the execution of process_event
            // if let (Ok(style), Ok(ex_style)) = (&self.style(), &self.ex_style()) {
            //     debug.window_style = Some(*style);
            //     debug.extended_window_style = Some(*ex_style);
            let eligible = window_is_eligible(
                self.id,
                &title,
                &exe_name,
                &[&role, &subrole],
                &path.to_string_lossy(),
                event,
                debug,
            );
            // debug.should_manage = eligible;
            return Ok(eligible);
            // }
        }
        // }
        //     _ => {}
        // }
        Ok(false)
    }
}

pub struct AdhocWindow;

impl AdhocWindow {
    pub fn process_id(element: &CFRetained<AXUIElement>) -> Option<i32> {
        let mut process_id = 0;

        unsafe {
            element.pid(NonNull::from_mut(&mut process_id));
        }

        if process_id != 0 {
            Some(process_id)
        } else {
            None
        }
    }

    pub fn exe(element: &CFRetained<AXUIElement>) -> Option<String> {
        let parent =
            AccessibilityApi::copy_attribute_value::<AXUIElement>(element, kAXParentAttribute)?;

        AccessibilityApi::copy_attribute_value::<CFString>(&parent, kAXTitleAttribute)
            .map(|s| s.to_string())
    }

    pub fn role(element: &CFRetained<AXUIElement>) -> Option<String> {
        AccessibilityApi::copy_attribute_value::<CFString>(element, kAXRoleAttribute)
            .map(|s| s.to_string())
    }

    pub fn subrole(element: &CFRetained<AXUIElement>) -> Option<String> {
        AccessibilityApi::copy_attribute_value::<CFString>(element, kAXSubroleAttribute)
            .map(|s| s.to_string())
    }

    pub fn title(element: &CFRetained<AXUIElement>) -> Option<String> {
        AccessibilityApi::copy_attribute_value::<CFString>(element, kAXTitleAttribute)
            .map(|s| s.to_string())
    }

    pub fn minimize(element: &CFRetained<AXUIElement>) -> Result<(), AccessibilityError> {
        let cf_boolean = CFBoolean::new(true);
        let value = &**cf_boolean;
        AccessibilityApi::set_attribute_cf_value(element, kAXMinimizedAttribute, value)
    }

    pub fn close(element: &CFRetained<AXUIElement>) -> Result<(), AccessibilityError> {
        if let Some(close_button) =
            AccessibilityApi::copy_attribute_value::<AXUIElement>(element, kAXCloseButtonAttribute)
        {
            AccessibilityApi::perform_action(&close_button, kAXPressAction)
        } else {
            Ok(())
        }
    }

    #[tracing::instrument(skip_all)]
    pub fn hide(
        id: u32,
        element: &CFRetained<AXUIElement>,
        hiding_position: WindowHidingPosition,
    ) -> Result<(), AccessibilityError> {
        let mut window_restore_positions = WINDOW_RESTORE_POSITIONS.lock();
        if let Entry::Vacant(entry) = window_restore_positions.entry(id) {
            let rect = MacosApi::window_rect(element)?;
            if let Some(monitor_size) = CoreGraphicsApi::display_bounds_for_window_rect(rect) {
                entry.insert(rect);
                drop(window_restore_positions);

                // I don't love this, but it's basically what Aerospace does in lieu of an actual "Hide" API
                let hidden_rect = match hiding_position {
                    WindowHidingPosition::BottomLeft => {
                        hidden_frame_bottom_left(monitor_size, rect.size)
                    }
                    WindowHidingPosition::BottomRight => {
                        hidden_frame_bottom_right(monitor_size, rect.size)
                    }
                };

                tracing::debug!(
                    "hiding window with id {id} and setting restore point to {},{}",
                    rect.origin.x,
                    rect.origin.y,
                );

                AccessibilityApi::set_attribute_ax_value(
                    element,
                    kAXPositionAttribute,
                    AXValueType::CGPoint,
                    hidden_rect.origin,
                )?;

                AccessibilityApi::set_attribute_ax_value(
                    element,
                    kAXSizeAttribute,
                    AXValueType::CGSize,
                    hidden_rect.size,
                )?;
            }
        }

        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub fn restore(id: u32, element: &CFRetained<AXUIElement>) -> Result<(), AccessibilityError> {
        let mut should_remove_restore_position = false;
        let mut window_restore_positions = WINDOW_RESTORE_POSITIONS.lock();
        if let Some(cg_rect) = window_restore_positions.get(&id) {
            tracing::debug!("restoring window with id {id} to {cg_rect:?}",);

            AccessibilityApi::set_attribute_ax_value(
                element,
                kAXPositionAttribute,
                AXValueType::CGPoint,
                cg_rect.origin,
            )?;

            AccessibilityApi::set_attribute_ax_value(
                element,
                kAXSizeAttribute,
                AXValueType::CGSize,
                cg_rect.size,
            )?;

            should_remove_restore_position = true;
        }

        if should_remove_restore_position {
            window_restore_positions.remove(&id);
        }

        Ok(())
    }

    pub fn raise(element: &CFRetained<AXUIElement>) -> Result<(), AccessibilityError> {
        let cf_boolean = CFBoolean::new(true);
        let value = &**cf_boolean;
        AccessibilityApi::set_attribute_cf_value(element, kAXMainAttribute, value)?;
        AccessibilityApi::set_attribute_cf_value(element, kAXFocusedAttribute, value)
    }
}

impl From<&CFDictionary> for WindowInfo {
    fn from(value: &CFDictionary) -> Self {
        unsafe {
            Self {
                name: cf_dictionary_value::<CFString>(value, kCGWindowName)
                    .map(|s| s.as_ref().to_string())
                    .and_then(|s| if s.is_empty() { None } else { Some(s) }),
                owner_pid: cf_dictionary_value::<CFNumber>(value, kCGWindowOwnerPID)
                    .and_then(|s| s.as_ref().as_i32())
                    .expect("window must have an owner process id"),
                owner_name: cf_dictionary_value::<CFString>(value, kCGWindowOwnerName)
                    .map(|s| s.as_ref().to_string())
                    .expect("window must have an owner name"),
                alpha: cf_dictionary_value::<CFNumber>(value, kCGWindowAlpha)
                    .and_then(|s| s.as_ref().as_f32())
                    .expect("window must have an alpha value"),

                bounds: if let Some(dict) =
                    cf_dictionary_value::<CFDictionary>(value, kCGWindowBounds).as_ref()
                {
                    WindowBounds::from(dict.as_ref())
                } else {
                    Default::default()
                },
            }
        }
    }
}

#[derive(Default, Debug, Copy, Clone)]
pub struct WindowBounds {
    pub height: f32,
    pub width: f32,
    pub x: f32,
    pub y: f32,
}

impl From<&CFDictionary> for WindowBounds {
    fn from(value: &CFDictionary) -> Self {
        unsafe {
            Self {
                height: cf_dictionary_value::<CFNumber>(
                    value,
                    &CFString::from_static_str("Height"),
                )
                .and_then(|val| val.as_ref().as_f32())
                .unwrap_or_default(),
                width: cf_dictionary_value::<CFNumber>(value, &CFString::from_static_str("Width"))
                    .and_then(|val| val.as_ref().as_f32())
                    .unwrap_or_default(),
                x: cf_dictionary_value::<CFNumber>(value, &CFString::from_static_str("X"))
                    .and_then(|val| val.as_ref().as_f32())
                    .unwrap_or_default(),
                y: cf_dictionary_value::<CFNumber>(value, &CFString::from_static_str("Y"))
                    .and_then(|val| val.as_ref().as_f32())
                    .unwrap_or_default(),
            }
        }
    }
}

#[derive(Copy, Clone, Debug, Display, EnumString, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum AspectRatio {
    /// A predefined aspect ratio
    Predefined(PredefinedAspectRatio),
    /// A custom W:H aspect ratio
    Custom(i32, i32),
}

impl Default for AspectRatio {
    fn default() -> Self {
        AspectRatio::Predefined(PredefinedAspectRatio::default())
    }
}

#[derive(Copy, Clone, Debug, Default, Display, EnumString, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum PredefinedAspectRatio {
    /// 21:9
    Ultrawide,
    /// 16:9
    Widescreen,
    /// 4:3
    #[default]
    Standard,
}

impl AspectRatio {
    pub fn width_and_height(self) -> (i32, i32) {
        match self {
            AspectRatio::Predefined(predefined) => match predefined {
                PredefinedAspectRatio::Ultrawide => (21, 9),
                PredefinedAspectRatio::Widescreen => (16, 9),
                PredefinedAspectRatio::Standard => (4, 3),
            },
            AspectRatio::Custom(w, h) => (w, h),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RuleDebug {
    pub should_manage: bool,
    pub is_window: bool,
    // pub has_minimum_width: bool,
    // pub has_minimum_height: bool,
    pub has_title: bool,
    // pub is_cloaked: bool,
    // pub allow_cloaked: bool,
    // pub allow_layered_transparency: bool,
    // pub window_style: Option<WindowStyle>,
    // pub extended_window_style: Option<ExtendedWindowStyle>,
    pub title: Option<String>,
    pub exe_name: Option<String>,
    pub role: Option<String>,
    pub subrole: Option<String>,
    pub path: Option<String>,
    pub matches_permaignore_class: Option<String>,
    pub matches_ignore_identifier: Option<MatchingRule>,
    pub matches_managed_override: Option<MatchingRule>,
    // pub matches_layered_whitelist: Option<MatchingRule>,
    pub matches_floating_applications: Option<MatchingRule>,
    // pub matches_wsl2_gui: Option<String>,
    // pub matches_no_titlebar: Option<MatchingRule>,
}

#[allow(clippy::too_many_arguments)]
fn window_is_eligible(
    _window_id: u32,
    title: &str,
    exe_name: &str,
    classes: &[&str],
    path: &str,
    // style: &WindowStyle,
    // ex_style: &ExtendedWindowStyle,
    _event: Option<WindowManagerEvent>,
    debug: &mut RuleDebug,
) -> bool {
    {
        let permaignore_classes = PERMAIGNORE_CLASSES.lock();
        for class in classes {
            if permaignore_classes.contains(&class.to_string()) {
                debug.matches_permaignore_class = Some(class.to_string());
                return false;
            }
        }
    }

    let regex_identifiers = REGEX_IDENTIFIERS.lock();

    let ignore_identifiers = IGNORE_IDENTIFIERS.lock();
    let should_ignore = if let Some(rule) = should_act(
        title,
        exe_name,
        classes,
        path,
        &ignore_identifiers,
        &regex_identifiers,
    ) {
        debug.matches_ignore_identifier = Some(rule);
        true
    } else {
        false
    };

    let manage_identifiers = MANAGE_IDENTIFIERS.lock();
    let managed_override = if let Some(rule) = should_act(
        title,
        exe_name,
        classes,
        path,
        &manage_identifiers,
        &regex_identifiers,
    ) {
        debug.matches_managed_override = Some(rule);
        true
    } else {
        false
    };

    let floating_identifiers = FLOATING_APPLICATIONS.lock();
    if let Some(rule) = should_act(
        title,
        exe_name,
        classes,
        path,
        &floating_identifiers,
        &regex_identifiers,
    ) {
        debug.matches_floating_applications = Some(rule);
    }

    if should_ignore && !managed_override {
        return false;
    }

    // let layered_whitelist = LAYERED_WHITELIST.lock();
    // let mut allow_layered = if let Some(rule) = should_act(
    //     title,
    //     exe_name,
    //     class,
    //     path,
    //     &layered_whitelist,
    //     &regex_identifiers,
    // ) {
    //     debug.matches_layered_whitelist = Some(rule);
    //     true
    // } else {
    //     false
    // };
    //
    // let known_layered_hwnds = transparency_manager::known_hwnds();

    // allow_layered = if known_layered_hwnds.contains(&hwnd)
    //     // we always want to process hide events for windows with transparency, even on other
    //     // monitors, because we don't want to be left with ghost tiles
    //     || matches!(event, Some(WindowManagerEvent::Hide(_, _)))
    // {
    //     debug.allow_layered_transparency = true;
    //     true
    // } else {
    //     allow_layered
    // };
    //
    // let allow_wsl2_gui = {
    //     let wsl2_ui_processes = WSL2_UI_PROCESSES.lock();
    //     let allow = wsl2_ui_processes.contains(exe_name);
    //     if allow {
    //         debug.matches_wsl2_gui = Some(exe_name.clone())
    //     }
    //
    //     allow
    // };

    // let titlebars_removed = NO_TITLEBAR.lock();
    // let allow_titlebar_removed = if let Some(rule) = should_act(
    //     title,
    //     exe_name,
    //     class,
    //     path,
    //     &titlebars_removed,
    //     &regex_identifiers,
    // ) {
    //     debug.matches_no_titlebar = Some(rule);
    //     true
    // } else {
    //     false
    // };
    //
    // {
    //     let slow_application_identifiers = SLOW_APPLICATION_IDENTIFIERS.lock();
    //     let should_sleep = should_act(
    //         title,
    //         exe_name,
    //         class,
    //         path,
    //         &slow_application_identifiers,
    //         &regex_identifiers,
    //     )
    //         .is_some();
    //
    //     if should_sleep {
    //         std::thread::sleep(Duration::from_millis(
    //             SLOW_APPLICATION_COMPENSATION_TIME.load(Ordering::SeqCst),
    //         ));
    //     }
    // }

    // TODO: not sure about this new manage by default base case for macOS
    true
}

#[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
pub fn should_act(
    title: &str,
    exe_name: &str,
    classes: &[&str],
    path: &str,
    identifiers: &[MatchingRule],
    regex_identifiers: &HashMap<String, Regex>,
) -> Option<MatchingRule> {
    let mut matching_rule = None;
    for rule in identifiers {
        match rule {
            MatchingRule::Simple(identifier) => {
                if should_act_individual(
                    title,
                    exe_name,
                    classes,
                    path,
                    identifier,
                    regex_identifiers,
                ) {
                    matching_rule = Some(rule.clone());
                };
            }
            MatchingRule::Composite(identifiers) => {
                let mut composite_results = vec![];
                for identifier in identifiers {
                    composite_results.push(should_act_individual(
                        title,
                        exe_name,
                        classes,
                        path,
                        identifier,
                        regex_identifiers,
                    ));
                }

                if composite_results.iter().all(|&x| x) {
                    matching_rule = Some(rule.clone());
                }
            }
        }
    }

    matching_rule
}

pub fn should_act_individual(
    title: &str,
    exe_name: &str,
    classes: &[&str],
    path: &str,
    identifier: &IdWithIdentifier,
    regex_identifiers: &HashMap<String, Regex>,
) -> bool {
    let mut should_act = false;

    let mut identifier = identifier.clone();
    identifier.id = identifier.id.replace(".exe", "");

    match identifier.matching_strategy {
        None | Some(MatchingStrategy::Legacy) => match identifier.kind {
            ApplicationIdentifier::Title => {
                if title.starts_with(&identifier.id) || title.ends_with(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Class => {
                for class in classes {
                    if class.starts_with(&identifier.id) || class.ends_with(&identifier.id) {
                        should_act = true;
                    }
                }
            }
            ApplicationIdentifier::Exe => {
                if exe_name.eq(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Path => {
                if path.eq(&identifier.id) {
                    should_act = true;
                }
            }
        },
        Some(MatchingStrategy::Equals) => match identifier.kind {
            ApplicationIdentifier::Title => {
                if title.eq(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Class => {
                for class in classes {
                    if class.eq(&identifier.id) {
                        should_act = true;
                    }
                }
            }
            ApplicationIdentifier::Exe => {
                if exe_name.eq(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Path => {
                if path.eq(&identifier.id) {
                    should_act = true;
                }
            }
        },
        Some(MatchingStrategy::DoesNotEqual) => match identifier.kind {
            ApplicationIdentifier::Title => {
                if !title.eq(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Class => {
                for class in classes {
                    if !class.eq(&identifier.id) {
                        should_act = true;
                    }
                }
            }
            ApplicationIdentifier::Exe => {
                if !exe_name.eq(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Path => {
                if !path.eq(&identifier.id) {
                    should_act = true;
                }
            }
        },
        Some(MatchingStrategy::StartsWith) => match identifier.kind {
            ApplicationIdentifier::Title => {
                if title.starts_with(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Class => {
                for class in classes {
                    if class.starts_with(&identifier.id) {
                        should_act = true;
                    }
                }
            }
            ApplicationIdentifier::Exe => {
                if exe_name.starts_with(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Path => {
                if path.starts_with(&identifier.id) {
                    should_act = true;
                }
            }
        },
        Some(MatchingStrategy::DoesNotStartWith) => match identifier.kind {
            ApplicationIdentifier::Title => {
                if !title.starts_with(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Class => {
                for class in classes {
                    if !class.starts_with(&identifier.id) {
                        should_act = true;
                    }
                }
            }
            ApplicationIdentifier::Exe => {
                if !exe_name.starts_with(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Path => {
                if !path.starts_with(&identifier.id) {
                    should_act = true;
                }
            }
        },
        Some(MatchingStrategy::EndsWith) => match identifier.kind {
            ApplicationIdentifier::Title => {
                if title.ends_with(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Class => {
                for class in classes {
                    if class.ends_with(&identifier.id) {
                        should_act = true;
                    }
                }
            }
            ApplicationIdentifier::Exe => {
                if exe_name.ends_with(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Path => {
                if path.ends_with(&identifier.id) {
                    should_act = true;
                }
            }
        },
        Some(MatchingStrategy::DoesNotEndWith) => match identifier.kind {
            ApplicationIdentifier::Title => {
                if !title.ends_with(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Class => {
                for class in classes {
                    if !class.ends_with(&identifier.id) {
                        should_act = true;
                    }
                }
            }
            ApplicationIdentifier::Exe => {
                if !exe_name.ends_with(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Path => {
                if !path.ends_with(&identifier.id) {
                    should_act = true;
                }
            }
        },
        Some(MatchingStrategy::Contains) => match identifier.kind {
            ApplicationIdentifier::Title => {
                if title.contains(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Class => {
                for class in classes {
                    if class.contains(&identifier.id) {
                        should_act = true;
                    }
                }
            }
            ApplicationIdentifier::Exe => {
                if exe_name.contains(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Path => {
                if path.contains(&identifier.id) {
                    should_act = true;
                }
            }
        },
        Some(MatchingStrategy::DoesNotContain) => match identifier.kind {
            ApplicationIdentifier::Title => {
                if !title.contains(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Class => {
                for class in classes {
                    if !class.contains(&identifier.id) {
                        should_act = true;
                    }
                }
            }
            ApplicationIdentifier::Exe => {
                if !exe_name.contains(&identifier.id) {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Path => {
                if !path.contains(&identifier.id) {
                    should_act = true;
                }
            }
        },
        Some(MatchingStrategy::Regex) => match identifier.kind {
            ApplicationIdentifier::Title => {
                if let Some(re) = regex_identifiers.get(&identifier.id)
                    && re.is_match(title)
                {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Class => {
                if let Some(re) = regex_identifiers.get(&identifier.id) {
                    for class in classes {
                        if re.is_match(class) {
                            should_act = true;
                        }
                    }
                }
            }
            ApplicationIdentifier::Exe => {
                if let Some(re) = regex_identifiers.get(&identifier.id)
                    && re.is_match(exe_name)
                {
                    should_act = true;
                }
            }
            ApplicationIdentifier::Path => {
                if let Some(re) = regex_identifiers.get(&identifier.id)
                    && re.is_match(path)
                {
                    should_act = true;
                }
            }
        },
    }

    should_act
}
