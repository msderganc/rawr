pub mod authoring;
pub mod engine;
pub mod listing;
pub mod others;
pub mod shell;
pub mod volumes;

/// Has the allocator give freed memory back to Windows at once rather than holding
/// it for 10 ms. A compress frees buffers fast enough that the wait kept 150-300 MB
/// the job was not using, and reusing the big buffers leaves little to pay for it.
pub fn release_memory_promptly() {
    // `mi_option_purge_delay` in the mimalloc this crate builds. The sys crate has
    // no constant for it yet.
    const PURGE_DELAY: libmimalloc_sys::mi_option_t = 15;
    // SAFETY: setting an option takes no pointers and any thread may do it.
    unsafe { libmimalloc_sys::mi_option_set(PURGE_DELAY, 0) }
}
