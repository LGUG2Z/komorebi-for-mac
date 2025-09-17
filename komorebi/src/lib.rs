#![warn(clippy::all)]

use crate::accessibility::error::AccessibilityError;
use crate::core_graphics::error::CoreGraphicsError;
use objc2_core_foundation::CFArray;
use objc2_core_foundation::CFDictionary;
use objc2_core_foundation::CFString;
use std::ptr::NonNull;

#[macro_use]
pub mod ring;

pub mod accessibility;
pub mod application;
pub mod core_graphics;
pub mod layout;
pub mod macos_api;
pub mod rect;
pub mod window;
pub mod window_manager;

pub fn cf_array_as<T>(array: &CFArray) -> impl Iterator<Item = NonNull<T>> + use<'_, T> {
    let count = CFArray::count(array);
    (0..count).flat_map(move |idx| {
        NonNull::new(unsafe { CFArray::value_at_index(array, idx).cast_mut() })
            .map(|ptr| ptr.cast::<T>())
    })
}

pub fn cf_dictionary_value<T>(dict: &CFDictionary, key: &CFString) -> Option<NonNull<T>> {
    let ptr = unsafe { CFDictionary::value(dict, NonNull::from(key).as_ptr().cast()) };
    NonNull::new(ptr.cast_mut()).map(|ptr| ptr.cast::<T>())
}

#[derive(thiserror::Error, Debug)]
pub enum LibraryError {
    #[error(transparent)]
    Accessibility(#[from] AccessibilityError),
    #[error(transparent)]
    CoreGraphics(#[from] CoreGraphicsError),
}
