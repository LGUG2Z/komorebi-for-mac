use crate::core_graphics::CoreGraphicsApi;
use crate::window::WindowBounds;
use objc2_core_foundation::CGFloat;
use objc2_core_foundation::CGPoint;
use objc2_core_foundation::CGRect;
use objc2_core_foundation::CGSize;

#[derive(Debug, Default, Copy, Clone, PartialEq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl From<CGSize> for Rect {
    fn from(value: CGSize) -> Self {
        Self {
            left: 0,
            top: 0,
            right: value.width as i32,
            bottom: value.height as i32,
        }
    }
}

impl From<CGRect> for Rect {
    fn from(value: CGRect) -> Self {
        Self {
            left: value.origin.x as i32,
            top: value.origin.y as i32,
            right: value.size.width as i32,
            bottom: value.size.height as i32,
        }
    }
}

impl From<&Rect> for CGRect {
    fn from(value: &Rect) -> Self {
        Self {
            origin: CGPoint {
                x: value.left as CGFloat,
                y: value.top as CGFloat,
            },
            size: CGSize {
                width: value.right as CGFloat,
                height: value.bottom as CGFloat,
            },
        }
    }
}

impl From<WindowBounds> for Rect {
    fn from(value: WindowBounds) -> Self {
        Self {
            left: value.x as i32,
            top: value.y as i32,
            right: value.width as i32,
            bottom: value.height as i32,
        }
    }
}

impl Rect {
    pub fn contains(&self, other: &Rect) -> bool {
        CoreGraphicsApi::contains_rect(other.into(), self.into())
    }

    /// Decrease the size of self by the padding amount.
    pub fn add_padding<T>(&mut self, padding: T)
    where
        T: Into<Option<i32>>,
    {
        if let Some(padding) = padding.into() {
            self.left += padding;
            self.top += padding;
            self.right -= padding * 2;
            self.bottom -= padding * 2;
        }
    }

    /// Increase the size of self by the margin amount.
    pub fn add_margin(&mut self, margin: i32) {
        self.left -= margin;
        self.top -= margin;
        self.right += margin * 2;
        self.bottom += margin * 2;
    }

    pub fn left_padding(&mut self, padding: i32) {
        self.left += padding;
    }

    pub fn right_padding(&mut self, padding: i32) {
        self.right -= padding;
    }

    #[must_use]
    pub const fn contains_point(&self, point: (i32, i32)) -> bool {
        point.0 >= self.left
            && point.0 <= self.left + self.right
            && point.1 >= self.top
            && point.1 <= self.top + self.bottom
    }
}
