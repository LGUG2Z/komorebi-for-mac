use objc2_application_services::AXError;
use objc2_application_services::AXUIElement;
use objc2_core_graphics::CGWindowID;

// this is the only private API call Aerospace uses, so I think we're ok to use it too
// https://github.com/nikitabobko/AeroSpace?tab=readme-ov-file#project-values
unsafe extern "C" {
    /// Extract `window_id` from an AXUIElement.
    pub fn _AXUIElementGetWindow(elem: &AXUIElement, window_id: *mut CGWindowID) -> AXError;
}
