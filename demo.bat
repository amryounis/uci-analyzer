@echo off
rem Optional: plays two pretend sessions with a made-up engine so you can try
rem the window without a real engine. try-it.bat does NOT do this.
setlocal
cd /d "%~dp0"

where cargo >nul 2>nul
if errorlevel 1 (
  echo Rust is not installed or not on the PATH. Install it from https://rustup.rs
  pause
  exit /b 1
)

cargo build --release
if errorlevel 1 (
  echo.
  echo The build failed. Copy the messages above into an issue on GitHub.
  pause
  exit /b 1
)

target\release\uci-demo.exe
echo.
pause
