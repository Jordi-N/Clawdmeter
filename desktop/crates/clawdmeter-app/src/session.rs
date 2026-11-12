//! Whether this computer's session is locked, for resting the device while
//! nobody is at the desk (`rest_on_lock`).
//!
//! Windows: the input desktop can't be opened while the lock screen (or a
//! UAC prompt, briefly) owns it. The daemon debounces it, so a UAC prompt
//! doesn't rest the device. Elsewhere: unknown, so nothing happens.

/// `Some(true)` locked, `Some(false)` unlocked, `None` can't tell here.
pub fn locked() -> Option<bool> {
    imp::locked()
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::System::StationsAndDesktops::{CloseDesktop, OpenInputDesktop};

    /// DESKTOP_SWITCHDESKTOP: the access the lock screen denies.
    const SWITCH_DESKTOP: u32 = 0x0100;

    pub fn locked() -> Option<bool> {
        // SAFETY: plain Win32 calls; a desktop handle that opens is closed.
        unsafe {
            let desk = OpenInputDesktop(0, 0, SWITCH_DESKTOP);
            if desk.is_null() {
                return Some(true);
            }
            CloseDesktop(desk);
        }
        Some(false)
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn locked() -> Option<bool> {
        None
    }
}
