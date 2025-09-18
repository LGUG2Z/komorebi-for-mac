use crate::core_graphics::error::CoreGraphicsError;
use objc2_core_foundation::CFArray;
use objc2_core_foundation::CFRetained;
use objc2_core_foundation::CGFloat;
use objc2_core_foundation::CGPoint;
use objc2_core_foundation::CGRect;
use objc2_core_graphics::CGDisplayBounds;
use objc2_core_graphics::CGGetDisplaysWithPoint;
use objc2_core_graphics::CGGetOnlineDisplayList;
use objc2_core_graphics::CGRectIntersectsRect;
use objc2_core_graphics::CGWarpMouseCursorPosition;
use objc2_core_graphics::CGWindowListCopyWindowInfo;
use objc2_core_graphics::CGWindowListOption;
use objc2_core_graphics::kCGNullWindowID;

pub mod error;

pub struct CoreGraphicsApi;

impl CoreGraphicsApi {
    pub fn warp_mouse_cursor_position(x: i32, y: i32) -> Result<(), CoreGraphicsError> {
        match unsafe {
            CoreGraphicsError::from(CGWarpMouseCursorPosition(CGPoint::new(
                x as CGFloat,
                y as CGFloat,
            )))
        } {
            CoreGraphicsError::Success => Ok(()),
            error => Err(error),
        }
    }

    pub fn contains_rect(smaller: CGRect, bigger: CGRect) -> bool {
        unsafe { CGRectIntersectsRect(smaller, bigger) }
    }

    pub fn connected_display_ids() -> Result<Vec<u32>, CoreGraphicsError> {
        let mut displays: Vec<u32> = Vec::with_capacity(16);
        let mut display_count = 0;

        unsafe {
            match CoreGraphicsError::from(CGGetOnlineDisplayList(
                displays.capacity() as u32,
                displays.as_mut_ptr(),
                &mut display_count,
            )) {
                CoreGraphicsError::Success => {
                    displays.set_len(display_count as usize);
                    Ok(displays)
                }
                error => Err(error),
            }
        }
    }

    pub fn display_bounds(display_id: u32) -> CGRect {
        unsafe { CGDisplayBounds(display_id) }
    }

    pub fn display_bounds_for_window_rect(window_rect: CGRect) -> Option<CGRect> {
        let mut displays: Vec<u32> = Vec::with_capacity(1);
        let mut display_count = 0;

        unsafe {
            match CoreGraphicsError::from(CGGetDisplaysWithPoint(
                window_rect.origin,
                displays.capacity() as u32,
                displays.as_mut_ptr(),
                &mut display_count,
            )) {
                CoreGraphicsError::Success => {
                    displays.set_len(display_count as usize);
                }
                error => {
                    tracing::error!("failed to find display for point: {error}");
                    return None;
                }
            }
        }

        displays
            .first()
            .map(|display| CoreGraphicsApi::display_bounds(*display))
    }

    pub fn window_list_info() -> Option<CFRetained<CFArray>> {
        unsafe {
            CGWindowListCopyWindowInfo(
                // this is still way too many bogus windows
                CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
                // required when using OnScreenOnly
                kCGNullWindowID,
            )
        }
    }
}
