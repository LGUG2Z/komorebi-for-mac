use crate::animation::animation_manager::AnimationManager;
use crate::core::animation::AnimationStyle;

use parking_lot::Mutex;
pub use prefix::AnimationPrefix;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;

pub use engine::AnimationEngine;
pub mod animation_manager;
pub mod engine;
pub mod lerp;
pub mod prefix;
pub mod render_dispatcher;
pub use render_dispatcher::RenderDispatcher;
pub mod style;

use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged)]
/// Animation configuration
///
/// This can be either global:
/// ```json
/// {
///     "enabled": true,
///     "style": "EaseInSine",
///     "fps": 60,
///     "duration": 250
/// }
/// ```
///
/// Or scoped by an animation kind prefix:
/// ```json
/// {
///     "movement": {
///         "enabled": true,
///         "style": "EaseInSine",
///         "fps": 60,
///         "duration": 250
///     }
/// }
/// ```
pub enum PerAnimationPrefixConfig<T> {
    /// Animation configuration prefixed for a specific animation kind
    Prefix(HashMap<AnimationPrefix, T>),
    /// Animation configuration for all animation kinds
    Global(T),
}

pub const DEFAULT_ANIMATION_ENABLED: bool = false;
pub const DEFAULT_ANIMATION_STYLE: AnimationStyle = AnimationStyle::Linear;
pub const DEFAULT_ANIMATION_DURATION: u64 = 250;
pub const DEFAULT_ANIMATION_FPS: u64 = 60;

pub static ANIMATION_MANAGER: LazyLock<Arc<Mutex<AnimationManager>>> =
    LazyLock::new(|| Arc::new(Mutex::new(AnimationManager::new())));

pub static ANIMATION_STYLE_GLOBAL: LazyLock<Arc<Mutex<AnimationStyle>>> =
    LazyLock::new(|| Arc::new(Mutex::new(DEFAULT_ANIMATION_STYLE)));

pub static ANIMATION_ENABLED_GLOBAL: LazyLock<Arc<AtomicBool>> =
    LazyLock::new(|| Arc::new(AtomicBool::new(DEFAULT_ANIMATION_ENABLED)));

pub static ANIMATION_DURATION_GLOBAL: LazyLock<Arc<AtomicU64>> =
    LazyLock::new(|| Arc::new(AtomicU64::new(DEFAULT_ANIMATION_DURATION)));

pub static ANIMATION_STYLE_PER_ANIMATION: LazyLock<
    Arc<Mutex<HashMap<AnimationPrefix, AnimationStyle>>>,
> = LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

pub static ANIMATION_ENABLED_PER_ANIMATION: LazyLock<Arc<Mutex<HashMap<AnimationPrefix, bool>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

pub static ANIMATION_DURATION_PER_ANIMATION: LazyLock<Arc<Mutex<HashMap<AnimationPrefix, u64>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

pub static ANIMATION_FPS: AtomicU64 = AtomicU64::new(DEFAULT_ANIMATION_FPS);
