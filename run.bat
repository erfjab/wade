@echo off
cd /d "%~dp0"
taskkill /im wade.exe /f >/dev/null 2>&1
echo Building Wade...
cargo build > build.log 2>&1
if errorlevel 1 (
  type build.log
  echo.
  echo BUILD FAILED - see build.log
  pause
  exit /b 1
)
start "" target\debug\wade.exe
echo Wade is running. Press Alt+Tab.
timeout /t 3 >nul
