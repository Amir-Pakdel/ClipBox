<p align="center"><img src="assets/logo.svg" width="96" alt="ClipBox logo"></p>

<p align="center"><strong>English</strong> · <a href="README.fa.md">فارسی</a></p>

# ClipBox

A small, keyboard-first clipboard history for Windows, macOS, and Linux.

ClipBox sits in the background and remembers what you copy: text, code, links, images, video, audio, PDFs, and files. Open it with a hotkey, search, and paste the item back into the window you were using. It stays out of the way until you ask for it.

The project lives at [github.com/Amir-Pakdel/ClipBox](https://github.com/Amir-Pakdel/ClipBox).

## What it does

- Keeps clipboard history until you delete it. Pinned items are never expired.
- Starts in the tray. Esc, Alt+F4, and the close button only hide the window.
- Optional start on login.
- Uses almost no CPU while it is idle.
- One instance only. A second launch exits immediately.

## Install on Windows

In PowerShell:

```powershell
irm https://raw.githubusercontent.com/Amir-Pakdel/ClipBox/main/install.ps1 | iex
```

The same command installs ClipBox the first time and updates it later. It replaces the program only. Your history and settings stay where they are. If the download fails, the installed copy is left untouched.

This repository is private. The installer can download a release only when the GitHub account running it can read the repo.

## Build from source

### Requirements

- Windows 10 or 11, or macOS, or Linux
- [Rust](https://rustup.rs)
- On Windows, the MSVC toolchain and the Windows 10/11 SDK

```powershell
cargo build --profile dist
```

On Windows, install the built program here and start that copy. On launch it registers itself for startup using its own path.

```powershell
taskkill /im clipbox.exe /f 2>$null
mkdir "$env:LOCALAPPDATA\Programs\ClipBox" -Force
copy target\dist\clipbox.exe "$env:LOCALAPPDATA\Programs\ClipBox\"
& "$env:LOCALAPPDATA\Programs\ClipBox\clipbox.exe"
```

The first build takes a few minutes.

## Use

Press `Alt+V` to open ClipBox. Type to search.

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

With the mouse: click to select, double-click to paste, right-click to pin, scroll to move through the list.

## Settings

Press `Ctrl+,`. `Up` / `Down` chooses a setting. `Left` / `Right` changes it.

| Setting | Options |
|---|---|
| theme | auto, dark, light |
| primary | 5 colors |
| open with | `alt+v`, `ctrl+shift+v`, `ctrl+alt+v`, `alt+c`, ``ctrl+` `` |
| start on boot | on, off |
| keep history | forever, 1 year, 3 months, 1 month, 1 week |
| skip passwords | on, off |
| text size | 12, 13, 14 |
| dim screen | off, light, medium, dark |

The link in the settings footer opens [github.com/Amir-Pakdel](https://github.com/Amir-Pakdel).

## Uninstall

1. Open ClipBox, press `Ctrl+,`, and set **start on boot** to `off`.
2. Right-click the tray icon and choose **Exit**.
3. On Windows, delete `%LOCALAPPDATA%\Programs\ClipBox`.
4. To also delete history and settings, delete `%APPDATA%\clipbox` on Windows, `~/Library/Application Support/clipbox` on macOS, or `~/.local/share/clipbox` on Linux.

## Develop

```powershell
cargo run
.\run.bat
```

| File | Purpose |
|---|---|
| `src/main.rs` | search, list, preview, settings |
| `src/store.rs` | history database |
| `src/win.rs` | Windows clipboard, hotkey, paste, tray |
| `src/unix.rs` | macOS and Linux clipboard, hotkey, paste, tray |
| `src/autostart.rs` | start on login |
| `vendor/gpui` | GPUI 0.2.2 with a right-to-left text fix |

## Release

A version tag `v*` builds and attaches:

- `clipbox-windows-x64.zip`
- `clipbox-linux-x64.tar.gz`
- `clipbox-linux-arm64.tar.gz`
- `clipbox-macos-arm64.tar.gz`

GitHub also adds the source zip and tar.gz to that release.
