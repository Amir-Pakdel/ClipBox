//! Clipboard, hotkey, tray, and show/hide for macOS and Linux.
//!
//! Windows keeps its own implementation in `win.rs`. The public surface matches
//! so the UI can call `win::` on every platform.

use crate::store::{Kind, NewClip, hash};
use futures::channel::mpsc::UnboundedSender;
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use fs2::FileExt;
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{OnceLock, atomic::{AtomicBool, AtomicI64, AtomicIsize, AtomicU32, AtomicU8, Ordering::Relaxed}},
    thread,
    time::Duration,
};

pub enum Event {
    Clip(NewClip),
    Toggle,
    Open,
    Quit,
}

pub static DIM_ALPHA: AtomicU32 = AtomicU32::new(90);
pub static SKIP_PRIVATE: AtomicBool = AtomicBool::new(true);

const KIND_X11: u8 = 1;
const KIND_MAC: u8 = 2;

static KIND: AtomicU8 = AtomicU8::new(0);
static HWND: AtomicIsize = AtomicIsize::new(0);
static HOTKEY: AtomicU32 = AtomicU32::new(0);
static HOTKEY_GEN: AtomicU32 = AtomicU32::new(1);
static LAST_HASH: AtomicI64 = AtomicI64::new(0);
static LOCK: OnceLock<File> = OnceLock::new();

pub const HOTKEYS: [(&str, u32, u32); 5] = [
    ("alt + v", 0, 0),
    ("ctrl + shift + v", 0, 0),
    ("ctrl + alt + v", 0, 0),
    ("alt + c", 0, 0),
    ("ctrl + `", 0, 0),
];

pub fn single_instance() {
    let path = lock_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(file) = File::options().create(true).read(true).write(true).open(&path) else { return };
    if file.try_lock_exclusive().is_err() {
        std::process::exit(0);
    }
    let _ = LOCK.set(file);
}

pub fn set_hotkey(ix: usize) {
    HOTKEY.store(ix.min(HOTKEYS.len() - 1) as u32, Relaxed);
    HOTKEY_GEN.fetch_add(1, Relaxed);
    #[cfg(target_os = "macos")]
    apply_mac_hotkey();
}

pub fn spawn(tx: UnboundedSender<Event>, dir: PathBuf) {
    #[cfg(target_os = "macos")]
    {
        MAC_HOTKEY.with(|slot| {
            if let Ok(manager) = GlobalHotKeyManager::new() {
                apply_hotkey(&manager);
                *slot.borrow_mut() = Some(manager);
            }
        });
        install_tray();
    }
    thread::spawn(move || listen(tx, dir));
}

pub fn quit() {
    std::process::exit(0);
}

pub fn set_menu_look(_bg: u32, _fg: u32, _dim: u32, _line: u32, _accent: u32, _hover: u32, _atext: u32) {}

pub fn system_apps_dark() -> bool {
    #[cfg(target_os = "macos")]
    {
        command_has("defaults", &["read", "-g", "AppleInterfaceStyle"], "dark")
    }
    #[cfg(target_os = "linux")]
    {
        command_has("gsettings", &["get", "org.gnome.desktop.interface", "color-scheme"], "dark")
            || command_has("gsettings", &["get", "org.gnome.desktop.interface", "gtk-theme"], "dark")
    }
}

pub fn write(_owner: isize, kind: Kind, text: &str, plain: bool) {
    let Ok(mut cb) = arboard::Clipboard::new() else { return };
    if plain || matches!(kind, Kind::Text | Kind::Code | Kind::Link) {
        LAST_HASH.store(hash(text.as_bytes()), Relaxed);
        let _ = cb.set_text(text);
        return;
    }
    if kind == Kind::Image
        && let Ok(img) = image::open(text)
    {
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        let bytes = rgba.into_raw();
        LAST_HASH.store(hash(&bytes), Relaxed);
        let _ = cb.set_image(arboard::ImageData {
            width: w as usize,
            height: h as usize,
            bytes: bytes.into(),
        });
        return;
    }
    LAST_HASH.store(hash(text.as_bytes()), Relaxed);
    let _ = cb.set_text(text);
}

pub fn paste_into(_prev: isize) {
    thread::spawn(|| {
        thread::sleep(Duration::from_millis(80));
        use enigo::Keyboard;
        let Ok(mut enigo) = enigo::Enigo::new(&enigo::Settings::default()) else { return };
        let modifier = if cfg!(target_os = "macos") { enigo::Key::Meta } else { enigo::Key::Control };
        let _ = enigo.key(modifier, enigo::Direction::Press);
        let _ = enigo.key(enigo::Key::Unicode('v'), enigo::Direction::Click);
        let _ = enigo.key(modifier, enigo::Direction::Release);
    });
}

pub fn foreground() -> isize {
    HWND.load(Relaxed)
}

pub fn awaiting_activation() -> bool { false }
pub fn became_active() {}
pub fn is_zoomed(_hwnd: isize) -> bool { false }
pub fn toggle_zoom(_hwnd: isize) {}

pub fn show(hwnd: isize) {
    match KIND.load(Relaxed) {
        KIND_MAC => mac_order(hwnd, true),
        KIND_X11 => x11_map(hwnd, true),
        _ => {}
    }
}

pub fn hide(hwnd: isize) {
    match KIND.load(Relaxed) {
        KIND_MAC => mac_order(hwnd, false),
        KIND_X11 => x11_map(hwnd, false),
        _ => {}
    }
}

pub fn hwnd_of(w: &impl raw_window_handle::HasWindowHandle) -> isize {
    let id = match w.window_handle().map(|h| h.as_raw()) {
        Ok(raw_window_handle::RawWindowHandle::Xlib(h)) => {
            KIND.store(KIND_X11, Relaxed);
            h.window as isize
        }
        Ok(raw_window_handle::RawWindowHandle::Xcb(h)) => {
            KIND.store(KIND_X11, Relaxed);
            h.window.get() as isize
        }
        Ok(raw_window_handle::RawWindowHandle::AppKit(h)) => {
            KIND.store(KIND_MAC, Relaxed);
            h.ns_view.as_ptr() as isize
        }
        _ => 0,
    };
    HWND.store(id, Relaxed);
    id
}

pub fn utc_offset() -> i64 {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() { 0 } else { tm.tm_gmtoff }
    }
}

#[cfg(target_os = "macos")]
thread_local! {
    static MAC_HOTKEY: std::cell::RefCell<Option<GlobalHotKeyManager>> = const { std::cell::RefCell::new(None) };
}

#[cfg(target_os = "macos")]
fn apply_mac_hotkey() {
    MAC_HOTKEY.with(|slot| {
        if let Some(manager) = slot.borrow().as_ref() {
            apply_hotkey(manager);
        }
    });
}

fn hotkey_for(ix: usize) -> HotKey {
    let (mods, code) = match ix {
        1 => (Modifiers::CONTROL | Modifiers::SHIFT, Code::KeyV),
        2 => (Modifiers::CONTROL | Modifiers::ALT, Code::KeyV),
        3 => (Modifiers::ALT, Code::KeyC),
        4 => (Modifiers::CONTROL, Code::Backquote),
        _ => (Modifiers::ALT, Code::KeyV),
    };
    HotKey::new(Some(mods), code)
}

fn apply_hotkey(manager: &GlobalHotKeyManager) {
    let keys: Vec<HotKey> = (0..HOTKEYS.len()).map(hotkey_for).collect();
    let _ = manager.unregister_all(&keys);
    let _ = manager.register(hotkey_for(HOTKEY.load(Relaxed) as usize));
}

fn listen(tx: UnboundedSender<Event>, dir: PathBuf) {
    if let Some(clip) = read_clip(&dir) {
        LAST_HASH.store(clip.hash, Relaxed);
    }
    #[cfg(target_os = "linux")]
    let manager = GlobalHotKeyManager::new().ok();
    #[cfg(target_os = "linux")]
    {
        if let Some(manager) = &manager {
            apply_hotkey(manager);
        }
        install_tray();
    }
    #[cfg(target_os = "linux")]
    let mut seen = HOTKEY_GEN.load(Relaxed);
    loop {
        #[cfg(target_os = "linux")]
        {
            let generation = HOTKEY_GEN.load(Relaxed);
            if generation != seen {
                if let Some(manager) = &manager {
                    apply_hotkey(manager);
                }
                seen = generation;
            }
        }
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state == HotKeyState::Pressed {
                let _ = tx.unbounded_send(Event::Toggle);
            }
        }
        poll_tray(&tx);
        if let Some(clip) = read_clip(&dir)
            && LAST_HASH.swap(clip.hash, Relaxed) != clip.hash
        {
            let _ = tx.unbounded_send(Event::Clip(clip));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn read_clip(dir: &Path) -> Option<NewClip> {
    let mut cb = arboard::Clipboard::new().ok()?;
    if let Ok(text) = cb.get_text() {
        let text = text.trim_end_matches('\0').to_string();
        if text.trim().is_empty() {
            return None;
        }
        let lines = text.lines().count();
        return Some(NewClip {
            kind: Kind::of_text(&text),
            meta: if lines > 1 { format!("{lines} lines") } else { format!("{} chars", text.chars().count()) },
            hash: hash(text.as_bytes()),
            size: text.len() as i64,
            thumb: String::new(),
            text,
            src: String::new(),
        });
    }
    let img = cb.get_image().ok()?;
    let bytes = img.bytes.into_owned();
    let h = hash(&bytes);
    let name = dir.join(format!("{h}.png"));
    let buf = image::RgbaImage::from_raw(img.width as u32, img.height as u32, bytes)?;
    buf.save(&name).ok()?;
    Some(NewClip {
        kind: Kind::Image,
        meta: format!("{}×{}", img.width, img.height),
        thumb: name.display().to_string(),
        text: name.display().to_string(),
        src: String::new(),
        size: (img.width * img.height * 4) as i64,
        hash: h,
    })
}

fn install_tray() {
    use tray_icon::{Icon, TrayIconBuilder, menu::{Menu, MenuItem}};
    let mut px = vec![0u8; 32 * 32 * 4];
    for (x0, y0, w, h) in [(6, 8, 20, 4), (4, 14, 24, 5), (2, 21, 28, 7)] {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                let i = (y * 32 + x) * 4;
                px[i] = 0x3a;
                px[i + 1] = 0x86;
                px[i + 2] = 0xff;
                px[i + 3] = 255;
            }
        }
    }
    let Ok(icon) = Icon::from_rgba(px, 32, 32) else { return };
    let menu = Menu::new();
    let _ = menu.append(&MenuItem::with_id("open", "Open", true, None));
    let _ = menu.append(&MenuItem::with_id("exit", "Exit", true, None));
    if let Ok(tray) = TrayIconBuilder::new()
        .with_tooltip("ClipBox")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()
    {
        // The icon has to stay alive for the whole process.
        Box::leak(Box::new(tray));
    }
}

fn poll_tray(tx: &UnboundedSender<Event>) {
    use tray_icon::{MouseButtonState, TrayIconEvent, menu::MenuEvent};
    while let Ok(event) = MenuEvent::receiver().try_recv() {
        match event.id.0.as_ref() {
            "open" => { let _ = tx.unbounded_send(Event::Open); }
            "exit" => { let _ = tx.unbounded_send(Event::Quit); }
            _ => {}
        }
    }
    while let Ok(event) = TrayIconEvent::receiver().try_recv() {
        if let TrayIconEvent::Click { button_state: MouseButtonState::Up, .. } = event {
            let _ = tx.unbounded_send(Event::Open);
        }
    }
}

fn lock_path() -> PathBuf {
    if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(run).join("clipbox.lock");
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if cfg!(target_os = "macos") {
            return home.join("Library/Application Support/clipbox/clipbox.lock");
        }
        return home.join(".local/share/clipbox/clipbox.lock");
    }
    std::env::temp_dir().join("clipbox.lock")
}

fn command_has(cmd: &str, args: &[&str], needle: &str) -> bool {
    std::process::Command::new(cmd)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_ascii_lowercase().contains(needle))
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn mac_order(hwnd: isize, front: bool) {
    if hwnd == 0 { return; }
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}
    unsafe {
        use objc::runtime::{Object, YES};
        use objc::{class, msg_send, sel, sel_impl};
        let view = hwnd as *mut Object;
        let window: *mut Object = msg_send![view, window];
        if window.is_null() { return; }
        let nil: *mut Object = std::ptr::null_mut();
        if front {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, activateIgnoringOtherApps: YES];
            let _: () = msg_send![window, makeKeyAndOrderFront: nil];
        } else {
            let _: () = msg_send![window, orderOut: nil];
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn mac_order(_hwnd: isize, _front: bool) {}

#[cfg(target_os = "linux")]
fn x11_map(hwnd: isize, map: bool) {
    if hwnd == 0 { return; }
    unsafe {
        let dpy = x11::xlib::XOpenDisplay(std::ptr::null());
        if dpy.is_null() { return; }
        let window = hwnd as std::os::raw::c_ulong;
        if map {
            x11::xlib::XMapRaised(dpy, window);
        } else {
            x11::xlib::XUnmapWindow(dpy, window);
        }
        x11::xlib::XFlush(dpy);
        x11::xlib::XCloseDisplay(dpy);
    }
}

#[cfg(not(target_os = "linux"))]
fn x11_map(_hwnd: isize, _map: bool) {}
