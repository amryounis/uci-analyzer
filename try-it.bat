@echo off
rem Builds everything and opens the analyzer. Nothing is started for you:
rem press "Load engine..." in the window to pick an engine, or point your
rem chess program at uci-shim.exe. Close this console window to stop it all.
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

echo.
echo Opening the analyzer. Close this window to stop it (engines you loaded stop too).
target\release\uci-viewer.exe
echo.
pause
