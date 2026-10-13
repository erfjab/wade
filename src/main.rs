#![windows_subsystem = "windows"]

mod hook;
mod switcher;
mod tray;
mod util;
mod winlist;

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, TranslateMessage, MSG,
};

fn main() {
    unsafe {
        // One instance only: a second launch exits right away.
        let name = util::wide("Local\\WadeSwitcher");
        let _mutex = CreateMutexW(null(), 1, name.as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }

        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let overlay = switcher::create_window();
        if overlay.is_null() {
            return;
        }
        tray::create();
        // The keyboard hook lives on its own thread so typing never waits on drawing.
        hook::spawn(overlay);

        // Idle cost is zero: GetMessageW blocks until something happens.
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
