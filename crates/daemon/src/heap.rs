//! glibc malloc settings for a daemon with many pane threads (S9, M9).
//!
//! glibc raises its mmap threshold each time a large block is freed, so
//! after a pane's first snapshots and checkpoints, later big buffers land on
//! that thread's arena, and an arena only gives back what's free at its top.
//! A daemon that once had busy panes kept about half its peak for good. With
//! a fixed threshold, big buffers are mmapped and go back when freed.
//!
//! Only glibc: musl's malloc (the static release builds) and macOS's already
//! unmap large blocks when they're freed.

/// Blocks this big or bigger are mmapped; free memory past this much at the
/// top of a heap is given back.
#[cfg(all(any(target_os = "linux", target_os = "android"), target_env = "gnu"))]
const THRESHOLD: i32 = 128 * 1024;

/// Fix the thresholds. Call before the first thread starts.
pub fn tune() {
    #[cfg(all(any(target_os = "linux", target_os = "android"), target_env = "gnu"))]
    // SAFETY: mallopt only sets allocator parameters.
    unsafe {
        nix::libc::mallopt(nix::libc::M_MMAP_THRESHOLD, THRESHOLD);
        nix::libc::mallopt(nix::libc::M_TRIM_THRESHOLD, THRESHOLD);
    }
}

/// Give free heap memory back to the system, from every arena (after a pane
/// closes: its thread's arena is mostly free then).
pub fn trim() {
    #[cfg(all(any(target_os = "linux", target_os = "android"), target_env = "gnu"))]
    // SAFETY: malloc_trim only releases free memory.
    unsafe {
        nix::libc::malloc_trim(0);
    }
}
