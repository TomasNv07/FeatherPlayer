# FeatherPlayer-rs: project notes

Port of FeatherPlayer (`reference/FeatherPlayer.py`, ~11.8k lines, one `VideoPlayerApp` class, Windows-only, fully custom-drawn tkinter UI) to Rust. The owner is Tomas; he works on two Windows PCs and syncs through this git repo, so everything important must live in the repo (this file, commit messages), not in chat history.

## Status
The proof of concept in `src/` is done and tested by the owner on Windows: video plays inside the egui window, speeds, audio track picking/mixing and playback all work. Nothing else is ported yet.

## Architecture decisions (keep unless there is a good reason)
- **GUI:** eframe/egui 0.33 with the **glow** (OpenGL) renderer. Needs Rust 1.88+.
- **mpv:** `src/mpv.rs` loads libmpv at runtime with `libloading` (no import library, no linking setup). It uses the **OpenGL render API**, not a child window (`wid`), so egui popups and menus can draw over the video (no airspace problem).
- **Video drawing:** mpv can only render at the origin of a framebuffer, so it renders into an off-screen texture/FBO sized to the video rectangle, then that is blitted into the egui paint-callback rectangle (`VideoGl` in `main.rs`). mpv's update callback calls `egui::Context::request_repaint`.
- **Shutdown order matters:** free the render context (GL thread, context current), then `mpv_terminate_destroy`. This is done in `on_exit`.
- **Seek bar:** custom widget that jumps to the pressed position (also next to the thumb) and follows the pointer on drag. A short hold after seeking stops the position poll from snapping the thumb back.
- **Audio tracks:** same rules as FeatherPlayer `_at_build_graph`. One untouched track is picked with `aid`; otherwise a `lavfi-complex` graph is built (`[aidN]volume=V[vK]`, then `amix=inputs=N:duration=longest,volume=N[ao]`, since amix divides the level by N). Set `lavfi-complex` only when it changes (each set rebuilds mpv's filter graph). After applying, seek back to the old position (`absolute+exact`), because adding a track makes playback jump ahead.
- `libmpv-2.dll` is NOT in the repo (`.gitignore` excludes dlls/exes). It is found in `%USERPROFILE%\FeatherPlayer\libmpv-2.dll`, the same place the Python app uses.

## Rough porting order
1. Project structure: split `main.rs` into modules (player, ui widgets, theme, prefs, scanner).
2. Prefs/settings (JSON with backups) and the data folder (`%USERPROFILE%\FeatherPlayer`).
3. Clip folder scanning (background threads, `walkdir`/`rayon`) and thumbnails (ffmpeg subprocess).
4. Clip grid (virtualized) with relative-time labels, theme ("Vibrant Neon"), custom widgets.
5. Categories/keywords (careful: the Python version had a bug that destructively deleted keyword groups).
6. Trim and lossless clip (`ffmpeg -c copy`), multi-clip compression (NVENC with fallback).
7. Cloud-sync star, phone/ClipSync sync, recycle bin (`trash` crate).
8. Frameless window and DWM dark title bar (`winit` + `windows` crate), tray icon, startup registry entry, single-instance socket.
9. Packaging: single exe plus Inno Setup installer, bundling `libmpv-2.dll` and `ffmpeg.exe`.

Port feature by feature, reading the matching part of `reference/FeatherPlayer.py` each time. Keep behavior identical unless the owner says otherwise.

## Working conventions
- Owner is concise and direct; keep answers short. He switches between English and Finnish.
- Build with `cargo build --release`. Always make sure it compiles before committing.
- Commit small, with clear messages, and push at the end of each session so the other PC can `git pull`.
- Do not commit binaries (libmpv, ffmpeg, exes) or video clips.

## Two-PC setup (each PC, once)
Rust (rustup) + Visual Studio C++ Build Tools, Claude Code logged in to the same account, `libmpv-2.dll` in `%USERPROFILE%\FeatherPlayer\`, ffmpeg later. Then `git clone` / `git pull` this repo.
