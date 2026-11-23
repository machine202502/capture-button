//! Small runtime facts shared by every layer.

/// Whether this process is a test run or was started with `--test-mode`.
///
/// Test mode keeps settings in a separate registry key / data directory and
/// skips the update check unless a feed is given.
pub fn test_mode() -> bool {
    cfg!(test) || std::env::args().any(|arg| arg == "--test-mode")
}
