//! Typing the device's buttons on this computer, for the links where the
//! device can't be the keyboard itself (USB and WiFi). Over Bluetooth the
//! device is a HID keyboard and this isn't used.
//!
//! Space is Claude Code's hold-to-talk key. A terminal sees a held key only
//! as auto-repeat, which the OS doesn't generate for injected keys, so a held
//! Space is repeated here like a keyboard would.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Before the first repeat, as a keyboard waits.
const REPEAT_DELAY: Duration = Duration::from_millis(400);
/// Between repeats (about 30 per second).
const REPEAT_EVERY: Duration = Duration::from_millis(33);

/// The held key's repeat thread, stopped by clearing the flag.
static HELD: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

/// Virtual-key codes for a device button, modifiers first: `space` and
/// `shift_tab`.
fn keys_for(name: &str) -> Option<&'static [u16]> {
    match name {
        "space" => Some(&[0x20]),           // VK_SPACE
        "shift_tab" => Some(&[0x10, 0x09]), // VK_SHIFT, VK_TAB
        _ => None,
    }
}

/// Press or release a device button's key on this computer.
pub fn send(name: &str, down: bool) {
    let Some(keys) = keys_for(name) else {
        log::warn!("unknown key {name:?} from the device");
        return;
    };
    if let Some(flag) = HELD.lock().ok().and_then(|mut h| h.take()) {
        flag.store(false, Ordering::Relaxed);
    }
    imp::send(keys, down);
    if down && name == "space" {
        let flag = Arc::new(AtomicBool::new(true));
        if let Ok(mut h) = HELD.lock() {
            *h = Some(flag.clone());
        }
        let spawned = std::thread::Builder::new().name("clawdmeter-key-repeat".into()).spawn(move || {
            std::thread::sleep(REPEAT_DELAY);
            while flag.load(Ordering::Relaxed) {
                imp::send(keys, true);
                std::thread::sleep(REPEAT_EVERY);
            }
        });
        if let Err(e) = spawned {
            log::warn!("key repeat: {e}");
        }
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    };

    /// Key down for each key in order, or key up in reverse order.
    pub fn send(keys: &[u16], down: bool) {
        let mut inputs: Vec<INPUT> = keys
            .iter()
            .map(|&vk| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        wScan: 0,
                        dwFlags: if down { 0 } else { KEYEVENTF_KEYUP },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            })
            .collect();
        if !down {
            inputs.reverse();
        }
        // SAFETY: `inputs` is a live slice of initialised INPUTs and the size
        // passed is the size of one INPUT, as SendInput requires.
        let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            log::warn!("SendInput sent {sent} of {} key events", inputs.len());
        }
    }
}

#[cfg(not(windows))]
mod imp {
    /// Only Windows is wired up; elsewhere the press is logged.
    pub fn send(keys: &[u16], down: bool) {
        log::info!("device key {keys:?} {} (not typed: Windows only)", if down { "down" } else { "up" });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_buttons_map_to_keys() {
        assert_eq!(keys_for("space"), Some(&[0x20u16][..]));
        assert_eq!(keys_for("shift_tab"), Some(&[0x10u16, 0x09][..]), "Shift goes down first");
        assert_eq!(keys_for("enter"), None);
    }
}
