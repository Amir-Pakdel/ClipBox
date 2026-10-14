<p align="center"><img src="assets/logo.svg" width="96" alt="ClipBox logo"></p>

# ClipBox

**v1.0.0** — a small, keyboard-first clipboard history.

- Saves everything you copy: text, code, links, images, videos, PDFs, audio and files.
- Keeps your history until you delete it.
- Runs in the background; optional start on boot.
- Tray icon with Open / Exit; Esc and × only hide the window.
- Uses almost no CPU while idle.

## Install

In PowerShell:

```powershell
irm https://raw.githubusercontent.com/Amir-Pakdel/ClipBox/main/install.ps1 | iex
```

The same command installs ClipBox the first time and updates it later. It only replaces
the program: your history and settings are never touched. If the download fails, the
installed version is left as it was.

## Build from source

### Requirements

- Windows 10 or 11
- [Rust](https://rustup.rs) with the MSVC toolchain
- Windows 10/11 SDK (install it with the Visual Studio Build Tools)

### Build and install

Open PowerShell in the project folder and run:

```powershell
cargo build --profile dist
taskkill /im clipbox.exe /f 2>$null
mkdir "$env:LOCALAPPDATA\Programs\ClipBox" -Force
copy target\dist\clipbox.exe "$env:LOCALAPPDATA\Programs\ClipBox\"
& "$env:LOCALAPPDATA\Programs\ClipBox\clipbox.exe"
```

The first build takes a few minutes.

Always start ClipBox from the install folder, not from `target`. On launch it adds
itself to Windows startup using its own path.

## Update

Pull or edit the code, then run the same commands as in Install.
Your history and settings are kept.

## Uninstall

1. Open ClipBox, press `Ctrl+,` and set **start on boot** to `off`.
2. Right-click the tray icon → **Exit** (or `taskkill /im clipbox.exe /f`).
3. Delete `%LOCALAPPDATA%\Programs\ClipBox`.
4. To also delete your history and settings, delete `%APPDATA%\clipbox`.

## Use

Press `Alt+V` to open ClipBox. Start typing to search.

| Key | Action |
|---|---|
| type | search |
| `Up` / `Down` | move |
| `Enter` | paste into the previous window |
| `Shift+Enter` | paste as plain text |
| `Tab` | next type: all, text, link, media, file |
| `Right` / `Left` | big preview / back |
| `Ctrl+P` | pin or unpin |
| `Del` | remove |
| `Ctrl+Z` | undo remove |
| `Ctrl+,` | settings |
| `Esc` | clear search, then hide |

With the mouse: click to select, double-click to paste, right-click to pin,
scroll to move through the list.

`Esc`, `Alt+F4` and clicking outside only hide ClipBox. It keeps running.

## Settings

Press `Ctrl+,`. Use `Up` / `Down` to choose a setting and `Left` / `Right` to change it.

| Setting | Options |
|---|---|
| theme | auto, dark, light |
| primary | 5 colors |
| open with | `alt+v`, `ctrl+shift+v`, `ctrl+alt+v`, `alt+c`, ``ctrl+` `` |
| start on boot | on, off |
| keep history | forever, 1 year, 3 months, 1 month, 1 week (pinned items are always kept) |
| skip passwords | on, off (ignore copies that password managers mark as private) |
| text size | 12, 13, 14 |
| dim screen | off, light, medium, dark |

## Develop

```powershell
cargo run          # fast debug build
.\run.bat          # stop the running copy, build, and start the new one
```

Only one ClipBox runs at a time. A second copy exits right away.

| File | Purpose |
|---|---|
| `src/main.rs` | user interface: search, list, preview, settings |
| `src/store.rs` | history database |
| `src/win.rs` | Windows: clipboard, hotkey, paste, thumbnails |
| `src/unix.rs` | macOS and Linux: clipboard, hotkey, paste, tray |
| `src/autostart.rs` | start on login (Windows, macOS, Linux) |
| `vendor/gpui` | GPUI 0.2.2 with a right-to-left text fix (see `PATCHES.md`) |

Data is stored in `%APPDATA%\clipbox` on Windows, `~/Library/Application Support/clipbox` on macOS, and `~/.local/share/clipbox` on Linux.

## Release

Tagged versions publish a GitHub Release with the source archives plus builds:

- `clipbox-windows-x64.zip`
- `clipbox-linux-x64.tar.gz`
- `clipbox-linux-arm64.tar.gz`
- `clipbox-macos-arm64.tar.gz`

GitHub adds **Source code (zip)** and **Source code (tar.gz)** to that release on its own.
