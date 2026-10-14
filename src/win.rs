//! Everything Windows-specific: clipboard listener, global hotkey, writing the
//! clipboard back, simulated Ctrl+V, shell thumbnails, show/hide.
//!
//! One background thread owns a hidden message-only window. Windows *pushes*
//! WM_CLIPBOARDUPDATE / WM_HOTKEY to it, so the app uses no CPU while idle.

use crate::store::{Kind, NewClip, hash};
use futures::channel::mpsc::UnboundedSender;
use std::{ffi::c_void, path::{Path, PathBuf}, sync::{OnceLock, atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicUsize, Ordering::Relaxed}}, thread, time::Duration};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{Com::*, DataExchange::*, LibraryLoader::GetModuleHandleW, Memory::*, Ole::CF_HDROP, Registry::*, Threading::*, Time::*},
        UI::{Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
    },
    core::{PCWSTR, PWSTR, w},
};

pub enum Event {
    Clip(NewClip),
    Toggle,
    /// Always show and focus the window (tray Open / left-click).
    Open,
    Quit,
}

const CF_UNICODETEXT: u32 = 13;
const CF_DIB: u32 = 8;

static TX: OnceLock<(UnboundedSender<Event>, PathBuf)> = OnceLock::new();
static LISTENER: AtomicIsize = AtomicIsize::new(0);
/// Full-screen dimmer behind ClipBox; also catches outside clicks to dismiss.
static DIM: AtomicIsize = AtomicIsize::new(0);
/// The visible ClipBox window hwnd (for dim-click dismiss).
static APP_HWND: AtomicIsize = AtomicIsize::new(0);
/// Its opacity, 0–255 (0 = off).
pub static DIM_ALPHA: AtomicU32 = AtomicU32::new(90);
static HOTKEY: AtomicU32 = AtomicU32::new(0);
/// Skip clips that password managers mark as private.
pub static SKIP_PRIVATE: AtomicBool = AtomicBool::new(true);
const WM_SET_HOTKEY: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const WM_MENU_CLOSE: u32 = WM_APP + 3;
const TRAY_ID: u32 = 1;
const ID_OPEN: usize = 1001;
const ID_EXIT: usize = 1002;

/// Colors for the custom tray menu (0xRRGGBB), synced from the app theme + accent.
static LOOK_BG: AtomicU32 = AtomicU32::new(0x16161a);
static LOOK_FG: AtomicU32 = AtomicU32::new(0xe8e8ec);
static LOOK_DIM: AtomicU32 = AtomicU32::new(0x9a9aa6);
static LOOK_LINE: AtomicU32 = AtomicU32::new(0x2c2c34);
static LOOK_ACCENT: AtomicU32 = AtomicU32::new(0x3a86ff);
static LOOK_HOVER: AtomicU32 = AtomicU32::new(0x1d293f);
static LOOK_ATEXT: AtomicU32 = AtomicU32::new(0x3a86ff);
static MENU_HOVER: AtomicIsize = AtomicIsize::new(0);
static MENU_RESULT: AtomicUsize = AtomicUsize::new(0);
static MENU_DONE: AtomicBool = AtomicBool::new(false);
static MENU_HWND: AtomicIsize = AtomicIsize::new(0);

/// Hotkeys offered in settings: (label, modifiers, virtual key).
pub const HOTKEYS: [(&str, u32, u32); 5] = [
    ("alt + v", 0x1, 0x56),
    ("ctrl + shift + v", 0x2 | 0x4, 0x56),
    ("ctrl + alt + v", 0x1 | 0x2, 0x56),
    ("alt + c", 0x1, 0x43),
    ("ctrl + `", 0x2, 0xC0),
];

/// Only one ClipBox at a time: a second launch exits immediately.
pub fn single_instance() {
    unsafe {
        let _ = CreateMutexW(None, true, w!("ClipBox.single-instance"));
        if GetLastError() == ERROR_ALREADY_EXISTS { std::process::exit(0); }
    }
}

/// Change the global hotkey (index into HOTKEYS).
pub fn set_hotkey(ix: usize) {
    HOTKEY.store(ix as u32, Relaxed);
    let h = LISTENER.load(Relaxed);
    if h != 0 {
        unsafe { PostMessageW(Some(HWND(h as *mut c_void)), WM_SET_HOTKEY, WPARAM(0), LPARAM(0)).ok(); }
    }
}

unsafe fn register_hotkey(hwnd: HWND) {
    let (_, m, vk) = HOTKEYS[(HOTKEY.load(Relaxed) as usize).min(HOTKEYS.len() - 1)];
    unsafe {
        UnregisterHotKey(Some(hwnd), 1).ok();
        RegisterHotKey(Some(hwnd), 1, HOT_KEY_MODIFIERS(m) | MOD_NOREPEAT, vk).ok();
    }
}

fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() }

/// Start the listener thread. `dir` is where image/thumbnail pngs go.
pub fn spawn(tx: UnboundedSender<Event>, dir: PathBuf) {
    TX.set((tx, dir)).ok();
    thread::spawn(|| unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().ok();
        let inst = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
        let class = w!("clipbox_listener");
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: class, ..Default::default() });
        let Ok(hwnd) = CreateWindowExW(Default::default(), class, w!(""), Default::default(), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(inst.into()), None) else {
            return;
        };
        AddClipboardFormatListener(hwnd).ok();
        // Alt+V opens ClipBox (Win+V belongs to Windows).
        LISTENER.store(hwnd.0 as isize, Relaxed);

        let dim_class = w!("clipbox_dim");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(dim_proc),
            hInstance: inst.into(),
            lpszClassName: dim_class,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        });
        // Not hit-transparent: clicks on the dim dismiss ClipBox (like clicking outside).
        if let Ok(dim) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            dim_class, w!(""), WS_POPUP, 0, 0, 0, 0, None, None, Some(inst.into()), None,
        ) {
            DIM.store(dim.0 as isize, Relaxed);
        }
        register_hotkey(hwnd);
        add_tray(hwnd, inst.into());
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
        remove_tray(hwnd);
    });
}

unsafe extern "system" fn dim_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            // Click outside the ClipBox window (on the dim) → hide to tray.
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let app = APP_HWND.load(Relaxed);
                if app != 0 {
                    hide(app);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe fn tray_data(hwnd: HWND, icon: HICON) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW::default();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_ID;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = icon;
    let tip: Vec<u16> = "ClipBox".encode_utf16().chain(Some(0)).collect();
    nid.szTip[..tip.len().min(128)].copy_from_slice(&tip[..tip.len().min(128)]);
    nid
}

unsafe fn add_tray(hwnd: HWND, inst: HINSTANCE) {
    unsafe {
        let icon = LoadImageW(
            Some(inst),
            PCWSTR(1usize as *const u16),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            IMAGE_FLAGS(0),
        )
        .map(|h| HICON(h.0))
        .unwrap_or_default();
        let nid = tray_data(hwnd, icon);
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

unsafe fn remove_tray(hwnd: HWND) {
    unsafe {
        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = TRAY_ID;
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

/// Remove the tray icon and exit the process.
pub fn quit() {
    let h = LISTENER.load(Relaxed);
    if h != 0 {
        unsafe { remove_tray(HWND(h as *mut c_void)) };
    }
    std::process::exit(0);
}

/// Sync tray menu colors with the app theme + selected accent (0xRRGGBB).
pub fn set_menu_look(bg: u32, fg: u32, dim: u32, line: u32, accent: u32, hover: u32, atext: u32) {
    LOOK_BG.store(bg, Relaxed);
    LOOK_FG.store(fg, Relaxed);
    LOOK_DIM.store(dim, Relaxed);
    LOOK_LINE.store(line, Relaxed);
    LOOK_ACCENT.store(accent, Relaxed);
    LOOK_HOVER.store(hover, Relaxed);
    LOOK_ATEXT.store(atext, Relaxed);
}

/// Whether Windows apps are in dark mode (`AppsUseLightTheme == 0`).
pub fn system_apps_dark() -> bool {
    unsafe {
        let mut val = 1u32;
        let mut size = 4u32;
        let err = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut val as *mut u32 as *mut c_void),
            Some(&mut size),
        );
        err != ERROR_SUCCESS || val == 0
    }
}

fn to_colorref(rgb: u32) -> COLORREF {
    COLORREF(((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff))
}

// FastBox-style tray popup: compact header + Open / Exit, left accent rail on hover.
const MENU_W: i32 = 148;
const MENU_HEADER: i32 = 28;
const MENU_PAD: i32 = 4;
const MENU_ITEM_H: i32 = 28;
const MENU_H: i32 = MENU_HEADER + MENU_PAD + MENU_ITEM_H * 2 + MENU_PAD;
const MENU_RADIUS: i32 = 10;
const MENU_ACCENT_W: i32 = 3; // bright vertical line on the left of the hovered row

fn menu_item_at(y: i32) -> isize {
    let start = MENU_HEADER + MENU_PAD;
    if y < start { return -1; }
    let i = (y - start) / MENU_ITEM_H;
    if (0..2).contains(&i) { i as isize } else { -1 }
}

fn menu_item_id(ix: isize) -> usize {
    match ix {
        0 => ID_OPEN,
        1 => ID_EXIT,
        _ => 0,
    }
}

unsafe fn register_menu_class(inst: HINSTANCE) {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| unsafe {
        let _ = RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(menu_proc),
            hInstance: inst,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("clipbox_tray_menu"),
            ..Default::default()
        });
    });
}

unsafe fn close_menu(cmd: usize) {
    MENU_RESULT.store(cmd, Relaxed);
    MENU_DONE.store(true, Relaxed);
    let h = MENU_HWND.load(Relaxed);
    if h != 0 {
        unsafe { PostMessageW(Some(HWND(h as *mut c_void)), WM_MENU_CLOSE, WPARAM(0), LPARAM(0)).ok(); }
    }
}

unsafe fn paint_menu(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);

        let bg = to_colorref(LOOK_BG.load(Relaxed));
        let fg = to_colorref(LOOK_FG.load(Relaxed));
        let dim = to_colorref(LOOK_DIM.load(Relaxed));
        let line = to_colorref(LOOK_LINE.load(Relaxed));
        let accent = to_colorref(LOOK_ACCENT.load(Relaxed));
        let hover = to_colorref(LOOK_HOVER.load(Relaxed));
        let atext = to_colorref(LOOK_ATEXT.load(Relaxed));
        let hi = MENU_HOVER.load(Relaxed);

        let br_bg = CreateSolidBrush(bg);
        FillRect(hdc, &rc, br_bg);

        // ── header: mini logo + "clipbox" ──
        let logo_x = 10i32;
        let logo_y = 8i32;
        for (i, (w, h, o_y)) in [(7, 2, 0), (9, 2, 3), (11, 4, 6)].iter().enumerate() {
            let shade = [0x55u32, 0x99, 0xff][i];
            let c = LOOK_ACCENT.load(Relaxed);
            let r = ((c >> 16) & 0xff) * shade / 255;
            let g = ((c >> 8) & 0xff) * shade / 255;
            let b = (c & 0xff) * shade / 255;
            let br = CreateSolidBrush(to_colorref((r << 16) | (g << 8) | b));
            let bar = RECT {
                left: logo_x + (11 - w) / 2,
                top: logo_y + o_y,
                right: logo_x + (11 - w) / 2 + w,
                bottom: logo_y + o_y + h,
            };
            FillRect(hdc, &bar, br);
            let _ = DeleteObject(br.into());
        }

        let font_title = CreateFontW(
            -12, 0, 0, 0, FW_BOLD.0 as i32, 0, 0, 0,
            DEFAULT_CHARSET, FONT_OUTPUT_PRECISION(0), FONT_CLIP_PRECISION(0), CLEARTYPE_QUALITY,
            0, w!("Cascadia Mono"),
        );
        let old_font = SelectObject(hdc, font_title.into());
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, fg);
        let mut title_rc = RECT { left: 26, top: 0, right: rc.right - 6, bottom: MENU_HEADER };
        let mut title: Vec<u16> = "clipbox".encode_utf16().collect();
        DrawTextW(hdc, &mut title, &mut title_rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        let br_line = CreateSolidBrush(line);
        let head_line = RECT { left: 8, top: MENU_HEADER - 1, right: rc.right - 8, bottom: MENU_HEADER };
        FillRect(hdc, &head_line, br_line);

        let font = CreateFontW(
            -12, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
            DEFAULT_CHARSET, FONT_OUTPUT_PRECISION(0), FONT_CLIP_PRECISION(0), CLEARTYPE_QUALITY,
            0, w!("Cascadia Mono"),
        );
        SelectObject(hdc, font.into());

        // ── Open / Exit — FastBox: inset hover + bright left rail + ">" ──
        for i in 0..2i32 {
            let top = MENU_HEADER + MENU_PAD + i * MENU_ITEM_H;
            let on = hi == i as isize;
            if on {
                // Soft inset highlight (like FastBox row).
                let fill = RECT { left: 4, top, right: rc.right - 4, bottom: top + MENU_ITEM_H };
                let br = CreateSolidBrush(hover);
                FillRect(hdc, &fill, br);
                let _ = DeleteObject(br.into());
                // Bright accent line flush to the left of the row.
                let rail = RECT {
                    left: 4,
                    top,
                    right: 4 + MENU_ACCENT_W,
                    bottom: top + MENU_ITEM_H,
                };
                let br_a = CreateSolidBrush(accent);
                FillRect(hdc, &rail, br_a);
                let _ = DeleteObject(br_a.into());

                SetTextColor(hdc, to_colorref(0xffffff));
                let mut mark = RECT { left: 12, top, right: 24, bottom: top + MENU_ITEM_H };
                let mut caret: Vec<u16> = ">".encode_utf16().collect();
                DrawTextW(hdc, &mut caret, &mut mark, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
                SetTextColor(hdc, atext);
            } else {
                SetTextColor(hdc, dim);
            }
            let mut text_rc = RECT { left: 26, top, right: rc.right - 8, bottom: top + MENU_ITEM_H };
            let mut label: Vec<u16> = (if i == 0 { "Open" } else { "Exit" }).encode_utf16().collect();
            DrawTextW(hdc, &mut label, &mut text_rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        }

        SelectObject(hdc, old_font);
        let _ = DeleteObject(font_title.into());
        let _ = DeleteObject(font.into());
        let _ = DeleteObject(br_bg.into());
        let _ = DeleteObject(br_line.into());
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn menu_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                paint_menu(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let y = ((lp.0 >> 16) & 0xffff) as i16 as i32;
                let next = menu_item_at(y);
                if MENU_HOVER.swap(next, Relaxed) != next {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let x = (lp.0 & 0xffff) as i16 as i32;
                let y = ((lp.0 >> 16) & 0xffff) as i16 as i32;
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                // With SetCapture, clicks outside arrive with coords outside the client rect.
                if x < rc.left || x >= rc.right || y < rc.top || y >= rc.bottom {
                    close_menu(0);
                    return LRESULT(0);
                }
                let ix = menu_item_at(y);
                if ix >= 0 {
                    close_menu(menu_item_id(ix));
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                let x = (lp.0 & 0xffff) as i16 as i32;
                let y = ((lp.0 >> 16) & 0xffff) as i16 as i32;
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                if x < rc.left || x >= rc.right || y < rc.top || y >= rc.bottom {
                    close_menu(0);
                }
                LRESULT(0)
            }
            WM_ACTIVATE => {
                // Lost focus (clicked another window) → dismiss.
                if (wp.0 as u32 & 0xffff) == 0 {
                    close_menu(0);
                }
                LRESULT(0)
            }
            WM_CAPTURECHANGED => {
                if !MENU_DONE.load(Relaxed) {
                    close_menu(0);
                }
                LRESULT(0)
            }
            x if x == WM_MENU_CLOSE => LRESULT(0),
            WM_ERASEBKGND => LRESULT(1),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Custom tray popup (header + Open / Exit). Click an item or click outside to dismiss.
unsafe fn show_tray_menu(_owner: HWND, tx: &UnboundedSender<Event>) {
    unsafe {
        if MENU_HWND.load(Relaxed) != 0 { return; }
        MENU_DONE.store(false, Relaxed);
        MENU_RESULT.store(0, Relaxed);
        MENU_HOVER.store(-1, Relaxed);

        let inst = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
        register_menu_class(inst.into());

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let (sw, _sh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        let (sx, sy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let mut x = pt.x;
        let mut y = pt.y - MENU_H;
        if y < sy { y = pt.y; }
        if x + MENU_W > sx + sw { x = sx + sw - MENU_W; }
        if x < sx { x = sx; }

        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("clipbox_tray_menu"),
            w!(""),
            WS_POPUP,
            x, y, MENU_W, MENU_H,
            None, None, Some(inst.into()), None,
        ) else { return };
        // Rounded corners like FastBox.
        let rgn = CreateRoundRectRgn(0, 0, MENU_W + 1, MENU_H + 1, MENU_RADIUS, MENU_RADIUS);
        let _ = SetWindowRgn(hwnd, Some(rgn), true);
        MENU_HWND.store(hwnd.0 as isize, Relaxed);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
        let _ = SetForegroundWindow(hwnd);
        // Capture mouse so outside clicks are delivered here and can dismiss the menu.
        SetCapture(hwnd);

        let mut msg = MSG::default();
        while !MENU_DONE.load(Relaxed) {
            let ok = GetMessageW(&mut msg, None, 0, 0);
            if !ok.as_bool() { break; }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        if GetCapture().0 == hwnd.0 {
            let _ = ReleaseCapture();
        }
        let _ = DestroyWindow(hwnd);
        MENU_HWND.store(0, Relaxed);

        match MENU_RESULT.load(Relaxed) {
            ID_OPEN => { tx.unbounded_send(Event::Open).ok(); }
            ID_EXIT => { tx.unbounded_send(Event::Quit).ok(); }
            _ => {}
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some((tx, dir)) = TX.get() else { return unsafe { DefWindowProcW(hwnd, msg, wp, lp) } };
    match msg {
        WM_CLIPBOARDUPDATE => {
            if let Some(c) = unsafe { read_clipboard(hwnd, dir) } {
                tx.unbounded_send(Event::Clip(c)).ok();
            }
            LRESULT(0)
        }
        WM_SET_HOTKEY => {
            unsafe { register_hotkey(hwnd) };
            LRESULT(0)
        }
        WM_HOTKEY => {
            tx.unbounded_send(Event::Toggle).ok();
            LRESULT(0)
        }
        WM_TRAY => {
            match lp.0 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    tx.unbounded_send(Event::Open).ok();
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => unsafe { show_tray_menu(hwnd, tx) },
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { remove_tray(hwnd) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// The clipboard is often briefly locked by the app that just wrote it.
unsafe fn open_clipboard(hwnd: HWND) -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(Some(hwnd)) }.is_ok() { return true; }
        thread::sleep(Duration::from_millis(15));
    }
    false
}

unsafe fn read_clipboard(hwnd: HWND, dir: &Path) -> Option<NewClip> {
    unsafe {
        // Respect password managers (same convention Windows' own history uses).
        let skip = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
        let allow = RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory"));
        let src = source_app();
        if !open_clipboard(hwnd) { return None; }
        let private = SKIP_PRIVATE.load(Relaxed) && IsClipboardFormatAvailable(skip).is_ok()
            || SKIP_PRIVATE.load(Relaxed) && (IsClipboardFormatAvailable(allow).is_ok() && with_global(allow, |p, _| *(p as *const u32)) == Some(0));
        if private {
            CloseClipboard().ok();
            return None;
        }
        let clip = if IsClipboardFormatAvailable(CF_HDROP.0 as u32).is_ok() {
            read_files(dir, src)
        } else if IsClipboardFormatAvailable(CF_DIB).is_ok() {
            read_dib(dir, src)
        } else if IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() {
            read_text(src)
        } else {
            None
        };
        CloseClipboard().ok();
        clip
    }
}

unsafe fn with_global<R>(fmt: u32, f: impl FnOnce(*const u8, usize) -> R) -> Option<R> {
    unsafe {
        let h = HGLOBAL(GetClipboardData(fmt).ok()?.0);
        let p = GlobalLock(h) as *const u8;
        if p.is_null() { return None; }
        let r = f(p, GlobalSize(h));
        GlobalUnlock(h).ok();
        Some(r)
    }
}

unsafe fn read_text(src: String) -> Option<NewClip> {
    let text = unsafe {
        with_global(CF_UNICODETEXT, |p, n| {
            let s = std::slice::from_raw_parts(p as *const u16, n / 2);
            String::from_utf16_lossy(&s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())])
        })?
    };
    if text.trim().is_empty() { return None; }
    let lines = text.lines().count();
    Some(NewClip {
        kind: Kind::of_text(&text),
        meta: if lines > 1 { format!("{lines} lines") } else { format!("{} chars", text.chars().count()) },
        hash: hash(text.as_bytes()),
        size: text.len() as i64,
        thumb: String::new(),
        text,
        src,
    })
}

unsafe fn read_files(dir: &Path, src: String) -> Option<NewClip> {
    unsafe {
        let hdrop = HDROP(GetClipboardData(CF_HDROP.0 as u32).ok()?.0);
        let n = DragQueryFileW(hdrop, u32::MAX, None);
        let mut paths = Vec::new();
        for i in 0..n {
            let mut buf = vec![0u16; DragQueryFileW(hdrop, i, None) as usize + 1];
            let len = DragQueryFileW(hdrop, i, Some(buf.as_mut_slice())) as usize;
            paths.push(String::from_utf16_lossy(&buf[..len]));
        }
        let first = paths.first()?.clone();
        let text = paths.join("\n");
        let h = hash(text.as_bytes());
        let kind = if n == 1 { Kind::of_path(&first) } else { Kind::File };
        let size = paths.iter().map(|p| std::fs::metadata(p).map(|m| m.len() as i64).unwrap_or(0)).sum();
        let meta = if n == 1 { first.clone() } else { format!("{n} files") };
        Some(NewClip { kind, meta, thumb: shell_thumb(&first, dir, h).unwrap_or_default(), text, src, size, hash: h })
    }
}

/// CF_DIB (any 24/32-bit bitmap) → png file.
unsafe fn read_dib(dir: &Path, src: String) -> Option<NewClip> {
    let (w, h, rgba) = unsafe {
        with_global(CF_DIB, |p, _| {
            let hdr = &*(p as *const BITMAPINFOHEADER);
            let (w, h, bpp) = (hdr.biWidth, hdr.biHeight, hdr.biBitCount as usize);
            if w <= 0 || h == 0 || (bpp != 24 && bpp != 32) { return None; }
            let masks = if hdr.biCompression == 3 && hdr.biSize == 40 { 12 } else { 0 };
            let px = p.add(hdr.biSize as usize + masks + hdr.biClrUsed as usize * 4);
            Some((w as u32, h.unsigned_abs(), dib_to_rgba(px, w as usize, h, bpp)))
        })??
    };
    let hs = hash(&rgba);
    let path = dir.join(format!("{:016x}.png", hs as u64));
    if !path.exists() {
        image::save_buffer(&path, &rgba, w, h, image::ExtendedColorType::Rgba8).ok()?;
    }
    let p = path.to_string_lossy().into_owned();
    let size = std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0);
    Some(NewClip { kind: Kind::Image, meta: format!("{w}×{h}"), thumb: p.clone(), text: p, src, size, hash: hs })
}

/// Bottom-up (h>0) or top-down (h<0) BGR(A) rows → top-down RGBA.
unsafe fn dib_to_rgba(px: *const u8, w: usize, h: i32, bpp: usize) -> Vec<u8> {
    let rows = h.unsigned_abs() as usize;
    let stride = (w * bpp / 8 + 3) & !3;
    let mut out = Vec::with_capacity(w * rows * 4);
    let mut any_alpha = false;
    for y in 0..rows {
        let sy = if h > 0 { rows - 1 - y } else { y };
        let row = unsafe { std::slice::from_raw_parts(px.add(sy * stride), w * bpp / 8) };
        for c in row.chunks_exact(bpp / 8) {
            let a = if bpp == 32 { c[3] } else { 255 };
            any_alpha |= a != 0;
            out.extend_from_slice(&[c[2], c[1], c[0], a]);
        }
    }
    if !any_alpha {
        out.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    }
    out
}

/// Windows already knows how to thumbnail videos, pdfs and images — ask it.
unsafe fn shell_thumb(path: &str, dir: &Path, h: i64) -> Option<String> {
    unsafe {
        let out = dir.join(format!("t{:016x}.png", h as u64));
        if out.exists() { return Some(out.to_string_lossy().into_owned()); }
        let f: IShellItemImageFactory = SHCreateItemFromParsingName(PCWSTR(wide(path).as_ptr()), None).ok()?;
        let hbmp = f.GetImage(SIZE { cx: 256, cy: 256 }, SIIGBF_RESIZETOFIT | SIIGBF_THUMBNAILONLY).ok()?;
        let mut bm = BITMAP::default();
        GetObjectW(hbmp.into(), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void));
        let (w, hh) = (bm.bmWidth, bm.bmHeight);
        let mut bi = BITMAPINFO::default();
        bi.bmiHeader = BITMAPINFOHEADER { biSize: 40, biWidth: w, biHeight: -hh, biPlanes: 1, biBitCount: 32, ..Default::default() };
        let mut buf = vec![0u8; (w * hh * 4) as usize];
        let dc = GetDC(None);
        GetDIBits(dc, hbmp, 0, hh as u32, Some(buf.as_mut_ptr() as *mut c_void), &mut bi, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        DeleteObject(hbmp.into()).ok().ok();
        let rgba = dib_to_rgba(buf.as_ptr(), w as usize, -hh, 32);
        image::save_buffer(&out, &rgba, w as u32, hh as u32, image::ExtendedColorType::Rgba8).ok()?;
        Some(out.to_string_lossy().into_owned())
    }
}

/// Name of the app in the foreground when the copy happened ("chrome", "Code"…).
fn source_app() -> String {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid as *mut u32));
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        CloseHandle(h).ok();
        if !ok { return String::new(); }
        let p = String::from_utf16_lossy(&buf[..len as usize]);
        Path::new(&p).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }
}


// ───────────────────────────── writing / pasting ─────────────────────────────

unsafe fn set_data(fmt: u32, bytes: &[u8]) {
    unsafe {
        let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) else { return };
        let p = GlobalLock(h) as *mut u8;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        GlobalUnlock(h).ok();
        SetClipboardData(fmt, Some(HANDLE(h.0))).ok();
    }
}

fn utf16_bytes(s: &str) -> Vec<u8> {
    wide(s).iter().flat_map(|c| c.to_le_bytes()).collect()
}

/// Put an item back on the clipboard. `owner` must be one of our windows.
pub fn write(owner: isize, kind: Kind, text: &str, plain: bool) {
    unsafe {
        if !open_clipboard(HWND(owner as *mut c_void)) { return; }
        EmptyClipboard().ok();
        let ours = TX.get().is_some_and(|t| Path::new(text).starts_with(&t.1));
        if plain || matches!(kind, Kind::Text | Kind::Code | Kind::Link) {
            set_data(CF_UNICODETEXT, &utf16_bytes(text));
        } else if !(kind == Kind::Image && ours) {
            // DROPFILES header (20 bytes, wide paths) + double-null-terminated list.
            let mut b = vec![20u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0];
            for p in text.lines() { b.extend(utf16_bytes(p)); }
            b.extend([0, 0]);
            set_data(CF_HDROP.0 as u32, &b);
        } else if let Ok(img) = image::open(text) {
            let img = img.to_rgba8();
            let (w, h) = img.dimensions();
            let hdr = BITMAPINFOHEADER { biSize: 40, biWidth: w as i32, biHeight: -(h as i32), biPlanes: 1, biBitCount: 32, ..Default::default() };
            let mut b = std::slice::from_raw_parts(&hdr as *const _ as *const u8, 40).to_vec();
            b.extend(img.pixels().flat_map(|p| [p[2], p[1], p[0], p[3]]));
            set_data(CF_DIB, &b);
        }
        CloseClipboard().ok();
    }
}

/// Focus the window that was active before ClipBox and press Ctrl+V in it.
pub fn paste_into(prev: isize) {
    thread::spawn(move || unsafe {
        SetForegroundWindow(HWND(prev as *mut c_void)).ok().ok();
        thread::sleep(Duration::from_millis(40));
        let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() }, ..Default::default() } },
        };
        let seq = [key(VK_CONTROL, false), key(VK_V, false), key(VK_V, true), key(VK_CONTROL, true)];
        SendInput(&seq, size_of::<INPUT>() as i32);
    });
}

pub fn foreground() -> isize { unsafe { GetForegroundWindow().0 as isize } }

pub fn show(hwnd: isize) {
    unsafe {
        APP_HWND.store(hwnd, Relaxed);
        let me = HWND(hwnd as *mut c_void);
        let _ = ShowWindow(me, SW_SHOW);
        // Become foreground so a later outside click reliably deactivates us too.
        let _ = SetForegroundWindow(me);
        let dim = HWND(DIM.load(Relaxed) as *mut c_void);
        if !dim.0.is_null() {
            // Always show the dim as an outside-click catcher. Alpha 0 → nearly invisible (1).
            let alpha = DIM_ALPHA.load(Relaxed).max(1) as u8;
            let _ = SetLayeredWindowAttributes(dim, COLORREF(0), alpha, LWA_ALPHA);
            let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
            let (w, h) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
            // Right behind ClipBox so clicks on the box hit the app; elsewhere hit the dim.
            let _ = SetWindowPos(dim, Some(me), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }
}

pub fn hide(hwnd: isize) {
    unsafe {
        let _ = ShowWindow(HWND(hwnd as *mut c_void), SW_HIDE);
        let dim = DIM.load(Relaxed);
        if dim != 0 {
            let _ = ShowWindow(HWND(dim as *mut c_void), SW_HIDE);
        }
        APP_HWND.store(0, Relaxed);
    }
}

pub fn hwnd_of(w: &impl raw_window_handle::HasWindowHandle) -> isize {
    match w.window_handle().map(|h| h.as_raw()) {
        Ok(raw_window_handle::RawWindowHandle::Win32(h)) => h.hwnd.get(),
        _ => 0,
    }
}

/// Local time offset from UTC in seconds.
pub fn utc_offset() -> i64 {
    unsafe {
        let mut tz = TIME_ZONE_INFORMATION::default();
        let daylight = GetTimeZoneInformation(&mut tz) == 2;
        -((tz.Bias + if daylight { tz.DaylightBias } else { 0 }) as i64) * 60
    }
}
