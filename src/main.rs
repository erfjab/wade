#![windows_subsystem = "windows"]

mod hook;
mod switcher;
mod tray;
mod util;
mod winlist;

use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, TranslateMessage, MSG,
};

static INSTANCE: AtomicIsize = AtomicIsize::new(0);

/// Lets go of the single-instance lock, so a copy started next can take over.
pub fn release_instance() {
    let h = INSTANCE.swap(0, Ordering::Relaxed) as HANDLE;
    if !h.is_null() {
        unsafe {
            ReleaseMutex(h);
            CloseHandle(h);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |a: &str| args.iter().any(|x| x == a);
    // The short elevated run that registers the "Run as administrator" logon task.
    if arg("--admin-task") {
        std::process::exit(tray::create_task());
    }
    unsafe {
        tray::load_settings();
        // Set to run as administrator but started normally (Start menu, installer):
        // hand over to the task, which starts Wade elevated without a UAC prompt.
        // Administrator is the default: the first start asks once (UAC) and sets up the task.
        if tray::admin_mode() && !tray::is_elevated() && (tray::run_task() || tray::admin_on()) {
            return;
        }

        // One instance only: a second launch exits right away. With --wait (the
        // task's start) it first waits for a copy that is about to exit.
        let name = util::wide("Local\\WadeSwitcher");
        let mutex = CreateMutexW(null(), 1, name.as_ptr());
        if mutex.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
            let waited = !mutex.is_null()
                && arg("--wait")
                && matches!(WaitForSingleObject(mutex, 10_000), WAIT_OBJECT_0 | WAIT_ABANDONED);
            if !waited {
                return;
            }
        }
        INSTANCE.store(mutex as isize, Ordering::Relaxed);

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
