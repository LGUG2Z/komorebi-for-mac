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
