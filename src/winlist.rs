//! Finds the windows Alt+Tab would show, in most-recently-used (z-order) order.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::util::send_mask_key;

#[derive(Clone)]
pub struct WinInfo {
    pub hwnd: HWND,
    pub title: Vec<u16>,
    /// The window's own icon, often only 32 px; see `icon` for a sharp one.
    pub icon: HICON,
    /// The app's executable (NUL-terminated), empty when unknown or not useful.
    pub exe: Vec<u16>,
    pub minimized: bool,
    /// Visible frame of the window on screen (no invisible resize borders).
    pub frame: RECT,
}

pub unsafe fn enumerate(own: HWND) -> Vec<WinInfo> {
    let mut all: Vec<HWND> = Vec::new();
    EnumWindows(Some(collect), &mut all as *mut Vec<HWND> as LPARAM);
    all.into_iter()
        .filter(|&h| h != own && eligible(h))
        .map(|h| info(h))
        .collect()
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    (*(lparam as *mut Vec<HWND>)).push(hwnd);
    1
}

unsafe fn eligible(h: HWND) -> bool {
    if IsWindowVisible(h) == 0 || GetWindowTextLengthW(h) == 0 {
        return false;
    }
    let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
    let app_window = ex & WS_EX_APPWINDOW != 0;
    if !app_window
        && (ex & (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) != 0 || !GetWindow(h, GW_OWNER).is_null())
    {
        return false;
    }
    // Cloaked windows live on another virtual desktop or are suspended UWP shells.
    let mut cloaked: u32 = 0;
    if DwmGetWindowAttribute(h, DWMWA_CLOAKED as u32, &mut cloaked as *mut u32 as *mut c_void, 4) == 0
        && cloaked != 0
    {
        return false;
    }
    let mut class = [0u16; 64];
    let n = GetClassNameW(h, class.as_mut_ptr(), class.len() as i32) as usize;
    let class = String::from_utf16_lossy(&class[..n]);
    !matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

unsafe fn info(h: HWND) -> WinInfo {
    let len = GetWindowTextLengthW(h);
    let mut title = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(h, title.as_mut_ptr(), len + 1);
    title.truncate(n.max(0) as usize);

    let minimized = IsIconic(h) != 0;
    let mut window: RECT = zeroed();
    let mut frame: RECT = zeroed();
    if minimized {
        let mut wp: WINDOWPLACEMENT = zeroed();
        wp.length = size_of::<WINDOWPLACEMENT>() as u32;
        GetWindowPlacement(h, &mut wp);
        window = wp.rcNormalPosition;
        frame = window;
    } else {
        GetWindowRect(h, &mut window);
        let ok = DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            &mut frame as *mut RECT as *mut c_void,
            size_of::<RECT>() as u32,
        ) == 0;
        if !ok {
            frame = window;
        }
    }
    WinInfo { hwnd: h, title, icon: icon_for(h), exe: exe_path(h), minimized, frame }
}

unsafe fn exe_path(h: HWND) -> Vec<u16> {
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, &mut pid);
    let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
    if process.is_null() {
        return Vec::new();
    }
    let mut path = vec![0u16; 1024];
    let mut len = path.len() as u32 - 1;
    let ok = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len) != 0;
    CloseHandle(process);
    path.truncate(if ok { len as usize } else { 0 });
    // Store apps all run inside this host, whose own icon says nothing.
    if String::from_utf16_lossy(&path).to_lowercase().ends_with("\\applicationframehost.exe") {
        path.clear();
    }
    if !path.is_empty() {
        path.push(0);
    }
    path
}

thread_local! {
    // ponytail: never freed; bounded by apps seen × the few sizes drawn.
    static ICONS: RefCell<HashMap<(Vec<u16>, i32), HICON>> = RefCell::new(HashMap::new());
}

/// The app's icon at exactly `size` pixels, taken from its executable (whose
/// icon resources go up to 256 px) so it stays sharp at any size and DPI.
/// Falls back to the window's own icon.
pub unsafe fn icon(w: &WinInfo, size: i32) -> HICON {
    if w.exe.is_empty() {
        return w.icon;
    }
    let key = (w.exe.clone(), size);
    let icon = ICONS.with(|cache| {
        *cache.borrow_mut().entry(key).or_insert_with(|| {
            let mut icon: HICON = null_mut();
            let n = PrivateExtractIconsW(w.exe.as_ptr(), 0, size, size, &mut icon, null_mut(), 1, 0);
            if n == 1 { icon } else { null_mut() }
        })
    });
    if icon.is_null() { w.icon } else { icon }
}

/// Asks the window for its icon with a short timeout so a hung app can't stall us.
unsafe fn icon_for(h: HWND) -> HICON {
    for kind in [ICON_BIG, ICON_SMALL2, ICON_SMALL] {
        let mut result: usize = 0;
        let ok = SendMessageTimeoutW(
            h, WM_GETICON, kind as WPARAM, 0,
            SMTO_ABORTIFHUNG | SMTO_BLOCK, 30, &mut result,
        );
        if ok != 0 && result != 0 {
            return result as HICON;
        }
    }
    for index in [GCLP_HICON, GCLP_HICONSM] {
        let icon = GetClassLongPtrW(h, index);
        if icon != 0 {
            return icon as HICON;
        }
    }
    LoadIconW(null_mut(), IDI_APPLICATION)
}

/// Brings a window to the front, working around Windows' foreground lock.
pub unsafe fn activate(h: HWND) {
    if IsIconic(h) != 0 {
        ShowWindowAsync(h, SW_RESTORE);
    }
    if SetForegroundWindow(h) != 0 {
        return;
    }
    send_mask_key();
    if SetForegroundWindow(h) != 0 {
        return;
    }
    let fg_thread = GetWindowThreadProcessId(GetForegroundWindow(), null_mut());
    let me = GetCurrentThreadId();
    if fg_thread != 0 && fg_thread != me {
        AttachThreadInput(me, fg_thread, 1);
        BringWindowToTop(h);
        SetForegroundWindow(h);
        AttachThreadInput(me, fg_thread, 0);
    }
}
