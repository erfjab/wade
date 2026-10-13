//! Low-level keyboard hook. It only reads key events and posts messages to the
//! overlay; all real work happens on the UI thread. Nothing is injected into
//! other processes.

use std::mem::zeroed;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_DELETE, VK_ESCAPE, VK_LEFT, VK_LMENU, VK_MENU, VK_RETURN, VK_RIGHT,
    VK_RMENU, VK_SHIFT, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL, WM_KEYDOWN,
    WM_SYSKEYDOWN,
};

use crate::util::*;

static OVERLAY: AtomicIsize = AtomicIsize::new(0);

/// True from the first Alt+Tab until the switcher is dismissed.
pub static SESSION: AtomicBool = AtomicBool::new(false);

pub fn spawn(overlay: HWND) {
    OVERLAY.store(overlay as isize, Ordering::Relaxed);
    std::thread::spawn(|| unsafe {
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), GetModuleHandleW(null()), 0);
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {}
        UnhookWindowsHookEx(hook);
    });
}

fn post(msg: u32, wparam: WPARAM) {
    unsafe {
        PostMessageW(OVERLAY.load(Ordering::Relaxed) as HWND, msg, wparam, 0);
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = &*(lparam as *const KBDLLHOOKSTRUCT);
        if k.flags & LLKHF_INJECTED == 0 && handle(wparam as u32, k) {
            return 1; // swallow the key
        }
    }
    CallNextHookEx(null_mut(), code, wparam, lparam)
}

/// Returns true when the key should not reach Windows or the focused app.
unsafe fn handle(msg: u32, k: &KBDLLHOOKSTRUCT) -> bool {
    let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let alt = k.flags & LLKHF_ALTDOWN != 0;
    let vk = k.vkCode as u16;
    let session = SESSION.load(Ordering::Relaxed);

    if vk == VK_TAB {
        if down && alt {
            let back = GetAsyncKeyState(VK_SHIFT as i32) < 0;
            if session {
                post(WM_WADE_NAV, if back { -1isize } else { 1 } as WPARAM);
            } else {
                SESSION.store(true, Ordering::Relaxed);
                send_mask_key();
                post(WM_WADE_OPEN, back as WPARAM);
            }
            return true;
        }
        return alt || session; // the matching Tab key-up
    }

    if vk == VK_MENU || vk == VK_LMENU || vk == VK_RMENU {
        if !down && session {
            SESSION.store(false, Ordering::Relaxed);
            post(WM_WADE_SELECT, 0);
        }
        return false; // Alt itself always passes through
    }

    if !session {
        return false;
    }
    if down {
        match vk {
            VK_ESCAPE => {
                SESSION.store(false, Ordering::Relaxed);
                post(WM_WADE_CANCEL, 0);
            }
            VK_RETURN => {
                SESSION.store(false, Ordering::Relaxed);
                post(WM_WADE_SELECT, 0);
            }
            VK_LEFT => post(WM_WADE_NAV, -1isize as WPARAM),
            VK_RIGHT => post(WM_WADE_NAV, 1),
            VK_UP | VK_DELETE => post(WM_WADE_CLOSE, 0),
            _ => return false,
        }
        return true;
    }
    matches!(vk, VK_RETURN | VK_LEFT | VK_RIGHT | VK_UP | VK_DELETE)
}
