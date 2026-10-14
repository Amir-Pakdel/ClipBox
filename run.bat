@echo off
cd /d "%~dp0"
taskkill /im clipbox.exe /f >/dev/null 2>&1
taskkill /im cobox.exe /f >/dev/null 2>&1
echo Building ClipBox...
cargo build > build.log 2>&1
if errorlevel 1 (
  type build.log
  echo.
  echo BUILD FAILED - see build.log
  pause
  exit /b 1
)
start "" target\debug\clipbox.exe
echo ClipBox is running. Press Alt+V.
timeout /t 3 >nul
