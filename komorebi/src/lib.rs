#![warn(clippy::all)]

use crate::accessibility::error::AccessibilityError;
use crate::core_graphics::error::CoreGraphicsError;
use core::pathext::PathExt;
use lazy_static::lazy_static;
use objc2_application_services::AXObserver;
use objc2_application_services::AXUIElement;
use objc2_core_foundation::CFArray;
use objc2_core_foundation::CFDictionary;
use objc2_core_foundation::CFRetained;
use objc2_core_foundation::CFRunLoop;
use objc2_core_foundation::CFString;
use std::ops::Deref;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::AtomicI32;

#[macro_use]
pub mod ring;

pub mod accessibility;
pub mod application;
pub mod core;
pub mod core_graphics;
pub mod lockable_sequence;
pub mod macos_api;
mod monitor;
pub mod process_command;
pub mod window;
pub mod window_manager;
pub mod workspace;

lazy_static! {
    pub static ref HOME_DIR: PathBuf = {
        std::env::var("KOMOREBI_CONFIG_HOME").map_or_else(|_| dirs::home_dir().expect("there is no home directory"), |home_path| {
            let home = home_path.replace_env();

            assert!(
                home.is_dir(),
                "$Env:KOMOREBI_CONFIG_HOME is set to '{home_path}', which is not a valid directory"
            );


            home
        })
    };
    pub static ref DATA_DIR: PathBuf = dirs::data_local_dir()
        .expect("there is no local data directory")
        .join("komorebi");
}

pub static DEFAULT_WORKSPACE_PADDING: AtomicI32 = AtomicI32::new(5);
pub static DEFAULT_CONTAINER_PADDING: AtomicI32 = AtomicI32::new(5);

#[derive(Debug)]
pub struct CoreFoundationRunLoop(pub CFRetained<CFRunLoop>);
unsafe impl Sync for CoreFoundationRunLoop {}
unsafe impl Send for CoreFoundationRunLoop {}
impl Deref for CoreFoundationRunLoop {
    type Target = CFRunLoop;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

#[derive(Debug, Clone)]
pub struct AccessibilityUiElement(pub CFRetained<AXUIElement>);
unsafe impl Sync for AccessibilityUiElement {}
unsafe impl Send for AccessibilityUiElement {}
impl Deref for AccessibilityUiElement {
    type Target = AXUIElement;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

#[derive(Debug, Clone)]
pub struct AccessibilityObserver(pub CFRetained<AXObserver>);
unsafe impl Sync for AccessibilityObserver {}
unsafe impl Send for AccessibilityObserver {}
impl Deref for AccessibilityObserver {
    type Target = AXObserver;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

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
