# Installs or updates ClipBox on Windows:
#   irm https://raw.githubusercontent.com/Amir-Pakdel/ClipBox/main/install.ps1 | iex
# Only the program file is replaced. History and settings in %APPDATA%\clipbox are never touched.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$dir = "$env:LOCALAPPDATA\Programs\ClipBox"
$exe = "$dir\clipbox.exe"
$tmp = Join-Path $env:TEMP "clipbox-install-$PID"
$update = Test-Path $exe

# Download and unpack first, so a failed download leaves the current install as it is.
Write-Host $(if ($update) { 'Updating ClipBox...' } else { 'Installing ClipBox...' })
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
    Invoke-WebRequest 'https://github.com/Amir-Pakdel/ClipBox/releases/latest/download/clipbox-windows-x64.zip' -OutFile "$tmp\clipbox.zip"
    Expand-Archive "$tmp\clipbox.zip" "$tmp\new" -Force
    if (-not (Test-Path "$tmp\new\clipbox.exe")) { throw 'The download did not contain clipbox.exe.' }

    # Close the running copy (the database survives this) and wait until the file is free.
    Get-Process clipbox,cobox -ErrorAction SilentlyContinue | Stop-Process -Force
    Get-Process clipbox,cobox -ErrorAction SilentlyContinue | Wait-Process -Timeout 10

    # Swap the program file, putting the old one back if anything goes wrong.
    New-Item -ItemType Directory -Force $dir | Out-Null
    if ($update) { Move-Item $exe "$exe.old" -Force }
    try {
        Copy-Item "$tmp\new\clipbox.exe" $exe
    } catch {
        if ($update) { Move-Item "$exe.old" $exe -Force }
        throw
    }
    Remove-Item "$exe.old" -ErrorAction SilentlyContinue
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

$lnk = (New-Object -ComObject WScript.Shell).CreateShortcut("$env:APPDATA\Microsoft\Windows\Start Menu\Programs\ClipBox.lnk")
$lnk.TargetPath = $exe
$lnk.Save()

Start-Process $exe
if ($update) { Write-Host 'ClipBox updated. Your history and settings are kept.' }
else { Write-Host "ClipBox installed to $dir. Press Alt+V to open it." }
