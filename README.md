# FeatherPlayer

Rust port of FeatherPlayer (Python/tkinter clip player built on mpv). Work in progress.

Right now this is the proof of concept: mpv renders inside an egui window, widgets draw on top of the video, multi-audio-track mixing works (same `lavfi-complex` / `amix` rules as the Python app), and the seek bar jumps exactly where you click.

## Build & run (Windows)

    cargo build --release
    target\release\featherplayer.exe "C:\path\to\clip.mkv"

Start with no argument and drag a video onto the window, or use the Open... button.
libmpv is loaded at runtime from: `FEATHER_LIBMPV` env var, next to the exe,
`%USERPROFILE%\FeatherPlayer\libmpv-2.dll`, then the normal DLL search path. `--lib <path>` also works.

Needs Rust 1.88+ and the Visual Studio C++ Build Tools.

## Keys
Space play/pause, Left/Right +-5 s, Up/Down volume, F fullscreen, click video = play/pause, double-click = fullscreen.

See `CLAUDE.md` for project notes and the porting plan.
