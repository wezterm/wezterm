@echo off
setlocal

set "ROOT=%~dp0"
set "WEZTERM_EXE=%ROOT%target\release\wezterm-gui.exe"
set "CONFIG_EXE=%ROOT%target\release\wezterm-config.exe"
set "LOCAL_PERL_DIR=%LOCALAPPDATA%\WezTerm\build-tools\strawberry-perl-5.34.3.1\perl\bin"

cd /d "%ROOT%" || goto :error

if defined WEZTERM_PERL_DIR (
    set "PATH=%WEZTERM_PERL_DIR%;%PATH%"
) else if exist "C:\Strawberry\perl\bin\perl.exe" (
    set "PATH=C:\Strawberry\perl\bin;%PATH%"
) else if exist "%LOCAL_PERL_DIR%\perl.exe" (
    set "PATH=%LOCAL_PERL_DIR%;%PATH%"
)

call :check_perl
if not errorlevel 1 goto :perl_ready

echo Preparing a local Strawberry Perl build dependency...
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%ROOT%ci\ensure-strawberry-perl.ps1" || goto :error
set "PATH=%LOCAL_PERL_DIR%;%PATH%"

call :check_perl
if errorlevel 1 (
    echo The local Strawberry Perl installation is invalid:
    echo   %LOCAL_PERL_DIR%
    goto :error
)

:perl_ready

echo Building WezTerm in release mode...
call cargo build --release --locked || goto :error

echo.
echo Building WezTerm Config in release mode...
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%ROOT%ci\build-wezterm-config.ps1" || goto :error

if not exist "%WEZTERM_EXE%" (
    echo Expected executable was not found:
    echo   %WEZTERM_EXE%
    goto :error
)

if not exist "%CONFIG_EXE%" (
    echo Expected executable was not found:
    echo   %CONFIG_EXE%
    goto :error
)

echo.
echo Release build completed:
echo   %WEZTERM_EXE%
echo   %CONFIG_EXE%
exit /b 0

:error
echo.
echo Build failed. See the output above.
exit /b 1

:check_perl
perl -V:osname 2>nul | findstr /C:"MSWin32" >nul
if errorlevel 1 exit /b 1
perl -MFindBin -e "exit 0" >nul 2>nul
exit /b %ERRORLEVEL%
