//! Keeps the church computer awake during a service, and says when a service is on.
//!
//! The rule (`in_service`): a service is on while streaming or recording is wanted, or while
//! a run sheet is set, the screens are not blacked out and an operator command changed what
//! is shown in the last 3 hours (so a run sheet left up all week never holds the computer awake).
//! Lives in the engine process: it holds that state, and a restarted engine asserts it again
//! within a second, so a crash only briefly drops the request.
//!
//! A future updater must check `Engine::in_service` and never install or restart anything
//! while it is true.

use std::time::Duration;

/// How long after the last slide, black or run sheet change the screens still count as a service.
pub const SHOWN_WINDOW: Duration = Duration::from_secs(3 * 60 * 60);

/// What the rule looks at.
#[derive(Clone, Copy)]
pub struct Activity {
    pub stream_wanted: bool,
    pub recording_wanted: bool,
    pub run_sheet_set: bool,
    pub black: bool,
    /// Time since an operator command last changed what is shown.
    pub since_last_change: Duration,
}

/// The rule, as a pure function of the engine's state.
pub fn in_service(a: &Activity) -> bool {
    a.stream_wanted || a.recording_wanted || (a.run_sheet_set && !a.black && a.since_last_change < SHOWN_WINDOW)
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
        if unsafe { SetThreadExecutionState(flags) } == 0 {
            crate::log("power", "SetThreadExecutionState failed; the computer may sleep");
        }
    }
}

// macOS and Linux: no-op for now (see docs/HARDENING.md).
#[cfg(not(windows))]
mod imp {
    pub fn keep_awake(_on: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_is_on_while_streaming_recording_or_showing_a_run_sheet() {
        let idle = Activity {
            stream_wanted: false,
            recording_wanted: false,
            run_sheet_set: false,
            black: false,
            since_last_change: Duration::ZERO,
        };
        assert!(!in_service(&idle), "idle, nothing set");
        assert!(in_service(&Activity { stream_wanted: true, ..idle }), "streaming");
        assert!(in_service(&Activity { recording_wanted: true, black: true, ..idle }), "recording, screens black");
        let shown = Activity { run_sheet_set: true, ..idle };
        assert!(in_service(&shown), "a run sheet on the screens");
        assert!(!in_service(&Activity { black: true, ..shown }), "run sheet blacked out");
        assert!(!in_service(&Activity { since_last_change: SHOWN_WINDOW, ..shown }), "3 h idle: sleep allowed");
        assert!(
            in_service(&Activity { stream_wanted: true, since_last_change: SHOWN_WINDOW, ..shown }),
            "streaming outlasts the window"
        );
    }
}
