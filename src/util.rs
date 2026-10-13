use std::mem::{size_of, zeroed};

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::WM_APP;

// Messages the hook thread and the tray post to the overlay window.
pub const WM_WADE_OPEN: u32 = WM_APP + 1; // wparam: 1 when Shift is held
pub const WM_WADE_NAV: u32 = WM_APP + 2; // wparam: +1 or -1 (as isize)
pub const WM_WADE_SELECT: u32 = WM_APP + 3;
pub const WM_WADE_CANCEL: u32 = WM_APP + 4;
pub const WM_WADE_CLOSE: u32 = WM_APP + 5;
pub const WM_WADE_TRAY: u32 = WM_APP + 6;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Taps the unassigned virtual key 0xE8. This keeps a lone Alt release from
/// opening the focused app's menu bar, and counts as our own input so Windows
/// lets us bring another window to the foreground.
pub unsafe fn send_mask_key() {
    let mut inputs: [INPUT; 2] = zeroed();
    for (i, input) in inputs.iter_mut().enumerate() {
        input.r#type = INPUT_KEYBOARD;
        input.Anonymous.ki = KEYBDINPUT {
            wVk: 0xE8,
            wScan: 0,
            dwFlags: if i == 0 { 0 } else { KEYEVENTF_KEYUP },
            time: 0,
            dwExtraInfo: 0,
        };
    }
    SendInput(2, inputs.as_ptr(), size_of::<INPUT>() as i32);
}
