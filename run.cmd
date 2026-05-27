@echo off
set "PATH=%USERPROFILE%\.cargo\bin;%USERPROFILE%\.bun\bin;C:\Program Files\Git\bin;%PATH%"
cd /d "%~dp0app"
npm run tauri:dev
