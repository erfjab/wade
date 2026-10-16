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
use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL};
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetClassNameW, GetForegroundWindow, GetMessageW, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    EVENT_SYSTEM_FOREGROUND, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, LLKHF_INJECTED, MSG,
    WH_KEYBOARD_LL, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_KEYDOWN, WM_SYSKEYDOWN,
};

use crate::util::*;

static OVERLAY: AtomicIsize = AtomicIsize::new(0);

/// True from the first Alt+Tab until the switcher is dismissed.
pub static SESSION: AtomicBool = AtomicBool::new(false);

static HOOK: AtomicIsize = AtomicIsize::new(0);

pub fn spawn(overlay: HWND) {
    OVERLAY.store(overlay as isize, Ordering::Relaxed);
    std::thread::spawn(|| unsafe {
        // Windows silently drops a low-level hook that once answers too slowly
        // (a busy or waking machine), and Alt+Tab falls back to its own switcher.
        // Running first makes that rare; reinstalling when Windows' switcher
        // shows up brings Wade back from the next Alt+Tab on.
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        install();
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, null_mut(), Some(on_foreground),
            0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {}
        UnhookWindowsHookEx(HOOK.load(Ordering::Relaxed) as HHOOK);
    });
}

/// Installs the keyboard hook, replacing the one before it (gone or not).
unsafe fn install() {
    let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), GetModuleHandleW(null()), 0);
    let old = HOOK.swap(hook as isize, Ordering::Relaxed);
    if old != 0 {
        UnhookWindowsHookEx(old as HHOOK);
    }
}

unsafe extern "system" fn on_foreground(_: HWINEVENTHOOK, _: u32, hwnd: HWND, _: i32, _: i32, _: u32, _: u32) {
    if is_stock_switcher(hwnd) {
        install();
    }
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
        // Windows' own switcher is already up: Alt went down over a window run as
        // administrator, where this hook sees no keys, so Windows took that Alt+Tab.
        // Leave the rest of it to Windows instead of opening a second switcher on top.
        if !session && alt && is_stock_switcher(GetForegroundWindow()) {
            return false;
        }
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

/// True for Windows' own Alt+Tab window (Windows 11, then Windows 10).
unsafe fn is_stock_switcher(hwnd: HWND) -> bool {
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
    let class = String::from_utf16_lossy(&buf[..n]);
    class == "XamlExplorerHostIslandWindow" || class == "MultitaskingViewFrame"
}
