//! Tray icon with "Start with Windows" and "Exit". Re-adds itself if Explorer restarts.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, REG_SZ,
    RRF_RT_ANY,
};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::util::{wide, WM_WADE_TRAY};

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
const CMD_EXIT: usize = 1;
const CMD_AUTOSTART: usize = 2;
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const WADE_KEY: &str = "Software\\Wade";

pub unsafe fn create() -> HWND {
    let class = wide("WadeTray");
    let inst = GetModuleHandleW(null());
    let mut wc: WNDCLASSEXW = zeroed();
    wc.cbSize = size_of::<WNDCLASSEXW>() as u32;
    wc.lpfnWndProc = Some(tray_proc);
    wc.hInstance = inst;
    wc.lpszClassName = class.as_ptr();
    RegisterClassExW(&wc);
    TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()), Ordering::Relaxed);

    let hwnd = CreateWindowExW(
        0, class.as_ptr(), class.as_ptr(), WS_POPUP, 0, 0, 0, 0,
        null_mut(), null_mut(), inst, null(),
    );
    add_icon(hwnd);
    // On by default. Re-written each launch so moving the exe just works.
    if !has_value(WADE_KEY, "NoAutostart") {
        autostart(true);
    }
    hwnd
}

unsafe fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid
}

unsafe fn add_icon(hwnd: HWND) {
    let mut nid = icon_data(hwnd);
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_WADE_TRAY;
    // The app icon embedded by build.rs (resource id 1), at the small-icon size.
    nid.hIcon = LoadImageW(
        GetModuleHandleW(null()), 1 as _, IMAGE_ICON,
        GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON), 0,
    ) as _;
    let tip = wide("Wade · Alt+Tab");
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    Shell_NotifyIconW(NIM_ADD, &nid);
}

unsafe extern "system" fn tray_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_WADE_TRAY {
        let event = (lparam & 0xFFFF) as u32;
        if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
            show_menu(hwnd);
        }
        return 0;
    }
    if msg == TASKBAR_CREATED.load(Ordering::Relaxed) && msg != 0 {
        add_icon(hwnd);
        return 0;
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    let on = has_value(RUN_KEY, "Wade");
    let check = if on { MF_CHECKED } else { MF_UNCHECKED };
    AppendMenuW(menu, MF_STRING | check, CMD_AUTOSTART, wide("Start with Windows").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, CMD_EXIT, wide("Exit").as_ptr());
    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd); // lets the menu close when clicking elsewhere
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, null());
    DestroyMenu(menu);
    match cmd as usize {
        CMD_AUTOSTART => autostart(!on),
        CMD_EXIT => {
            Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd));
            PostQuitMessage(0);
        }
        _ => {}
    }
}

unsafe fn has_value(key: &str, name: &str) -> bool {
    let (key, name) = (wide(key), wide(name));
    RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_ANY, null_mut(), null_mut(), null_mut()) == 0
}

/// Start with Windows on/off (HKCU\...\Run). "Off" is remembered in HKCU\Software\Wade.
unsafe fn autostart(on: bool) {
    let (run, wade) = (wide(RUN_KEY), wide(WADE_KEY));
    let (name, off) = (wide("Wade"), wide("NoAutostart"));
    if !on {
        RegDeleteKeyValueW(HKEY_CURRENT_USER, run.as_ptr(), name.as_ptr());
        let one = 1u32;
        RegSetKeyValueW(HKEY_CURRENT_USER, wade.as_ptr(), off.as_ptr(), REG_DWORD, &one as *const u32 as *const c_void, 4);
        return;
    }
    RegDeleteKeyValueW(HKEY_CURRENT_USER, wade.as_ptr(), off.as_ptr());
    let Ok(exe) = std::env::current_exe() else { return };
    let v = wide(&format!("\"{}\"", exe.display()));
    RegSetKeyValueW(HKEY_CURRENT_USER, run.as_ptr(), name.as_ptr(), REG_SZ, v.as_ptr() as *const c_void, (v.len() * 2) as u32);
}
