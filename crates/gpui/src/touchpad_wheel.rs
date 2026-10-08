//! Whether a touchpad scrolls as a mouse wheel.
//!
//! On Windows gpui captures a precision touchpad with DirectManipulation:
//! scrolls arrive as pixel deltas with begin/end phases and DirectManipulation's
//! own inertia. An app that wants the touchpad the way the system hands it to
//! every other Win32 program — `WM_MOUSEWHEEL` in fractions of a notch, the
//! system's inertia included, as GTK receives it — turns this on before its
//! windows open: the touchpad's contacts are then not taken, and its scrolls
//! arrive as `ScrollDelta::Lines`.
//!
//! Other platforms ignore it.

use std::sync::atomic::{AtomicBool, Ordering};

static TOUCHPAD_SCROLLS_AS_WHEEL: AtomicBool = AtomicBool::new(false);

/// Have the touchpad scroll as a mouse wheel (see the module docs).
pub fn set_touchpad_scrolls_as_wheel(enabled: bool) {
    TOUCHPAD_SCROLLS_AS_WHEEL.store(enabled, Ordering::Relaxed);
}

/// See [`set_touchpad_scrolls_as_wheel`].
pub fn touchpad_scrolls_as_wheel() -> bool {
    TOUCHPAD_SCROLLS_AS_WHEEL.load(Ordering::Relaxed)
}
