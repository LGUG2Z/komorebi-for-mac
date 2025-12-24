#![allow(non_upper_case_globals, unused)]

use crate::accessibility::AccessibilityApi;
use crate::accessibility::error::AccessibilityError;
use objc2_application_services::AXError;
use objc2_application_services::AXUIElement;
use objc2_core_foundation::CFBoolean;
use objc2_core_graphics::CGWindowID;

pub const kAXEnhancedUserInterface: &str = "AXEnhancedUserInterface";

// this is the only private API call Aerospace uses, so I think we're ok to use it too
// https://github.com/nikitabobko/AeroSpace?tab=readme-ov-file#project-values
unsafe extern "C" {
    /// Extract `window_id` from an AXUIElement.
    pub fn _AXUIElementGetWindow(elem: &AXUIElement, window_id: *mut CGWindowID) -> AXError;
}

/// Get the current state of Enhanced User Interface for an element
pub fn get_enhanced_user_interface(element: &AXUIElement) -> bool {
    AccessibilityApi::copy_attribute_value::<CFBoolean>(element, kAXEnhancedUserInterface)
        .map(|b| b.as_bool())
        .unwrap_or(false)
}

/// Set the Enhanced User Interface state for an element
pub fn set_enhanced_user_interface(
    element: &AXUIElement,
    enabled: bool,
) -> Result<(), AccessibilityError> {
    let cf_boolean = CFBoolean::new(enabled);
    let value = &**cf_boolean;
    AccessibilityApi::set_attribute_cf_value(element, kAXEnhancedUserInterface, value)
}

/// Execute a closure with Enhanced User Interface temporarily disabled.
/// This can improve performance during window positioning operations.
pub fn with_enhanced_ui_disabled<F, R>(element: &AXUIElement, f: F) -> R
where
    F: FnOnce() -> R,
{
    let original_state = get_enhanced_user_interface(element);

    if original_state && let Err(error) = set_enhanced_user_interface(element, false) {
        tracing::warn!("Failed to disable Enhanced User Interface: {:?}", error);
    }

    let result = f();

    if original_state && let Err(error) = set_enhanced_user_interface(element, true) {
        tracing::warn!("Failed to restore Enhanced User Interface: {:?}", error);
    }

    result
}

/// Execute a closure with system-wide Enhanced User Interface temporarily disabled.
pub fn with_system_enhanced_ui_disabled<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let system_element = unsafe { AXUIElement::new_system_wide() };
    with_enhanced_ui_disabled(&system_element, f)
}
