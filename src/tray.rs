//! Tray icon and its menu: animation speed, card size, background, corners, start with Windows,
//! run as administrator, exit. Settings live in HKCU\Software\Wade.
//! Re-adds the icon if Explorer restarts.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, REG_SZ,
    RRF_RT_ANY, RRF_RT_REG_DWORD,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::UI::Shell::{
    ShellExecuteExW, ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD,
    NIM_DELETE, NOTIFYICONDATAW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::util::{wide, WM_WADE_TRAY};

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
const CMD_EXIT: usize = 1;
const CMD_AUTOSTART: usize = 2;
const CMD_ADMIN: usize = 3;
const CMD_SPEED: usize = 10; // + index into SPEEDS
const CMD_SIZE: usize = 20; // + index into SIZES
const CMD_BG: usize = 30; // + index into BACKGROUNDS
const CMD_CORNERS: usize = 40; // + index into CORNERS
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const WADE_KEY: &str = "Software\\Wade";
const TASK: &str = "Wade";

pub const SPEEDS: [&str; 4] = ["Fast", "Normal", "Slow", "Off"];
pub const SIZES: [&str; 4] = ["Large", "Medium", "Small", "Icons only"];
pub const SIZE_ICONS: u32 = 3;
pub static SPEED: AtomicU32 = AtomicU32::new(0);
pub static SIZE: AtomicU32 = AtomicU32::new(0);
pub const BACKGROUNDS: [&str; 4] = ["Black", "75% darker", "50% darker", "25% darker"];
pub static BG: AtomicU32 = AtomicU32::new(2);

pub const CORNERS: [&str; 5] = ["Sharp", "Slightly round", "Round", "Rounder", "Very round"];
pub static CORNER: AtomicU32 = AtomicU32::new(0);

/// Corner radius of the middle card at 100% display scale, in pixels.
pub fn corner_radius() -> f32 {
    [0.0, 6.0, 12.0, 20.0, 28.0][CORNER.load(Ordering::Relaxed).min(4) as usize]
}

/// How much of the desktop background's brightness the switcher keeps, of 256.
pub fn backdrop_keep() -> u32 {
    [0, 64, 128, 192][BG.load(Ordering::Relaxed).min(3) as usize]
}

/// How long animations take relative to normal; None turns them off.
pub fn anim_factor() -> Option<f32> {
    [Some(0.6), Some(1.0), Some(1.6), None][SPEED.load(Ordering::Relaxed).min(3) as usize]
}

pub unsafe fn load_settings() {
    SPEED.store(get_dword("Speed").unwrap_or(0).min(3), Ordering::Relaxed);   // Fast by default
    SIZE.store(get_dword("Size").unwrap_or(0).min(3), Ordering::Relaxed);
    BG.store(get_dword("Background").unwrap_or(2).min(3), Ordering::Relaxed);
    CORNER.store(get_dword("Corners").unwrap_or(0).min(4), Ordering::Relaxed);
}

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
    // Run as administrator, Explorer's messages are filtered out unless let in.
    ChangeWindowMessageFilterEx(hwnd, WM_WADE_TRAY, MSGFLT_ALLOW, null_mut());
    ChangeWindowMessageFilterEx(hwnd, TASKBAR_CREATED.load(Ordering::Relaxed), MSGFLT_ALLOW, null_mut());
    add_icon(hwnd);
    // On by default. Re-written each launch so moving the exe just works.
    // The administrator task starts Wade itself, so the Run entry stays out of its way.
    if !has_value(WADE_KEY, "NoAutostart") && !admin_mode() {
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

unsafe fn submenu(items: &[&str], base: usize, current: u32) -> HMENU {
    let m = CreatePopupMenu();
    for (i, s) in items.iter().enumerate() {
        AppendMenuW(m, MF_STRING, base + i, wide(s).as_ptr());
    }
    CheckMenuRadioItem(m, base as u32, (base + items.len() - 1) as u32, base as u32 + current, MF_BYCOMMAND);
    m
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
    let admin = admin_mode();
    let speed = submenu(&SPEEDS, CMD_SPEED, SPEED.load(Ordering::Relaxed));
    let size = submenu(&SIZES, CMD_SIZE, SIZE.load(Ordering::Relaxed));
    AppendMenuW(menu, MF_POPUP, speed as usize, wide("Animation speed").as_ptr());
    AppendMenuW(menu, MF_POPUP, size as usize, wide("Card size").as_ptr());
    let bg = submenu(&BACKGROUNDS, CMD_BG, BG.load(Ordering::Relaxed));
    AppendMenuW(menu, MF_POPUP, bg as usize, wide("Background").as_ptr());
    let corners = submenu(&CORNERS, CMD_CORNERS, CORNER.load(Ordering::Relaxed));
    AppendMenuW(menu, MF_POPUP, corners as usize, wide("Corners").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    // With the administrator task, Wade always starts with Windows.
    let start = if admin { MF_CHECKED | MF_GRAYED } else { check(has_value(RUN_KEY, "Wade")) };
    AppendMenuW(menu, MF_STRING | start, CMD_AUTOSTART, wide("Start with Windows").as_ptr());
    AppendMenuW(menu, MF_STRING | check(admin), CMD_ADMIN, wide("Run as administrator").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, CMD_EXIT, wide("Exit").as_ptr());
    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd); // lets the menu close when clicking elsewhere
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, null()) as usize;
    DestroyMenu(menu); // also destroys the submenus
    match cmd {
        CMD_AUTOSTART => autostart(!has_value(RUN_KEY, "Wade")),
        CMD_ADMIN => {
            if if admin { admin_off() } else { admin_on() } {
                quit(hwnd);
            }
        }
        CMD_EXIT => quit(hwnd),
        c if (CMD_SPEED..CMD_SPEED + SPEEDS.len()).contains(&c) => {
            SPEED.store((c - CMD_SPEED) as u32, Ordering::Relaxed);
            set_dword("Speed", (c - CMD_SPEED) as u32);
        }
        c if (CMD_BG..CMD_BG + BACKGROUNDS.len()).contains(&c) => {
            BG.store((c - CMD_BG) as u32, Ordering::Relaxed);
            set_dword("Background", (c - CMD_BG) as u32);
        }
        c if (CMD_CORNERS..CMD_CORNERS + CORNERS.len()).contains(&c) => {
            CORNER.store((c - CMD_CORNERS) as u32, Ordering::Relaxed);
            set_dword("Corners", (c - CMD_CORNERS) as u32);
        }
        c if (CMD_SIZE..CMD_SIZE + SIZES.len()).contains(&c) => {
            SIZE.store((c - CMD_SIZE) as u32, Ordering::Relaxed);
            set_dword("Size", (c - CMD_SIZE) as u32);
        }
        _ => {}
    }
}

unsafe fn quit(hwnd: HWND) {
    Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd));
    PostQuitMessage(0);
}

// ───────── run as administrator ─────────
// A logon task with the highest rights starts Wade elevated, with no UAC prompt,
// so its keyboard hook also sees keys while a game or app run as administrator
// has focus. Creating the task needs one UAC prompt; `--admin-task` is that
// short elevated run of this same exe.

/// On by default; off once turned off in the menu or the first UAC prompt was declined.
pub fn admin_mode() -> bool {
    unsafe { get_dword("Admin") != Some(0) }
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut e: TOKEN_ELEVATION = zeroed();
        let mut len = 0u32;
        let ok = GetTokenInformation(token, TokenElevation, &mut e as *mut _ as *mut c_void, size_of::<TOKEN_ELEVATION>() as u32, &mut len);
        CloseHandle(token);
        ok != 0 && e.TokenIsElevated != 0
    }
}

pub fn schtasks(args: &[&str]) -> bool {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new("schtasks").args(args).creation_flags(CREATE_NO_WINDOW).status().is_ok_and(|s| s.success())
}

/// Starts the administrator task, which runs Wade elevated.
pub fn run_task() -> bool {
    schtasks(&["/Run", "/TN", TASK])
}

/// The elevated helper: registers the logon task, then starts it. Returns the exit code.
pub fn create_task() -> i32 {
    let Ok(exe) = std::env::current_exe() else { return 1 };
    let user = format!("{}\\{}", std::env::var("USERDOMAIN").unwrap_or_default(), std::env::var("USERNAME").unwrap_or_default());
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    // On battery too, no time limit, normal priority (the default 7 would slow the keyboard hook).
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Starts Wade as administrator when you sign in.</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>"{exe}"</Command><Arguments>--wait</Arguments></Exec></Actions>
</Task>"#,
        user = esc(&user),
        exe = esc(&exe.display().to_string()),
    );
    let path = std::env::temp_dir().join("wade-task.xml");
    let bytes: Vec<u8> = [0xFFFEu16].into_iter().chain(xml.encode_utf16()).flat_map(|u| u.to_le_bytes()).collect();
    if std::fs::write(&path, bytes).is_err() {
        return 1;
    }
    let ok = schtasks(&["/Create", "/TN", TASK, "/XML", &path.display().to_string(), "/F"]);
    let _ = std::fs::remove_file(&path);
    if !ok {
        return 1;
    }
    run_task(); // the new Wade waits (--wait) for this one to exit
    0
}

/// Asks for administrator rights once, registers the task and hands over to it.
pub unsafe fn admin_on() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let (verb, file, params) = (wide("runas"), wide(&exe.display().to_string()), wide("--admin-task"));
    let mut sei: SHELLEXECUTEINFOW = zeroed();
    sei.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    sei.fMask = SEE_MASK_NOCLOSEPROCESS;
    sei.lpVerb = verb.as_ptr();
    sei.lpFile = file.as_ptr();
    sei.lpParameters = params.as_ptr();
    if ShellExecuteExW(&mut sei) == 0 || sei.hProcess.is_null() {
        set_dword("Admin", 0); // UAC declined: don't ask again on every start
        return false;
    }
    WaitForSingleObject(sei.hProcess, INFINITE);
    let mut code = 1u32;
    GetExitCodeProcess(sei.hProcess, &mut code);
    CloseHandle(sei.hProcess);
    if code != 0 {
        return false;
    }
    set_dword("Admin", 1);
    let (run, name) = (wide(RUN_KEY), wide("Wade"));
    RegDeleteKeyValueW(HKEY_CURRENT_USER, run.as_ptr(), name.as_ptr());
    true
}

/// Back to a normal start: removes the task (this copy runs elevated, so it may)
/// and restarts Wade without administrator rights through Explorer.
unsafe fn admin_off() -> bool {
    let deleted = schtasks(&["/Delete", "/TN", TASK, "/F"]);
    if is_elevated() && !deleted {
        return false;
    }
    set_dword("Admin", 0);
    if !has_value(WADE_KEY, "NoAutostart") {
        autostart(true);
    }
    crate::release_instance();
    if let Ok(exe) = std::env::current_exe() {
        let (open, explorer, arg) = (wide("open"), wide("explorer.exe"), wide(&format!("\"{}\"", exe.display())));
        ShellExecuteW(null_mut(), open.as_ptr(), explorer.as_ptr(), arg.as_ptr(), null(), SW_SHOWNORMAL);
    }
    true
}

// ───────── registry ─────────

unsafe fn has_value(key: &str, name: &str) -> bool {
    let (key, name) = (wide(key), wide(name));
    RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_ANY, null_mut(), null_mut(), null_mut()) == 0
}

unsafe fn get_dword(name: &str) -> Option<u32> {
    let (key, name) = (wide(WADE_KEY), wide(name));
    let (mut v, mut len) = (0u32, 4u32);
    let r = RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, null_mut(), &mut v as *mut u32 as *mut c_void, &mut len);
    (r == 0).then_some(v)
}

unsafe fn set_dword(name: &str, v: u32) {
    let (key, name) = (wide(WADE_KEY), wide(name));
    RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), REG_DWORD, &v as *const u32 as *const c_void, 4);
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
