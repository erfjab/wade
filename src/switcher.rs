//! The overlay: a row of cards with live previews drawn by DWM itself, so the
//! window contents cost us no copying. GDI only paints the background, the
//! labels and the icon strip. Timers run only while something is moving.

use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::Ordering;
use std::time::{Instant, SystemTime};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForSystem, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, ReleaseCapture, SetCapture, VK_MENU,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::hook::SESSION;
use crate::util::*;
use crate::winlist::{self, WinInfo};

// Layout, as fractions of the monitor. Matches the approved mockup.
const CARD_HEIGHT: f32 = 0.62; // the active card has this height...
const CARD_MAX_WIDTH: f32 = 0.75; // ...unless its width would pass this
const ROW_CENTER_Y: f32 = 0.45;
const GAP: f32 = 0.025;
const SIDE_SCALE: f32 = 0.68; // the others are this much smaller
const SIDE_ALPHA: f32 = 0.62;
// Seen a thousand times a day, so short, soft and with little travel.
const ANIM_MS: f32 = 160.0;
const OPEN_MS: f32 = 260.0;
const CLOSE_MS: f32 = 230.0;
const FADE_PART: f32 = 0.4; // share of an open/close spent fading the overlay
const ENTER_RISE: f32 = 0.015; // side cards rise this far (of monitor height) while fading in
const ENTER_SCALE: f32 = 0.97;
const CLOSE_DRAG: f32 = 0.12; // drag up this far (of monitor height) to close

const TIMER_ANIM: usize = 1;
const TIMER_WATCH: usize = 2;

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}
const PANEL: COLORREF = rgb(0x1d, 0x22, 0x2c);
const STRIP_ALPHA: u8 = 140; // of 255: dark glass behind the icon strip
const STRIP_ACTIVE_ALPHA: u8 = 40; // light veil over the active app's cell
const TEXT: COLORREF = rgb(0xff, 0xff, 0xff);
const TEXT_DIM: COLORREF = rgb(0x9a, 0xa3, 0xb2);
const DIM: u32 = 150; // of 256: the desktop background, darkened so the windows stand out

#[derive(Clone, Copy, Default)]
struct Pose {
    x: f32,
    y: f32,
    s: f32,
    a: f32,
}

impl Pose {
    fn lerp(a: Pose, b: Pose, t: f32) -> Pose {
        let l = |p: f32, q: f32| p + (q - p) * t;
        Pose { x: l(a.x, b.x), y: l(a.y, b.y), s: l(a.s, b.s), a: l(a.a, b.a) }
    }
}

struct Card {
    info: WinInfo,
    thumb: isize,
    bw: f32,
    bh: f32,
    cur: Pose,
    from: Pose,
    to: Pose,
    slot: i32, // position in the ring relative to the active card
}

impl Card {
    fn new(info: WinInfo, mw: f32, mh: f32) -> Card {
        let f = info.frame;
        let aspect = ((f.right - f.left).max(1) as f32 / (f.bottom - f.top).max(1) as f32).clamp(0.45, 2.6);
        let mut bh = mh * CARD_HEIGHT;
        let mut bw = bh * aspect;
        if bw > mw * CARD_MAX_WIDTH {
            bw = mw * CARD_MAX_WIDTH;
            bh = bw / aspect;
        }
        Card { info, thumb: 0, bw, bh, cur: Pose::default(), from: Pose::default(), to: Pose::default(), slot: 0 }
    }

    /// The pose that lays the card exactly over its real window on screen.
    fn frame_pose(&self, mon: &RECT) -> Pose {
        let f = self.info.frame;
        Pose {
            x: (f.left + f.right) as f32 / 2.0 - mon.left as f32,
            y: (f.top + f.bottom) as f32 / 2.0 - mon.top as f32,
            s: (f.right - f.left) as f32 / self.bw,
            a: 1.0,
        }
    }

    /// A second live preview of the same window, for the copy that slides off
    /// one end of the ring while the card itself enters at the other end.
    unsafe fn ghost(&self, overlay: HWND) -> Card {
        let mut g = Card { info: self.info.clone(), thumb: 0, ..*self };
        g.attach(overlay);
        g
    }

    fn rect(&self) -> RECT {
        let (w, h) = (self.bw * self.cur.s, self.bh * self.cur.s);
        RECT {
            left: (self.cur.x - w / 2.0).round() as i32,
            top: (self.cur.y - h / 2.0).round() as i32,
            right: (self.cur.x + w / 2.0).round() as i32,
            bottom: (self.cur.y + h / 2.0).round() as i32,
        }
    }

    /// Registers the live preview. DWM already sizes its source to the visible
    /// frame (no invisible borders), so it lines up with the real window uncropped.
    unsafe fn attach(&mut self, overlay: HWND) {
        if self.info.minimized || DwmRegisterThumbnail(overlay, self.info.hwnd, &mut self.thumb) != 0 {
            self.thumb = 0;
        }
    }

    unsafe fn detach(&mut self) {
        if self.thumb != 0 {
            DwmUnregisterThumbnail(self.thumb);
            self.thumb = 0;
        }
    }

    unsafe fn apply(&self, mw: f32) {
        if self.thumb == 0 {
            return;
        }
        let r = self.rect();
        let visible = r.right > 0 && (r.left as f32) < mw && self.cur.a > 0.01;
        let mut p: DWM_THUMBNAIL_PROPERTIES = zeroed();
        p.dwFlags = DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY;
        p.rcDestination = r;
        p.opacity = (self.cur.a.clamp(0.0, 1.0) * 255.0) as u8;
        p.fVisible = visible as BOOL;
        p.fSourceClientAreaOnly = 0;
        DwmUpdateThumbnailProperties(self.thumb, &p);
    }
}

/// Off-screen buffer for flicker-free GDI drawing; exists only while visible.
struct Gfx {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    font: HFONT,
}

/// The monitor's own desktop background, copied under every repaint.
/// Rendering it takes tens of milliseconds, so it is kept between openings
/// and rebuilt only when the monitor or the background changes.
struct Backdrop {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    mon: RECT,
    key: WallKey,
}

/// Wallpaper path, its file time, Windows' own copy's file time, desktop color.
type WallKey = (String, Option<SystemTime>, Option<SystemTime>, u32);

/// Identifies the current desktop background without drawing it, so a change
/// is caught on the next opening however it was made: Windows does not always
/// announce it.
// ponytail: a fit-mode change alone (Fill → Fit, same image) is not caught;
// add the WallpaperStyle registry value to the key if that matters.
unsafe fn wall_key() -> WallKey {
    let mut buf = [0u16; 260];
    SystemParametersInfoW(SPI_GETDESKWALLPAPER, buf.len() as u32, buf.as_mut_ptr().cast(), 0);
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    let path = String::from_utf16_lossy(&buf[..len]);
    let modified = |p: &str| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let copy = std::env::var("APPDATA").ok()
        .and_then(|a| modified(&format!("{a}\\Microsoft\\Windows\\Themes\\TranscodedWallpaper")));
    (path.clone(), modified(&path), copy, GetSysColor(COLOR_DESKTOP))
}

impl Drop for Backdrop {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp);
            DeleteDC(self.dc);
        }
    }
}

struct Switcher {
    hwnd: HWND,
    cards: Vec<Card>,
    ghosts: Vec<Card>, // closed cards still flying away
    active: usize,
    visible: bool,
    mon: RECT,
    mw: f32,
    mh: f32,
    scale: f32,
    t0: Instant,
    dur: f32,
    animating: bool,
    alpha: f32, // whole-overlay opacity, faded on open and close
    fade_from: f32,
    fade_to: f32,
    closing: bool,
    gfx: Option<Gfx>,
    drag: Option<(i32, i32)>,
    wheel: i32,
    backdrop: Option<Backdrop>,
}

thread_local! {
    static STATE: RefCell<Option<Switcher>> = const { RefCell::new(None) };
}

pub unsafe fn create_window() -> HWND {
    // Chromium and Firefox stop painting windows they think are covered, which
    // freezes their previews (a playing video stops moving). They make an
    // exception while the Windows Alt+Tab view, this class name, is showing.
    let class = wide("MultitaskingViewFrame");
    let inst = GetModuleHandleW(null());
    let mut wc: WNDCLASSEXW = zeroed();
    wc.cbSize = size_of::<WNDCLASSEXW>() as u32;
    wc.lpfnWndProc = Some(wndproc);
    wc.hInstance = inst;
    wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
    wc.lpszClassName = class.as_ptr();
    RegisterClassExW(&wc);

    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_LAYERED,
        class.as_ptr(), wide("Wade").as_ptr(), WS_POPUP,
        0, 0, 0, 0, null_mut(), null_mut(), inst, null(),
    );
    if !hwnd.is_null() {
        STATE.with(|s| {
            *s.borrow_mut() = Some(Switcher {
                hwnd,
                cards: Vec::new(),
                ghosts: Vec::new(),
                active: 0,
                visible: false,
                mon: zeroed(),
                mw: 0.0,
                mh: 0.0,
                scale: 1.0,
                t0: Instant::now(),
                dur: ANIM_MS,
                animating: false,
                alpha: 0.0,
                fade_from: 0.0,
                fade_to: 0.0,
                closing: false,
                gfx: None,
                drag: None,
                wheel: 0,
                backdrop: None,
            });
            if let Some(sw) = s.borrow_mut().as_mut() {
                sw.warm();
            }
        });
    }
    hwnd
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Win32 calls sent from inside a handler (ShowWindow, SetCapture...) re-enter
    // here; those fall through to the default handling instead of panicking.
    let handled = STATE.with(|s| match s.try_borrow_mut() {
        Ok(mut guard) => guard.as_mut().and_then(|sw| sw.handle(msg, wparam, lparam)),
        Err(_) => None,
    });
    handled.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wparam, lparam))
}

fn point(lparam: LPARAM) -> (i32, i32) {
    ((lparam & 0xFFFF) as i16 as i32, ((lparam >> 16) & 0xFFFF) as i16 as i32)
}

fn inside(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

impl Switcher {
    unsafe fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_WADE_OPEN => self.open(wparam != 0),
            WM_WADE_NAV => self.nav(wparam as isize as i32),
            WM_WADE_SELECT => self.select(),
            WM_WADE_CANCEL => self.dismiss(None),
            WM_WADE_CLOSE => self.close_active(),
            WM_TIMER if wparam == TIMER_ANIM => self.tick(),
            WM_TIMER if wparam == TIMER_WATCH => {
                // Safety net: the overlay never outlives a held Alt key.
                if GetAsyncKeyState(VK_MENU as i32) >= 0 {
                    self.select();
                }
            }
            // A new wallpaper (also each slideshow step), desktop color or screen layout.
            WM_SETTINGCHANGE if wparam == SPI_SETDESKWALLPAPER as usize => self.warm(),
            WM_SYSCOLORCHANGE | WM_DISPLAYCHANGE => self.warm(),
            WM_PAINT => self.paint(),
            WM_ERASEBKGND => return Some(1),
            WM_MOUSEACTIVATE => return Some(MA_NOACTIVATE as LRESULT),
            WM_LBUTTONDOWN => {
                let (x, y) = point(lparam);
                self.mouse_down(x, y);
            }
            WM_MOUSEMOVE => {
                let (_, y) = point(lparam);
                self.mouse_move(y);
            }
            WM_LBUTTONUP => {
                let (x, y) = point(lparam);
                self.mouse_up(x, y);
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let delta = ((wparam >> 16) & 0xFFFF) as i16 as i32;
                self.wheel += if msg == WM_MOUSEWHEEL { -delta } else { delta };
                while self.wheel >= 120 {
                    self.wheel -= 120;
                    self.nav(1);
                }
                while self.wheel <= -120 {
                    self.wheel += 120;
                    self.nav(-1);
                }
            }
            _ => return None,
        }
        Some(0)
    }

    unsafe fn open(&mut self, back: bool) {
        if self.closing {
            self.teardown(); // a new Alt+Tab cuts the close animation short
        }
        if !self.cards.is_empty() {
            self.nav(if back { -1 } else { 1 });
            return;
        }
        let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(monitor, &mut mi);
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        self.mon = mi.rcMonitor;
        self.mw = (self.mon.right - self.mon.left) as f32;
        self.mh = (self.mon.bottom - self.mon.top) as f32;
        self.scale = dpi_x as f32 / 96.0;

        let (mw, mh) = (self.mw, self.mh);
        self.cards = winlist::enumerate(self.hwnd).into_iter().map(|w| Card::new(w, mw, mh)).collect();
        if self.cards.is_empty() {
            SESSION.store(false, Ordering::Relaxed);
            return;
        }
        let n = self.cards.len();
        let fg = GetForegroundWindow();
        let current = self.cards.iter().position(|c| c.info.hwnd == fg).unwrap_or(0);

        // A quick Alt+Tab tap: Alt is already released, so switch to the
        // previous window without drawing anything.
        let mut pending: MSG = zeroed();
        if PeekMessageW(&mut pending, self.hwnd, WM_WADE_SELECT, WM_WADE_CANCEL, PM_NOREMOVE) != 0 {
            self.active = if back { (current + n - 1) % n } else { (current + 1) % n };
            return;
        }
        // Held: start on the window you are in; each further Tab moves on.
        self.active = current;
        self.show();
    }

    unsafe fn show(&mut self) {
        let hwnd = self.hwnd;
        for c in &mut self.cards {
            c.attach(hwnd);
        }
        let screen = GetDC(hwnd);
        let dc = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, self.mw as i32, self.mh as i32);
        ReleaseDC(hwnd, screen);
        let old = SelectObject(dc, bmp);
        let font = CreateFontW(
            -(15.0 * self.scale) as i32, 0, 0, 0, FW_SEMIBOLD as i32, 0, 0, 0,
            DEFAULT_CHARSET as u32, OUT_DEFAULT_PRECIS as u32, CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32, 0, wide("Segoe UI").as_ptr(),
        );
        self.gfx = Some(Gfx { dc, bmp, old, font });
        let (m, key) = (self.mon, wall_key());
        let same_mon = |b: &Backdrop| (b.mon.left, b.mon.top, b.mon.right, b.mon.bottom) == (m.left, m.top, m.right, m.bottom);
        if !self.backdrop.as_ref().is_some_and(|b| same_mon(b) && b.key == key) {
            self.backdrop = Some(self.backdrop(m));
        }

        // The focused window shrinks from where it really is into the row while
        // the rest rise in and the overlay fades up around them.
        self.layout();
        let (mon, mh, fg) = (self.mon, self.mh, GetForegroundWindow());
        for c in &mut self.cards {
            c.cur = if c.info.hwnd == fg && c.thumb != 0 {
                c.frame_pose(&mon)
            } else {
                Pose { y: c.to.y + mh * ENTER_RISE, s: c.to.s * ENTER_SCALE, a: 0.0, ..c.to }
            };
        }
        self.alpha = 0.0;
        self.fade_to = 1.0;
        SetLayeredWindowAttributes(hwnd, 0, 0, LWA_ALPHA);
        self.start_anim(OPEN_MS);
        self.apply_all();

        SetWindowPos(
            hwnd, HWND_TOPMOST, self.mon.left, self.mon.top, self.mw as i32, self.mh as i32,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        self.visible = true;
        InvalidateRect(hwnd, null(), 0);
        SetTimer(hwnd, TIMER_WATCH, 150, None);
    }

    unsafe fn hide(&mut self) {
        SESSION.store(false, Ordering::Relaxed);
        self.teardown();
    }

    unsafe fn teardown(&mut self) {
        self.closing = false;
        if self.visible {
            ShowWindow(self.hwnd, SW_HIDE);
            self.visible = false;
        }
        KillTimer(self.hwnd, TIMER_ANIM);
        KillTimer(self.hwnd, TIMER_WATCH);
        self.animating = false;
        if self.drag.take().is_some() {
            ReleaseCapture();
        }
        for c in self.cards.iter_mut().chain(self.ghosts.iter_mut()) {
            c.detach();
        }
        self.cards.clear();
        self.ghosts.clear();
        if let Some(g) = self.gfx.take() {
            SelectObject(g.dc, g.old);
            DeleteObject(g.bmp);
            DeleteObject(g.font);
            DeleteDC(g.dc);
        }
        self.wheel = 0;
    }

    unsafe fn select(&mut self) {
        if self.closing {
            return;
        }
        let Some(h) = self.cards.get(self.active).map(|c| c.info.hwnd) else {
            self.hide();
            return;
        };
        // Raise the real window first, under the overlay, so the zooming card
        // lands exactly on top of it as the overlay fades away.
        winlist::activate(h);
        self.dismiss(Some(self.active));
    }

    /// Ends the session: `target` zooms onto its real window, everything else
    /// shrinks and fades out with the overlay.
    unsafe fn dismiss(&mut self, target: Option<usize>) {
        if self.closing {
            return;
        }
        if !self.visible {
            self.hide();
            return;
        }
        SESSION.store(false, Ordering::Relaxed);
        KillTimer(self.hwnd, TIMER_WATCH);
        if self.drag.take().is_some() {
            ReleaseCapture();
        }
        self.closing = true;
        let mon = self.mon;
        for (i, c) in self.cards.iter_mut().enumerate() {
            c.to = if Some(i) == target { c.frame_pose(&mon) } else { Pose { s: c.cur.s * ENTER_SCALE, a: 0.0, ..c.cur } };
        }
        self.fade_to = 0.0;
        self.start_anim(CLOSE_MS);
    }

    unsafe fn nav(&mut self, step: i32) {
        if self.cards.is_empty() || self.closing {
            return;
        }
        let n = self.cards.len() as i32;
        self.go((self.active as i32 + step).rem_euclid(n) as usize);
    }

    unsafe fn go(&mut self, i: usize) {
        self.active = i;
        if self.visible {
            self.layout();
            self.start_anim(ANIM_MS);
        }
    }

    unsafe fn close_active(&mut self) {
        if self.cards.is_empty() || self.closing {
            return;
        }
        let mut card = self.cards.remove(self.active);
        PostMessageW(card.info.hwnd, WM_CLOSE, 0, 0);
        if !self.visible {
            card.detach();
            if self.cards.is_empty() {
                self.hide();
            }
            self.active = self.active.min(self.cards.len().saturating_sub(1));
            return;
        }
        card.to = Pose { y: card.cur.y - self.mh * 0.6, a: 0.0, ..card.cur };
        self.ghosts.push(card);
        self.active = self.active.min(self.cards.len().saturating_sub(1));
        self.layout();
        self.start_anim(ANIM_MS);
    }

    /// Computes target poses on an endless ring: the active card centered, the
    /// rest laid out edge to edge, half after it and half before it, each as
    /// wide as its window's shape. A card that wraps from one end to the other
    /// enters from beyond its new end while a ghost of it leaves the old one.
    unsafe fn layout(&mut self) {
        let n = self.cards.len();
        if n == 0 {
            return;
        }
        let a = self.active;
        let slot = |i: usize| ring_slot(i, a, n);
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| slot(i));
        let p = order.iter().position(|&i| i == a).unwrap_or(0);

        let gap = self.mw * GAP;
        let scale = |i: usize| if i == a { 1.0 } else { SIDE_SCALE };
        let half = |c: &Card, i: usize| c.bw * scale(i) / 2.0;
        let mut cx = vec![0f32; n];
        cx[a] = self.mw / 2.0;
        for k in p + 1..n {
            let (i, j) = (order[k], order[k - 1]);
            cx[i] = cx[j] + half(&self.cards[j], j) + gap + half(&self.cards[i], i);
        }
        for k in (0..p).rev() {
            let (i, j) = (order[k], order[k + 1]);
            cx[i] = cx[j] - half(&self.cards[j], j) - gap - half(&self.cards[i], i);
        }

        let y = self.mh * ROW_CENTER_Y;
        let shift = self.cards[a].slot; // how many places the ring turned
        let dx = self.mw / 2.0 - self.cards[a].cur.x; // how far that is on screen
        let (hwnd, visible) = (self.hwnd, self.visible);
        let mut ghosts = Vec::new();
        for (i, c) in self.cards.iter_mut().enumerate() {
            c.to = Pose { x: cx[i], y, s: scale(i), a: if i == a { 1.0 } else { SIDE_ALPHA } };
            let wrapped = slot(i) != c.slot - shift;
            c.slot = slot(i);
            if visible && wrapped {
                let mut g = c.ghost(hwnd);
                g.to = Pose { x: g.cur.x + dx, a: 0.0, ..g.cur };
                ghosts.push(g);
                c.cur = Pose { x: c.to.x - dx, a: 0.0, ..c.to };
            }
        }
        self.ghosts.extend(ghosts);
    }

    unsafe fn start_anim(&mut self, dur: f32) {
        for c in self.cards.iter_mut().chain(self.ghosts.iter_mut()) {
            c.from = c.cur;
        }
        self.fade_from = self.alpha;
        // Windows' "Animation effects" switch turns ours off too.
        let mut on: BOOL = 1;
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&mut on as *mut BOOL).cast(), 0);
        self.dur = if on != 0 { dur } else { 1.0 };
        self.t0 = Instant::now();
        if !self.animating {
            self.animating = true;
            SetTimer(self.hwnd, TIMER_ANIM, 8, None);
        }
    }

    unsafe fn tick(&mut self) {
        let t = (self.t0.elapsed().as_secs_f32() * 1000.0 / self.dur).min(1.0);
        // Opening and closing (the moves that fade the overlay) ease more evenly
        // so the zoom between the card and its real window can be seen. The
        // fade itself is kept out of the zoom's way: it finishes in the first
        // part when opening and only starts in the second part when closing,
        // otherwise the zoom just reads as a cross-fade with the window below.
        let fading = self.fade_from != self.fade_to;
        let e = 1.0 - (1.0 - t).powi(if fading { 2 } else { 3 });
        let full = !self.ghosts.is_empty() || self.closing || fading; // open/close moves cards outside the row band
        if fading && self.alpha != self.fade_to {
            // Fading in eases out so the first frame already shows; a linear ramp
            // from zero left the screen looking unchanged for ~100 ms.
            let f = if self.fade_to > self.fade_from { 1.0 - (1.0 - (t / FADE_PART).min(1.0)).powi(3) } else { ((t - 1.0) / FADE_PART + 1.0).max(0.0) };
            self.alpha = self.fade_from + (self.fade_to - self.fade_from) * f;
            SetLayeredWindowAttributes(self.hwnd, 0, (self.alpha.clamp(0.0, 1.0) * 255.0) as u8, LWA_ALPHA);
        }
        let dragging = self.drag.is_some();
        for (i, c) in self.cards.iter_mut().enumerate() {
            if !(dragging && i == self.active) {
                c.cur = Pose::lerp(c.from, c.to, e);
            }
        }
        for g in &mut self.ghosts {
            g.cur = Pose::lerp(g.from, g.to, e);
        }
        if t >= 1.0 {
            self.animating = false;
            KillTimer(self.hwnd, TIMER_ANIM);
            self.fade_from = self.fade_to; // settled: no fade in progress
            if self.closing {
                self.hide();
                return;
            }
            for g in &mut self.ghosts {
                g.detach();
            }
            self.ghosts.clear();
            if self.cards.is_empty() {
                self.hide();
                return;
            }
        }
        self.apply_all();
        self.invalidate(full);
    }

    unsafe fn apply_all(&self) {
        for c in self.cards.iter().chain(self.ghosts.iter()) {
            c.apply(self.mw);
        }
    }

    /// Repaints only the band the cards move in, plus the icon strip.
    unsafe fn invalidate(&self, full: bool) {
        if full {
            InvalidateRect(self.hwnd, null(), 0);
            return;
        }
        let half = self.mh * CARD_HEIGHT / 2.0;
        let y = self.mh * ROW_CENTER_Y;
        let band = RECT {
            left: 0,
            top: (y - half - 60.0 * self.scale) as i32,
            right: self.mw as i32,
            bottom: (y + half + 20.0 * self.scale) as i32,
        };
        InvalidateRect(self.hwnd, &band, 0);
        if let Some(panel) = self.strip_panel() {
            InvalidateRect(self.hwnd, &panel, 0);
        }
    }

    /// The faint box behind the icon strip that sets it apart from the background.
    fn strip_panel(&self) -> Option<RECT> {
        let strip = self.strip();
        let (first, last) = (strip.first()?, strip.last()?);
        // The same margin on every side, so the icons sit evenly inside the band.
        let pad = (4.0 * self.scale) as i32;
        Some(RECT { left: first.left - pad, top: first.top - pad, right: last.right + pad, bottom: last.bottom + pad })
    }

    fn strip_cell(&self) -> f32 {
        let n = self.cards.len().max(1) as f32;
        (44.0 * self.scale).min((self.mw * 0.9 - 6.0 * self.scale * (n - 1.0)) / n)
    }

    fn strip(&self) -> Vec<RECT> {
        let n = self.cards.len();
        if n == 0 {
            return Vec::new();
        }
        let gap = 6.0 * self.scale;
        let cell = self.strip_cell();
        let total = cell * n as f32 + gap * (n - 1) as f32;
        let x0 = (self.mw - total) / 2.0;
        let top = self.mh - 56.0 * self.scale - cell;
        (0..n)
            .map(|i| {
                let left = x0 + i as f32 * (cell + gap);
                RECT { left: left as i32, top: top as i32, right: (left + cell) as i32, bottom: (top + cell) as i32 }
            })
            .collect()
    }

    unsafe fn paint(&mut self) {
        let mut ps: PAINTSTRUCT = zeroed();
        let hdc = BeginPaint(self.hwnd, &mut ps);
        if let (Some(g), Some(b)) = (&self.gfx, &self.backdrop) {
            let dc = g.dc;
            let p = ps.rcPaint;
            BitBlt(dc, p.left, p.top, p.right - p.left, p.bottom - p.top, b.dc, p.left, p.top, SRCCOPY);
            SetBkMode(dc, TRANSPARENT as i32);
            SelectObject(dc, g.font);
            let s = self.scale;

            for (i, c) in self.cards.iter().enumerate() {
                let r = c.rect();
                if r.right < 0 || r.left as f32 > self.mw {
                    continue;
                }
                if c.thumb == 0 {
                    // Minimized windows have no live preview: show a panel with the app icon.
                    fill_round(dc, &r, PANEL, (10.0 * s) as i32);
                    let size = (64.0 * s * c.cur.s) as i32;
                    let cx = (r.left + r.right) / 2 - size / 2;
                    let cy = (r.top + r.bottom) / 2 - size / 2;
                    let icon = winlist::icon(&c.info, panel_icon(s));
                    DrawIconEx(dc, cx, cy, icon, size, size, 0, null_mut(), DI_NORMAL);
                }
                if self.closing {
                    continue; // only the previews themselves take part in the exit
                }
                self.label(dc, c, &r, i == self.active);
            }

            if let Some(panel) = self.strip_panel().filter(|_| !self.closing) {
                tint(dc, &panel, rgb(0, 0, 0), STRIP_ALPHA, (8.0 * s) as i32);
            }
            for (i, r) in self.strip().iter().enumerate().filter(|_| !self.closing) {
                if i == self.active {
                    tint(dc, r, rgb(0xff, 0xff, 0xff), STRIP_ACTIVE_ALPHA, (10.0 * s) as i32);
                }
                let size = strip_icon(self.strip_cell());
                let pad = (r.right - r.left - size) / 2;
                let icon = winlist::icon(&self.cards[i].info, size);
                DrawIconEx(dc, r.left + pad, r.top + pad, icon, size, size, 0, null_mut(), DI_NORMAL);
            }

            BitBlt(hdc, p.left, p.top, p.right - p.left, p.bottom - p.top, dc, p.left, p.top, SRCCOPY);
        }
        EndPaint(self.hwnd, &ps);
    }

    /// Builds the backdrop for the primary monitor ahead of time, so even the
    /// first Alt+Tab opens without a pause.
    unsafe fn warm(&mut self) {
        let primary = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut mi: MONITORINFO = zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(primary, &mut mi);
        self.backdrop = Some(self.backdrop(mi.rcMonitor));
        // Same for the icons of the apps already open.
        let s = GetDpiForSystem() as f32 / 96.0;
        for w in winlist::enumerate(self.hwnd) {
            winlist::icon(&w, label_icon(s));
            winlist::icon(&w, strip_icon(44.0 * s));
        }
    }

    /// The desktop background exactly as Windows draws it behind `mon`:
    /// whatever wallpaper, fit mode, slideshow image or plain color the
    /// system uses. Dimmed a little so the titles stay readable.
    unsafe fn backdrop(&self, mon: RECT) -> Backdrop {
        let (w, h) = (mon.right - mon.left, mon.bottom - mon.top);
        let mut bi: BITMAPINFO = zeroed();
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            ..zeroed()
        };
        let mut bits: *mut core::ffi::c_void = null_mut();
        let bmp = CreateDIBSection(null_mut(), &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        let dc = CreateCompatibleDC(null_mut());
        let old = SelectObject(dc, bmp);
        // PaintDesktop works in screen coordinates; shift them onto this monitor.
        SetWindowOrgEx(dc, mon.left, mon.top, null_mut());
        PaintDesktop(dc);
        SetWindowOrgEx(dc, 0, 0, null_mut());
        GdiFlush();
        if !bits.is_null() {
            for px in std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize) {
                let p = *px;
                let ch = |shift: u32| ((p >> shift & 0xFF) * DIM >> 8) << shift;
                *px = ch(0) | ch(8) | ch(16);
            }
        }
        Backdrop { dc, bmp, old, mon, key: wall_key() }
    }

    /// App icon and window title, centered above the card.
    unsafe fn label(&self, dc: HDC, c: &Card, r: &RECT, active: bool) {
        let s = self.scale;
        let icon = label_icon(s);
        let space = (8.0 * s) as i32;
        let max_text = ((r.right - r.left) as f32).max(240.0 * s) as i32 - icon - space;
        let mut size: SIZE = zeroed();
        GetTextExtentPoint32W(dc, c.info.title.as_ptr(), c.info.title.len() as i32, &mut size);
        let text_w = size.cx.min(max_text).max(0);
        let left = (r.left + r.right) / 2 - (icon + space + text_w) / 2;
        let top = r.top - (16.0 * s) as i32 - icon;
        DrawIconEx(dc, left, top, winlist::icon(&c.info, icon), icon, icon, 0, null_mut(), DI_NORMAL);
        SetTextColor(dc, if active { TEXT } else { TEXT_DIM });
        let mut tr = RECT { left: left + icon + space, top, right: left + icon + space + text_w, bottom: top + icon };
        DrawTextW(
            dc, c.info.title.as_ptr(), c.info.title.len() as i32, &mut tr,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
    }

    unsafe fn mouse_down(&mut self, x: i32, y: i32) {
        if self.closing {
            return;
        }
        if let Some(i) = self.cards.iter().position(|c| inside(&c.rect(), x, y)) {
            if i == self.active {
                self.drag = Some((x, y));
                SetCapture(self.hwnd);
            } else {
                self.go(i);
            }
        } else if let Some(i) = self.strip().iter().position(|r| inside(r, x, y)) {
            self.go(i);
        }
    }

    /// Dragging the active card upward, like swiping an app away on a phone.
    unsafe fn mouse_move(&mut self, y: i32) {
        let Some((_, y0)) = self.drag else { return };
        let Some(c) = self.cards.get_mut(self.active) else { return };
        c.cur.y = c.to.y + (y - y0).min(0) as f32;
        c.apply(self.mw);
        self.invalidate(true); // its title rides above the card, out of the row band
    }

    unsafe fn mouse_up(&mut self, x: i32, y: i32) {
        let Some((x0, y0)) = self.drag.take() else { return };
        ReleaseCapture();
        let (dx, dy) = (x - x0, y - y0);
        let click = 8.0 * self.scale;
        if (dy as f32) < -self.mh * CLOSE_DRAG {
            self.close_active();
        } else if (dx.abs() as f32) < click && (dy.abs() as f32) < click {
            self.select();
        } else {
            self.start_anim(ANIM_MS);
        }
    }
}

// Icon sizes in pixels. Icons are fetched at exactly these sizes so they are
// drawn 1:1, never stretched by GDI.
fn label_icon(scale: f32) -> i32 {
    (20.0 * scale).round() as i32
}
fn strip_icon(cell: f32) -> i32 {
    (cell * 0.6).round() as i32
}
fn panel_icon(scale: f32) -> i32 {
    (64.0 * scale).round() as i32
}

/// Where card `i` sits relative to the active card `a` on a ring of `n`:
/// 0 is the center, positive after it, negative before it.
fn ring_slot(i: usize, a: usize, n: usize) -> i32 {
    let n = n as i32;
    let d = (i as i32 - a as i32).rem_euclid(n);
    if d > n / 2 { d - n } else { d }
}

#[test]
fn ring_wraps_exactly_one_card_per_step() {
    for n in 2..8 {
        for a in 0..n {
            let next = (a + 1) % n;
            let wrapped = (0..n).filter(|&i| ring_slot(i, next, n) != ring_slot(i, a, n) - 1).count();
            assert_eq!(wrapped, 1, "n={n} a={a}");
            assert_eq!(ring_slot(a, a, n), 0);
        }
    }
}

/// Lays a translucent rounded rectangle over what is already drawn.
unsafe fn tint(dc: HDC, r: &RECT, color: COLORREF, alpha: u8, radius: i32) {
    let src = CreateCompatibleDC(dc);
    let px = CreateCompatibleBitmap(dc, 1, 1);
    let old = SelectObject(src, px);
    SetPixel(src, 0, 0, color);
    let clip = CreateRoundRectRgn(r.left, r.top, r.right + 1, r.bottom + 1, radius, radius);
    SelectClipRgn(dc, clip);
    let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: 0 };
    AlphaBlend(dc, r.left, r.top, r.right - r.left, r.bottom - r.top, src, 0, 0, 1, 1, blend);
    SelectClipRgn(dc, null_mut());
    DeleteObject(clip);
    SelectObject(src, old);
    DeleteObject(px);
    DeleteDC(src);
}

unsafe fn fill_round(dc: HDC, r: &RECT, color: COLORREF, radius: i32) {
    let brush = CreateSolidBrush(color);
    let old_brush = SelectObject(dc, brush);
    let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
    RoundRect(dc, r.left, r.top, r.right + 1, r.bottom + 1, radius, radius);
    SelectObject(dc, old_pen);
    SelectObject(dc, old_brush);
    DeleteObject(brush);
}
