//! Start on login for Windows, macOS, and Linux.
//!
//! - Windows: `HKCU\...\Run` (points at the installed copy under LocalAppData)
//! - macOS: `~/Library/LaunchAgents/com.clipbox.app.plist`
//! - Linux: `~/.config/autostart/clipbox.desktop` (XDG)

/// Enable or disable start-on-login for the current platform.
pub fn set(on: bool) {
    #[cfg(windows)]
    windows::set(on);
    #[cfg(target_os = "macos")]
    macos::set(on);
    #[cfg(all(unix, not(target_os = "macos")))]
    linux::set(on);
}

#[cfg(windows)]
mod windows {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::PathBuf};
    use windows::{
        Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW},
        core::w,
    };

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    fn install_dir() -> Option<PathBuf> {
        Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Programs").join("ClipBox"))
    }

    /// Copy this exe into the install folder (if needed) and return that path.
    /// Boot startup always points here so it survives moving the download/Desktop copy.
    fn ensure_installed() -> Option<PathBuf> {
        let dir = install_dir()?;
        let dest = dir.join("clipbox.exe");
        let current = std::env::current_exe().ok()?;
        std::fs::create_dir_all(&dir).ok()?;
        let same = current
            .canonicalize()
            .ok()
            .zip(dest.canonicalize().ok())
            .is_some_and(|(a, b)| a == b);
        if !same {
            let _ = std::fs::copy(&current, &dest);
        }
        if dest.is_file() { Some(dest) } else { Some(current) }
    }

    pub fn set(on: bool) {
        let key = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        unsafe {
            if !on {
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, key, w!("ClipBox"));
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, key, w!("CoBox"));
                return;
            }
            let Some(exe) = ensure_installed() else { return };
            let v = wide(&format!("\"{}\"", exe.display()));
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key,
                w!("ClipBox"),
                REG_SZ.0,
                Some(v.as_ptr() as *const c_void),
                (v.len() * 2) as u32,
            );
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, key, w!("CoBox"));
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::{fs, path::PathBuf};

    const LABEL: &str = "com.clipbox.app";

    fn plist_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
    }

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    pub fn set(on: bool) {
        let Some(plist) = plist_path() else { return };
        if !on {
            let _ = fs::remove_file(&plist);
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let exe = exe.canonicalize().unwrap_or(exe);
        if let Some(parent) = plist.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // launchd loads ~/Library/LaunchAgents at login (same timing as Windows Run).
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
</dict>
</plist>
"#,
            xml_escape(&exe.to_string_lossy())
        );
        let _ = fs::write(&plist, body);
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod linux {
    use std::{fs, path::PathBuf};

    fn desktop_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("autostart").join("clipbox.desktop"))
    }

    fn desktop_escape(s: &str) -> String {
        // Desktop Entry Spec: backslash-escape `\`, `"`, and newlines inside quoted Exec.
        let mut out = String::with_capacity(s.len() + 8);
        for c in s.chars() {
            match c {
                '\\' | '"' => {
                    out.push('\\');
                    out.push(c);
                }
                '\n' => out.push_str("\\n"),
                _ => out.push(c),
            }
        }
        out
    }

    pub fn set(on: bool) {
        let Some(path) = desktop_path() else { return };
        if !on {
            let _ = fs::remove_file(&path);
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let exe = exe.canonicalize().unwrap_or(exe);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let body = format!(
            "\
[Desktop Entry]
Type=Application
Version=1.0
Name=ClipBox
Comment=Clipboard history
Exec=\"{}\"
Terminal=false
X-GNOME-Autostart-enabled=true
Hidden=false
",
            desktop_escape(&exe.to_string_lossy())
        );
        let _ = fs::write(&path, body);
    }
}
