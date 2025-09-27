// Only read operations are permitted, and their usage and potential failure must
// never disrupt the window manager
unsafe extern "C" {
    pub fn CGSMainConnectionID() -> i32;
    pub fn CGSGetActiveSpace(cid: i32) -> u64;
}
