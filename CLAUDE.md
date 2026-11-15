# FeatherPlayer-rs: project notes

Port of FeatherPlayer (`reference/FeatherPlayer.py`, ~11.8k lines, one `VideoPlayerApp` class, Windows-only, fully custom-drawn tkinter UI) to Rust. The owner is Tomas; he works on two Windows PCs and syncs through this git repo, so everything important must live in the repo (this file, commit messages), not in chat history.

## Context from the owner
- Renamed: FeatherLauncher2 is now FeatherPlayer. Python keeps data in `%USERPROFILE%\FeatherPlayer` (it migrates the old FeatherLauncher2 folder itself). Rust must use the same folder and stay compatible with the existing settings, prefs and thumbnails.
- Windows only. Full port with identical behavior, not a redesign. **Ask the owner before changing how a feature works.**
- Distributed to friends as a PyInstaller build plus an Inno Setup installer, deliberately not via GitHub. Rust plan: single exe plus installer, bundling `libmpv-2.dll` and `ffmpeg.exe`. The Python `build.bat` must never be shared with end users.
- Owner's clips: `.mkv`, 1080p h264, several audio tracks (game, mic, ...), so multi-track mixing must keep working. Test clips: `Z:\All clips\Clips`.
- ClipSync is a separate Android app that plays clips copied to a phone. FeatherPlayer's phone sync feature for it gets ported later.
- Two Windows PCs, synced only through this repo: `git pull` at the start of every session, commit and push at the end. Anything important goes in CLAUDE.md or commit messages, since chat history does not carry over.
- Owner writes English and Finnish. Keep answers short and direct, and explain compile errors briefly.
- `libmpv-2.dll` and `ffmpeg.exe` are not committed. libmpv lives in `%USERPROFILE%\FeatherPlayer\`.

## Status
The proof of concept in `src/` is done and tested by the owner on Windows: video plays inside the egui window, speeds, audio track picking/mixing and playback all work. Nothing else is ported yet.

Steps 1-4 done (step 4 partly, see below). Layout:
- `main.rs`: arg parsing (`[--lib path] [file]`), `run_native`.
- `app.rs`: `FeatherApp` shell: owns `Player`, `Prefs`, `Library`, `Scanner` and the views; handles scan/thumbnail messages, folder actions, playing a clip (`play` = Python `_play_index`), prefs saving; shutdown in `on_exit`.
- `mpv.rs`: libmpv loader/wrapper.
- `player/`: `mod.rs` (`Player`: open, play/pause, seek, volume, speed, poll), `audio.rs` (tracks, `build_graph`, `apply_audio`), `video_gl.rs` (FBO + paint callback).
- `library.rs`: the clip list (`videos` sorted in place like Python, per-clip date/MB/"N days ago" labels, sort Newest/Oldest/Largest/Smallest, filter All/Starred/Liked/Trimmed, `visible_count` 50 + "Load more" step from settings `see_more_step`, current clip, right-click selection).
- `ui/`: `widgets.rs` (Pill, GhostButton, RoundedPopup/PopupRow, ellipsize, seek bar, play button), `icons.rs` (Python `_icon_image` vector icons via tiny-skia, cached textures, shadowed variant), `clip_grid.rs` (sidebar: filter/sort pill + virtualized single-column card list, heart toggles like, right-drag selects), `header.rs` ("ALL CLIPS" + count + size, Folder pill with saved-folders dropdown, Refresh), `player_view.rs` (video, now-playing row, PoC controls in theme colours, audio overlay), `fmt_time`.
- `theme.rs`: full port of the Python theme engine (8 themes, default "Obsidian", `accent_overrides`, colorsys maths). `c("#rrggbb")` = Python `_c()`. Verified: all 152 themed colours equal the Python output. Fonts: Segoe UI from `C:\Windows\Fonts` at tk point sizes (9pt = 12px).
- `prefs.rs` (step 2 done): `data_dir()` (with FeatherLauncher2 migration), `settings.json` (`read_settings`/`save_setting`, pretty JSON, tmp+rename), `Prefs` for `.clipplayer_prefs.json` (backup to `backups\prefs_*.json` before each save, keep 30, refuse accidental wipes, fall back to newest backup). Unknown/untyped keys are kept on save (Python drops unknown keys). `durations`, `trims` (floats 0..10000), `category_renames`, `keyword_groups` are still raw JSON: type them in their porting step. Volume is loaded from prefs (default 55) and prefs are saved on exit, like Python `on_close`. `cargo test -- --ignored real_prefs` checks the real file round-trips (read-only).
- `scanner.rs` (step 3 done): background folder scan (folder + one subfolder level, Python path format `base + "\\" + name`, filename dates), `find_ffmpeg` (exe dir `ffmpeg\`, data dir `ffmpeg\`, PATH, C:\ffmpeg), ffprobe durations into prefs `durations`, thumbnails in `.clipplayer_cache` with the SAME md5 cache key as Python (verified: existing thumbnails are reused), 4 worker threads with new-clips-first priority. `game_name_from_filename` for the now-playing label.
- Paths from folder dialogs are stored with forward slashes like Tk (`Z:/All clips/Clips`), because folder strings are part of every clip key in prefs.
- Not ported yet from the main window: the full player controls (frame step, star/heart buttons, trim slider, speed and audio menus), game/category filter and calendar (step 5), the other header actions (delete, categorize, show in folder, share, compress, settings), automatic folder refresh (Python checks every 4 s), the 16:9 sidebar auto-fit, saved window size. Durations are saved at most every 2 s (Python saved prefs after every single one).

## Architecture decisions (keep unless there is a good reason)
- **GUI:** eframe/egui 0.33 with the **glow** (OpenGL) renderer. Needs Rust 1.88+.
- **mpv:** `src/mpv.rs` loads libmpv at runtime with `libloading` (no import library, no linking setup). It uses the **OpenGL render API**, not a child window (`wid`), so egui popups and menus can draw over the video (no airspace problem).
- **Video drawing:** mpv can only render at the origin of a framebuffer, so it renders into an off-screen texture/FBO sized to the video rectangle, then that is blitted into the egui paint-callback rectangle (`VideoGl` in `player/video_gl.rs`). mpv's update callback calls `egui::Context::request_repaint`.
- **Shutdown order matters:** free the render context (GL thread, context current), then `mpv_terminate_destroy`. This is done in `on_exit`.
- **Seek bar:** custom widget that jumps to the pressed position (also next to the thumb) and follows the pointer on drag. A short hold after seeking stops the position poll from snapping the thumb back.
- **Audio tracks:** same rules as FeatherPlayer `_at_build_graph`. One untouched track is picked with `aid`; otherwise a `lavfi-complex` graph is built (`[aidN]volume=V[vK]`, then `amix=inputs=N:duration=longest,volume=N[ao]`, since amix divides the level by N). Set `lavfi-complex` only when it changes (each set rebuilds mpv's filter graph). After applying, seek back to the old position (`absolute+exact`), because adding a track makes playback jump ahead.
- `libmpv-2.dll` is NOT in the repo (`.gitignore` excludes dlls/exes). It is found in `%USERPROFILE%\FeatherPlayer\libmpv-2.dll`, the same place the Python app uses.

## Rough porting order
1. Project structure: split `main.rs` into modules (player, ui widgets, theme, prefs, scanner).
2. Prefs/settings (JSON with backups) and the data folder (`%USERPROFILE%\FeatherPlayer`).
3. Clip folder scanning (background threads) and thumbnails (ffmpeg subprocess).
4. Clip grid (virtualized) with relative-time labels, theme engine (Python has 8 themes, default "Obsidian"; there is no "Vibrant Neon" theme in the code), custom widgets.
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
