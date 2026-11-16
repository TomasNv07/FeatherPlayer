; Inno Setup script for Feather Player (Rust version)
;
; Easiest: run build_installer.bat in the repo root. It builds, picks the next version and
; compiles this file; the installer appears in installers\.
;
; By hand:
;   1) run build_local.bat (fills dist\FeatherPlayer\ with the exe, libmpv-2.dll, pics, ffmpeg)
;   2) change MyAppVersion below, open this file in Inno Setup and press Compile (Ctrl+F9)
;
; Installs over the Python version (same AppId, folder and exe name), so the Start menu entry
; and "Start with Windows" keep working, and removes the Python build's leftovers.

#ifndef MyAppVersion
  #define MyAppVersion "1.0.5"
#endif
#define MyAppName "Feather Player"
#define MyAppExe  "FeatherPlayer.exe"
; repo root = the folder above this file
#define Root      AddBackslash(SourcePath) + ".."
#define DistDir   Root + "\dist\FeatherPlayer"

[Setup]
; Same AppId as the Python Feather Player (and the old FeatherLauncher), so this is an upgrade
AppId={{5197C491-2AFB-41A8-83C8-61EEE5A7C4CB}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
VersionInfoVersion={#MyAppVersion}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
; Installs just for the current user, so no admin prompt (the user can still choose all-users)
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#Root}\installers
OutputBaseFilename=FeatherPlayer_Setup_{#MyAppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
SetupIconFile={#DistDir}\pics\feather1.ico
; the exe has no icon of its own (the window icon is set at runtime), so use the .ico
UninstallDisplayIcon={app}\pics\feather1.ico
; Closes a running copy (and restarts it) when updating
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[InstallDelete]
; The Python (PyInstaller) build: everything lived in _internal
Type: filesandordirs; Name: "{app}\_internal"
; Leftovers of the old FeatherLauncher name
Type: files; Name: "{app}\FeatherLauncher.exe"
Type: files; Name: "{autoprograms}\FeatherLauncher.lnk"
Type: files; Name: "{autodesktop}\FeatherLauncher.lnk"

[Files]
; FeatherPlayer.exe, libmpv-2.dll, pics\, ffmpeg\ (ffmpeg.exe + ffprobe.exe)
Source: "{#DistDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"; IconFilename: "{app}\pics\feather1.ico"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"; IconFilename: "{app}\pics\feather1.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExe}"; Description: "Launch {#MyAppName}"; Flags: nowait postinstall skipifsilent
