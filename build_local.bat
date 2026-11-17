@echo off
rem Builds Feather Player (Rust) and puts everything it needs into dist\FeatherPlayer\:
rem   FeatherPlayer.exe, libmpv-2.dll, pics\, ffmpeg\ (ffmpeg.exe + ffprobe.exe)
rem libmpv-2.dll and ffmpeg are copied from %USERPROFILE%\FeatherPlayer (only read, never changed).
rem pics come from the repo's pics\ folder.
rem Usage: build_local.bat [nopause]
setlocal
cd /d "%~dp0"
set "DATA=%USERPROFILE%\FeatherPlayer"
set "DIST=%~dp0dist\FeatherPlayer"

where cargo >nul 2>nul || set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

echo Building featherplayer-rs (release)...
cargo build --release
if errorlevel 1 goto :fail

echo Copying files to %DIST% ...
if exist "%DIST%" rmdir /s /q "%DIST%"
mkdir "%DIST%" || goto :fail
copy /y "target\release\featherplayer-rs.exe" "%DIST%\FeatherPlayer.exe" >nul || goto :fail

if not exist "%DATA%\libmpv-2.dll" (
    echo ERROR: %DATA%\libmpv-2.dll not found - the app cannot play video without it.
    goto :fail
)
copy /y "%DATA%\libmpv-2.dll" "%DIST%\" >nul || goto :fail

rem pics\ is in the repo (feather_app.ico = installer + exe icon, made by setup\make_icon.ps1)
if not exist "pics\feather_app.ico" (
    echo ERROR: pics\feather_app.ico not found - git pull, or run setup\make_icon.ps1.
    goto :fail
)
if not exist "pics\feather.png" echo WARNING: pics\feather.png missing - no logo in the title bar.
xcopy /e /i /y /q "pics" "%DIST%\pics" >nul || goto :fail

if exist "%DATA%\ffmpeg\ffmpeg.exe" (
    mkdir "%DIST%\ffmpeg"
    copy /y "%DATA%\ffmpeg\ffmpeg.exe" "%DIST%\ffmpeg\" >nul || goto :fail
    if exist "%DATA%\ffmpeg\ffprobe.exe" copy /y "%DATA%\ffmpeg\ffprobe.exe" "%DIST%\ffmpeg\" >nul
) else (
    echo WARNING: %DATA%\ffmpeg\ffmpeg.exe not found - no thumbnails, trim, compress or share.
)

echo.
echo Done! The app is in %DIST%
if /i not "%~1"=="nopause" pause
exit /b 0

:fail
echo.
echo BUILD FAILED
if /i not "%~1"=="nopause" pause
exit /b 1
