#![warn(clippy::all)]

use crate::accessibility::error::AccessibilityError;
use crate::core::ApplicationIdentifier;
use crate::core::config_generation::IdWithIdentifier;
use crate::core::config_generation::MatchingRule;
use crate::core::config_generation::MatchingStrategy;
use crate::core::config_generation::WorkspaceMatchingRule;
use crate::core_graphics::error::CoreGraphicsError;
use crate::window::AspectRatio;
use crate::window::PredefinedAspectRatio;
use core::pathext::PathExt;
use lazy_static::lazy_static;
use objc2_application_services::AXObserver;
use objc2_application_services::AXUIElement;
use objc2_core_foundation::CFArray;
use objc2_core_foundation::CFDictionary;
use objc2_core_foundation::CFRetained;
use objc2_core_foundation::CFRunLoop;
use objc2_core_foundation::CFString;
use objc2_core_foundation::CGPoint;
use objc2_core_foundation::CGRect;
use objc2_core_foundation::CGSize;
use parking_lot::Mutex;
use regex::Regex;
use std::collections::HashMap;
use std::ops::Deref;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI32;

#[macro_use]
pub mod ring;

pub mod accessibility;
pub mod app_kit_notification_constants;
pub mod application;
pub mod ax_event_listener;
pub mod container;
pub mod core;
pub mod core_graphics;
pub mod display_reconfiguration_listener;
pub mod input_event_listener;
pub mod lockable_sequence;
pub mod macos_api;
pub mod monitor;
pub mod monitor_reconciliator;
pub mod notification_center_listener;
pub mod process_command;
pub mod process_event;
pub mod reaper;
pub mod static_config;
pub mod window;
pub mod window_manager;
pub mod window_manager_event;
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
    static ref FLOATING_WINDOW_TOGGLE_ASPECT_RATIO: Arc<Mutex<AspectRatio>> = Arc::new(Mutex::new(
        AspectRatio::Predefined(PredefinedAspectRatio::Widescreen)
    ));
    static ref WINDOW_RESTORE_POSITIONS: Arc<Mutex<HashMap<u32, CGRect>>> =
        Arc::new(Mutex::new(HashMap::new()));
    pub static ref UPDATE_MONITOR_WORK_AREAS: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    static ref WORKSPACE_MATCHING_RULES: Arc<Mutex<Vec<WorkspaceMatchingRule>>> =
        Arc::new(Mutex::new(Vec::new()));
    static ref REGEX_IDENTIFIERS: Arc<Mutex<HashMap<String, Regex>>> =
        Arc::new(Mutex::new(HashMap::new()));
    static ref MANAGE_IDENTIFIERS: Arc<Mutex<Vec<MatchingRule>>> = Arc::new(Mutex::new(vec![]));
    static ref IGNORE_IDENTIFIERS: Arc<Mutex<Vec<MatchingRule>>> = Arc::new(Mutex::new(vec![
        MatchingRule::Simple(IdWithIdentifier {
            kind: ApplicationIdentifier::Class,
            id: String::from("OPContainerClass"),
            matching_strategy: Option::from(MatchingStrategy::Equals),
        }),
        MatchingRule::Simple(IdWithIdentifier {
            kind: ApplicationIdentifier::Class,
            id: String::from("IHWindowClass"),
            matching_strategy: Option::from(MatchingStrategy::Equals),
        }),
        MatchingRule::Simple(IdWithIdentifier {
            kind: ApplicationIdentifier::Exe,
            id: String::from("komorebi-bar.exe"),
            matching_strategy: Option::from(MatchingStrategy::Equals),
        })
    ]));
    static ref SESSION_FLOATING_APPLICATIONS: Arc<Mutex<Vec<MatchingRule>>> =
        Arc::new(Mutex::new(Vec::new()));
    static ref FLOATING_APPLICATIONS: Arc<Mutex<Vec<MatchingRule>>> =
        Arc::new(Mutex::new(vec![MatchingRule::Simple(IdWithIdentifier {
            kind: ApplicationIdentifier::Exe,
            id: String::from("komorebi-shortcuts.exe"),
            matching_strategy: Option::from(MatchingStrategy::Equals),
        })]));
    static ref PERMAIGNORE_CLASSES: Arc<Mutex<Vec<String>>> =
        Arc::new(Mutex::new(vec!["Chrome_RenderWidgetHostHWND".to_string(),]));
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

pub fn hidden_frame_bottom_left(screen_frame: CGRect, window_size: CGSize) -> CGRect {
    let visible_sliver: f64 = 1.0;
    let origin_x = screen_frame.origin.x - (window_size.width - visible_sliver);
    let origin_y = screen_frame.origin.y - (window_size.height - visible_sliver);

    CGRect::new(CGPoint::new(origin_x, origin_y), window_size)
}
