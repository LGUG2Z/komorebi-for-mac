use objc2_core_foundation::CGRect;
use objc2_core_foundation::CGSize;

#[derive(Debug, Copy, Clone)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl From<CGSize> for Rect {
    fn from(value: CGSize) -> Self {
        Self {
            left: 0.0,
            top: 0.0,
            right: value.width as f32,
            bottom: value.height as f32,
        }
    }
}

impl From<CGRect> for Rect {
    fn from(value: CGRect) -> Self {
        Self {
            left: value.origin.x as f32,
            top: value.origin.y as f32,
            right: value.size.width as f32,
            bottom: value.size.height as f32,
        }
    }
}
