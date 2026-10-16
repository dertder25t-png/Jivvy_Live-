//! Keeps the church computer awake during a service, and says when a service is on.
//!
//! The rule (`in_service`): a service is on while streaming or recording is wanted, or while
//! a run sheet is set and the screens are not blacked out (the projector is showing it).
//! Lives in the engine process: it holds that state, and a restarted engine asserts it again
//! within a second, so a crash only briefly drops the request.
//!
//! A future updater must check `Engine::in_service` and never install or restart anything
//! while it is true.

/// The rule, as a pure function of the engine's state.
pub fn in_service(stream_wanted: bool, recording_wanted: bool, run_sheet_set: bool, black: bool) -> bool {
    stream_wanted || recording_wanted || (run_sheet_set && !black)
}

/// Holds the system and display awake for as long as `on` is true. Call from one dedicated
/// thread (Windows ties the request to the calling thread); repeating a value is harmless.
pub fn keep_awake(on: bool) {
    imp::keep_awake(on);
}

#[cfg(windows)]
mod imp {
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
    const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetThreadExecutionState(flags: u32) -> u32;
    }

    pub fn keep_awake(on: bool) {
        let flags = if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED } else { ES_CONTINUOUS };
        // SAFETY: plain Win32 call with flag values; no pointers.
        unsafe { SetThreadExecutionState(flags) };
    }
}

// macOS and Linux: no-op for now (see docs/HARDENING.md).
#[cfg(not(windows))]
mod imp {
    pub fn keep_awake(_on: bool) {}
}

#[cfg(test)]
mod tests {
    use super::in_service;

    #[test]
    fn a_service_is_on_while_streaming_recording_or_showing_a_run_sheet() {
        assert!(!in_service(false, false, false, false), "idle, nothing set");
        assert!(in_service(true, false, false, false), "streaming");
        assert!(in_service(false, true, false, true), "recording, even with the screens black");
        assert!(in_service(false, false, true, false), "a run sheet on the screens");
        assert!(!in_service(false, false, true, true), "run sheet blacked out");
    }
}
