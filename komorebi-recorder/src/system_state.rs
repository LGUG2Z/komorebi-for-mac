//! Whether the user's session is locked or switched away.
//!
//! Locking the screen and fast user switching change what ScreenCaptureKit can see and can kill
//! its streams. We cannot subscribe to those transitions without a run loop, but the session
//! dictionary is cheap to read, so the recorder polls it and logs each transition as an incident,
//! next to whatever the video and the logs did at the same moment.

use crate::incidents::Draft;
use crate::incidents::kind;
use objc2_core_foundation::CFBoolean;
use objc2_core_foundation::CFDictionary;
use objc2_core_foundation::CFString;
use objc2_core_graphics::CGSessionCopyCurrentDictionary;
use std::ptr::NonNull;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionState {
    pub locked: bool,
    /// `false` while another user's session is in front (fast user switching).
    pub on_console: bool,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            locked: false,
            on_console: true,
        }
    }
}

fn flag(dictionary: &CFDictionary, key: &'static str) -> Option<bool> {
    let key = CFString::from_static_str(key);
    // SAFETY: `key` is a valid CFString and the dictionary is live
    let value = unsafe { CFDictionary::value(dictionary, NonNull::from(&*key).as_ptr().cast()) };
    NonNull::new(value.cast_mut()).map(|value| {
        // SAFETY: the session dictionary stores these flags as CFBoolean
        unsafe { value.cast::<CFBoolean>().as_ref() }.as_bool()
    })
}

/// The current session state, or the default (unlocked, on console) if it cannot be read.
#[must_use]
pub fn read() -> SessionState {
    let Some(dictionary) = CGSessionCopyCurrentDictionary() else {
        return SessionState::default();
    };

    SessionState {
        locked: flag(&dictionary, "CGSSessionScreenIsLocked").unwrap_or(false),
        on_console: flag(&dictionary, "kCGSSessionOnConsoleKey").unwrap_or(true),
    }
}

/// Turns successive [`SessionState`]s into incidents.
#[derive(Debug, Default)]
pub struct SessionTracker {
    last: SessionState,
}

impl SessionTracker {
    pub fn observe(&mut self, now: SessionState) -> Vec<Draft> {
        let mut drafts = Vec::new();

        if now.locked != self.last.locked {
            drafts.push(if now.locked {
                Draft::new(
                    kind::SCREEN_LOCKED,
                    "the screen was locked; video may pause or restart until it is unlocked",
                )
            } else {
                Draft::new(kind::SCREEN_UNLOCKED, "the screen was unlocked")
            });
        }

        if now.on_console != self.last.on_console {
            drafts.push(if now.on_console {
                Draft::new(
                    kind::SESSION_ACTIVE,
                    "this user's session is in front again",
                )
            } else {
                Draft::new(
                    kind::SESSION_INACTIVE,
                    "another user's session is in front; this one is in the background",
                )
            });
        }

        self.last = now;
        drafts
    }

    /// Whether the session can currently be captured at all.
    #[must_use]
    pub fn capturable(&self) -> bool {
        !self.last.locked && self.last.on_console
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(locked: bool, on_console: bool) -> SessionState {
        SessionState { locked, on_console }
    }

    #[test]
    fn transitions_are_reported_once_each() {
        let mut tracker = SessionTracker::default();
        assert!(tracker.observe(state(false, true)).is_empty());

        let drafts = tracker.observe(state(true, true));
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].kind, kind::SCREEN_LOCKED);
        assert!(!tracker.capturable());

        assert!(
            tracker.observe(state(true, true)).is_empty(),
            "no repeat while it stays locked"
        );

        let drafts = tracker.observe(state(false, true));
        assert_eq!(drafts[0].kind, kind::SCREEN_UNLOCKED);
        assert!(tracker.capturable());
    }

    #[test]
    fn fast_user_switching_is_tracked_separately_from_locking() {
        let mut tracker = SessionTracker::default();
        let drafts = tracker.observe(state(false, false));
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].kind, kind::SESSION_INACTIVE);
        assert!(!tracker.capturable());

        // both change at once: both are reported
        let drafts = tracker.observe(state(true, true));
        let kinds: Vec<_> = drafts.iter().map(|draft| draft.kind).collect();
        assert_eq!(kinds, vec![kind::SCREEN_LOCKED, kind::SESSION_ACTIVE]);
    }

    /// Against the real session: it must be readable, whatever state it is in. Whether the screen
    /// happens to be locked while the tests run (a developer who stepped away, a CI machine under
    /// a screensaver) is not something to assert on.
    #[test]
    fn reading_the_real_session_works_in_any_state() {
        let state = read();

        let mut tracker = SessionTracker::default();
        let drafts = tracker.observe(state);

        // the tracker agrees with what was read, and reports at most one change per flag
        assert_eq!(tracker.capturable(), !state.locked && state.on_console);
        assert!(drafts.len() <= 2);
    }
}
