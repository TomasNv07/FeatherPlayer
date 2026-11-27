# Feather Player

Feather Player is a Windows desktop app for browsing and watching your personal video clips. It uses the mpv player for playback and FFmpeg for thumbnails and clip tools, and runs quietly in the system tray when you close the window.

This is the Rust version of Feather Player. It looks and works like the earlier Python version and uses the same settings, so an existing install upgrades in place.


<img width="1161" height="701" alt="Feather Player main window" src="https://github.com/user-attachments/assets/11eb2f13-4c64-48c4-8468-8ebcf6d2908a" />


## Features

- Plays your video clips using mpv, including clips with several audio tracks (game, mic, ...) that you can pick and mix
- Thumbnail list of your clips, sorted by date or size, with filters for starred, liked and trimmed clips, a game filter and a calendar
- Trim clips, compress them to 10, 20 or 50 MB, and share them to Discord with one click
- Optional copy of new clips to a cloud folder (for example Google Drive)
- Fullscreen mode with its own controls and a clip list that slides in from the right
- Runs in the system tray, so it stays out of your way
- Optional start with Windows
- Tells you in Settings when a new version is out and updates itself
- Only one copy of the app runs at a time

## Download and install

1. Go to the [Releases](../../releases) page.
2. Download the latest `FeatherPlayer_Setup_x.x.x.exe` file from the Assets section.
3. Run the installer and follow the steps.
4. Start Feather Player from the Start menu.

By default the app installs for your user account only, so it does not need administrator rights. During setup you can choose to create a desktop shortcut.

## Requirements

- Windows 10 or Windows 11, 64-bit
- No other software needs to be installed. The mpv player and FFmpeg are included.

## Windows SmartScreen warning

The installer is not code-signed, so Windows may show a "Windows protected your PC" message the first time you run it. This is normal for small independent projects. To continue, click "More info", then "Run anyway".

If you prefer, you can check the file with your antivirus before running it.

## Updating

When a new version is released, Feather Player shows an "Update available" box at the top of Settings, and the Settings button gets a small "!". Click "Update now": the update installs in the background and Feather Player restarts by itself. Your clips and settings stay as they are.

You can also download the newer installer from the Releases page and run it. It installs over the old version and closes the running copy if needed.

## Uninstalling

Open Windows Settings, go to Apps, find Feather Player, and choose Uninstall.

## Feedback and bugs

Found a problem or have an idea? Open an issue on the [Issues](../../issues) tab and describe what happened and what version you are using.

## Building from source

Needs Rust 1.88 or newer and the Visual Studio C++ Build Tools.

    cargo build --release

The app loads `libmpv-2.dll` at runtime from the `FEATHER_LIBMPV` environment variable, from next to the exe, or from `%USERPROFILE%\FeatherPlayer\`. FFmpeg is found in an `ffmpeg\` folder next to the exe or in the same data folder. Project notes are in [CLAUDE.md](CLAUDE.md).

## Credits

Feather Player includes the following third-party software. Each is a separate project with its own license, and those licenses apply to those components.

- [mpv](https://mpv.io) (`libmpv-2.dll`) is used for video playback and is included with the installer.
- [FFmpeg](https://ffmpeg.org) (`ffmpeg.exe` and `ffprobe.exe`) is used for thumbnails and video processing and is included with the installer. The included build is from [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) and is licensed under the GNU GPL v3. FFmpeg's source code is available at <https://ffmpeg.org/download.html>. FFmpeg is a trademark of Fabrice Bellard, originator of the FFmpeg project.
- [egui / eframe](https://github.com/emilk/egui), [winit](https://github.com/rust-windowing/winit) and other open-source Rust libraries (MIT / Apache-2.0) are used to build the app.

## License

See the [LICENSE](LICENSE) file for terms.
