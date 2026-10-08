//! Whether the platform carries a touchpad scroll on after the fingers lift.
//!
//! On Windows gpui captures a precision touchpad with DirectManipulation,
//! which by default adds the system's inertia: the lifted-finger glide arrives
//! as more `Moved` deltas, and the gesture's `Ended` only once it has stopped.
//! An app that runs its own momentum from the deltas and the `Ended` that marks
//! the fingers lifting — as GTK does, whose DirectManipulation pan viewport
//! has no inertia (`gdkinput-dmanipulation.c`) — turns this off before its
//! windows open.
//!
//! Other platforms ignore it.

use std::sync::atomic::{AtomicBool, Ordering};

static PLATFORM_SCROLL_INERTIA: AtomicBool = AtomicBool::new(true);

/// Let (the default) or stop the platform carrying a touchpad scroll on after
/// the fingers lift. Takes effect for windows opened afterwards.
pub fn set_platform_scroll_inertia(enabled: bool) {
    PLATFORM_SCROLL_INERTIA.store(enabled, Ordering::Relaxed);
}

/// See [`set_platform_scroll_inertia`].
pub fn platform_scroll_inertia() -> bool {
    PLATFORM_SCROLL_INERTIA.load(Ordering::Relaxed)
}
