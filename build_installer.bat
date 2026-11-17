@echo off
rem Builds Feather Player and makes the installer: installers\FeatherPlayer_Setup_<version>.exe
rem Usage: build_installer.bat            -> next version after the newest installer (x.y.z+1)
rem        build_installer.bat 2.0.0      -> that version
rem Needs Inno Setup (found through the registry; or set ISCC=path\to\ISCC.exe first).
setlocal
cd /d "%~dp0"

call "%~dp0build_local.bat" nopause
if errorlevel 1 goto :fail
rem an installer without ffmpeg would ship an app without thumbnails / trim / compress / share
if not exist "dist\FeatherPlayer\ffmpeg\ffmpeg.exe" (
    echo ERROR: ffmpeg.exe was not copied ^(needs %USERPROFILE%\FeatherPlayer\ffmpeg\ffmpeg.exe^).
    goto :fail
)

set "VER=%~1"
if "%VER%"=="" (
    for /f "usebackq delims=" %%v in (`powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup\next_version.ps1"`) do set "VER=%%v"
)
if "%VER%"=="" (
    echo ERROR: could not work out the version.
    goto :fail
)

if not defined ISCC (
    for /f "usebackq delims=" %%i in (`powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup\find_iscc.ps1"`) do set "ISCC=%%i"
)
if not defined ISCC (
    echo ERROR: Inno Setup ^(ISCC.exe^) not found. Install it, or set ISCC=path\to\ISCC.exe
    goto :fail
)

if exist "installers\FeatherPlayer_Setup_%VER%.exe" (
    echo ERROR: installers\FeatherPlayer_Setup_%VER%.exe already exists. Pass another version.
    goto :fail
)

echo.
echo Making installer version %VER% with "%ISCC%" ...
"%ISCC%" /Q /DMyAppVersion=%VER% "%~dp0setup\FeatherPlayer.iss"
if errorlevel 1 goto :fail

echo.
echo Done! installers\FeatherPlayer_Setup_%VER%.exe
pause
exit /b 0

:fail
echo.
echo INSTALLER BUILD FAILED
pause
exit /b 1
