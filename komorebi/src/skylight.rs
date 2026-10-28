// Private SkyLight/CoreGraphics APIs
//
// Read operations (CGS*) are used for querying window server state.
// Write operations (SLS*) are used for animation screen update batching.
unsafe extern "C" {
    // Read operations - these APIs are undocumented but stable - their usage should not disrupt the window manager.
    pub fn CGSMainConnectionID() -> i32;
    pub fn CGSGetActiveSpace(cid: i32) -> u64;

    // Write operations for animation - these APIs are undocumented and I don't know how stable they are
    // SLSDisableUpdate freezes screen compositing - all window changes are batched
    pub fn SLSDisableUpdate(cid: i32) -> i32;
    // SLSReenableUpdate resumes compositing - all batched changes appear at once
    pub fn SLSReenableUpdate(cid: i32) -> i32;
}

#[cfg(test)]
thread_local! {
    /// How many [`BatchedUpdates`] there are on this thread, since there is no window server to
    /// ask under test
    static OPEN: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}

/// Everything that changes on screen is held back until this is dropped, and then appears at once.
///
/// Moving a lot of windows one after another, which is what putting one workspace away and another
/// up is, is otherwise seen as it happens: the windows of both workspaces at the same time, with
/// the ones that take the longest to answer the last to go. Dropping it, which is also what an
/// early return does, is what lets the screen update again.
///
/// Keep it for what has to appear together and for no longer: nothing on screen changes while it
/// is held.
#[must_use = "updates are only held back for as long as this is kept"]
pub struct BatchedUpdates {
    connection_id: i32,
}

impl BatchedUpdates {
    pub fn begin() -> Self {
        // there is no window server to ask when testing
        let connection_id = if cfg!(test) {
            0
        } else {
            unsafe {
                let connection_id = CGSMainConnectionID();
                SLSDisableUpdate(connection_id);
                connection_id
            }
        };

        #[cfg(test)]
        OPEN.with(|open| open.set(open.get() + 1));

        Self { connection_id }
    }
}

impl Drop for BatchedUpdates {
    fn drop(&mut self) {
        if !cfg!(test) {
            unsafe { SLSReenableUpdate(self.connection_id) };
        }

        #[cfg(test)]
        OPEN.with(|open| open.set(open.get() - 1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> i32 {
        OPEN.with(std::cell::Cell::get)
    }

    #[test]
    fn updates_are_held_back_for_as_long_as_it_is_kept() {
        assert_eq!(open(), 0);

        let batched = BatchedUpdates::begin();
        assert_eq!(open(), 1);

        drop(batched);
        assert_eq!(open(), 0);
    }

    #[test]
    fn an_early_return_lets_the_screen_update_again() {
        fn fails() -> Result<(), ()> {
            let _batched = BatchedUpdates::begin();
            assert_eq!(open(), 1);

            Err(())?;

            Ok(())
        }

        assert!(fails().is_err());
        assert_eq!(open(), 0);
    }

    #[test]
    fn they_may_be_nested() {
        let outer = BatchedUpdates::begin();
        {
            let _inner = BatchedUpdates::begin();
            assert_eq!(open(), 2);
        }
        assert_eq!(open(), 1);

        drop(outer);
        assert_eq!(open(), 0);
    }
}
