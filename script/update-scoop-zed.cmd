@echo off
pwsh.exe -NoLogo -NoProfile -File "%~dp0update-scoop-zed.ps1" %*
if errorlevel 1 (
    pause
    exit /b 1
)
