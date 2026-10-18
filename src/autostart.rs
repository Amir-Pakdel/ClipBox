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
        Win32::System::Registry::{
            HKEY_CURRENT_USER, REG_BINARY, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
        },
        core::w,
    };

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    fn install_dir() -> Option<PathBuf> {
        Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Programs").join("ClipBox"))
    }

    fn usable(path: &std::path::Path) -> bool {
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
    }

    /// Copy this exe into the install folder (if needed) and return that path.
    /// Boot startup always points here so it survives moving the download copy.
    /// A running dest is renamed aside first: Windows lets you rename a locked exe.
    fn ensure_installed() -> Option<PathBuf> {
        let dir = install_dir()?;
        let dest = dir.join("clipbox.exe");
        let current = std::env::current_exe().ok()?;
        std::fs::create_dir_all(&dir).ok()?;
        let same = current.canonicalize().ok().zip(dest.canonicalize().ok()).is_some_and(|(a, b)| a == b);
        if same {
            let _ = std::fs::remove_file(dir.join("clipbox.exe.old"));
            return Some(dest);
        }
        let old = dir.join("clipbox.exe.old");
        let _ = std::fs::remove_file(&old);
        if dest.exists() {
            let _ = std::fs::rename(&dest, &old);
        }
        if std::fs::copy(&current, &dest).is_err() && !usable(&dest) {
            let _ = std::fs::rename(&old, &dest);
        } else if usable(&dest) {
            let _ = std::fs::remove_file(&old);
        }
        if usable(&dest) { Some(dest) } else if usable(&current) { Some(current) } else { None }
    }

    const RUN: windows::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    /// Task Manager's on/off switch. Byte 0 is 2 when enabled and 3 when disabled.
    /// Writing the Run key alone does not clear a previous disable.
    const APPROVED: windows::core::PCWSTR =
        w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run");

    fn approved(on: bool) {
        let data: [u8; 12] = [if on { 2 } else { 3 }, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        unsafe {
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                APPROVED,
                w!("ClipBox"),
                REG_BINARY.0,
                Some(data.as_ptr() as *const c_void),
                data.len() as u32,
            );
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, APPROVED, w!("CoBox"));
        }
    }

    pub fn set(on: bool) {
        unsafe {
            if !on {
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, w!("ClipBox"));
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, w!("CoBox"));
                approved(false);
                return;
            }
            let Some(exe) = ensure_installed() else { return };
            let v = wide(&format!("\"{}\"", exe.display()));
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN,
                w!("ClipBox"),
                REG_SZ.0,
                Some(v.as_ptr() as *const c_void),
                (v.len() * 2) as u32,
            );
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, w!("CoBox"));
            approved(true);
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::{fs, path::PathBuf, process::Command};

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

    fn uid() -> Option<String> {
        let out = Command::new("id").arg("-u").output().ok()?;
        let s = String::from_utf8(out.stdout).ok()?;
        let s = s.trim();
        if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) { Some(s.to_string()) } else { None }
    }

    /// Drop a previous `launchctl disable`, then load the agent for this login and the next one.
    fn reload(plist: &PathBuf) {
        let Some(uid) = uid() else { return };
        let target = format!("gui/{uid}/{LABEL}");
        let domain = format!("gui/{uid}");
        let _ = Command::new("launchctl").args(["bootout", &target]).status();
        let _ = Command::new("launchctl").args(["enable", &target]).status();
        let _ = Command::new("launchctl").args(["bootstrap", &domain, &plist.to_string_lossy()]).status();
    }

    fn bootout() {
        let Some(uid) = uid() else { return };
        let target = format!("gui/{uid}/{LABEL}");
        let _ = Command::new("launchctl").args(["bootout", &target]).status();
        let _ = Command::new("launchctl").args(["disable", &target]).status();
    }

    pub fn set(on: bool) {
        let Some(plist) = plist_path() else { return };
        if !on {
            bootout();
            let _ = fs::remove_file(&plist);
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let exe = exe.canonicalize().unwrap_or(exe);
        if let Some(parent) = plist.parent() {
            let _ = fs::create_dir_all(parent);
        }
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
        if fs::write(&plist, body).is_ok() {
            reload(&plist);
        }
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
