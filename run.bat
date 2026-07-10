@echo off
setlocal

set "ROOT=%~dp0"
set "EXE=%ROOT%target\release\wezterm-gui.exe"

if not exist "%EXE%" (
    echo Release build not found:
    echo   %EXE%
    echo Run build.bat first.
    exit /b 1
)

echo Launching WezTerm release build...
start "WezTerm" "%EXE%" %*
if errorlevel 1 goto :error
exit /b 0

:error
echo.
echo Failed to launch WezTerm. See the output above.
exit /b 1
