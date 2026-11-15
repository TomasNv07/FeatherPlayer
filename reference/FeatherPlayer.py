import tkinter as tk
from tkinter import filedialog
import os
import threading
import queue
import time
import subprocess
import hashlib
import datetime
import re
import json
import math as _math
import concurrent.futures
import ctypes
import ctypes.wintypes
import socket
import shutil

# ── Step 3: crash log ─────────────────────────────────────────────────────────
# A native crash (access violation etc.) kills Python without any traceback.
# faulthandler writes the Python stack of every thread to this file when that
# happens, so we can see exactly which line was running.
import faulthandler
import sys as _sys
_CRASH_LOG_PATH = os.path.join(os.path.expanduser("~"), "Downloads", "crash.txt")
try:
    os.makedirs(os.path.dirname(_CRASH_LOG_PATH), exist_ok=True)
    _crash_log_file = open(_CRASH_LOG_PATH, "a", buffering=1, encoding="utf-8")
    _crash_log_file.write("\n=== started %s ===\n" % datetime.datetime.now().isoformat(timespec="seconds"))
    pass
except Exception:
    pass

# ── Minimize trace ────────────────────────────────────────────────────────────
# Records every window message (and video-area resize) for ~8 s after the
# minimize button is pressed, so the last line shows what happened right
# before a crash. File: Downloads\minimize_trace.txt
_TRACE_PATH = os.path.join(os.path.expanduser("~"), "Downloads", "minimize_trace.txt")
_trace_until = 0.0

def _trace(msg):
    try:
        if time.monotonic() > _trace_until:
            return
        with open(_TRACE_PATH, "a", encoding="utf-8") as f:
            f.write("%.3f %s\n" % (time.monotonic(), msg))
    except Exception:
        pass

# ── libmpv DLL location ────────────────────────────────────────────────────
# python-mpv loads libmpv via ctypes under the hood, which on Windows needs
# the DLL's folder to be discoverable. We add it to PATH and (Python 3.8+)
# register it with os.add_dll_directory, plus point python-mpv's own
# override env var straight at the exact file, so lookup can't fail
# regardless of which mechanism it uses internally.
# ── App data folder ────────────────────────────────────────────────────────
# The app was renamed FeatherLauncher -> Feather Player, and its folder from
# ~/FeatherLauncher2 to ~/FeatherPlayer. On first launch the old folder is
# moved to the new name (settings, prefs, backups, thumbnails, libmpv...). If it
# can't be moved (e.g. it is in use), the old folder keeps being used so
# nothing is ever lost or started from scratch.
def _resolve_data_dir():
    home = os.path.expanduser("~")
    new  = os.path.join(home, "FeatherPlayer")
    old  = os.path.join(home, "FeatherLauncher2")
    if not os.path.exists(new) and os.path.isdir(old):
        try:
            os.rename(old, new)
        except Exception:
            return old
    return new

_DATA_DIR = _resolve_data_dir()

LIBMPV_DLL_PATH = os.path.join(_DATA_DIR, "libmpv-2.dll")
if os.path.exists(LIBMPV_DLL_PATH):
    _libmpv_dir = os.path.dirname(LIBMPV_DLL_PATH)
    os.environ["PATH"] = _libmpv_dir + os.pathsep + os.environ.get("PATH", "")
    if hasattr(os, "add_dll_directory"):
        try:
            os.add_dll_directory(_libmpv_dir)
        except Exception:
            pass
    os.environ.setdefault("MPV_DYLIB_PATH", LIBMPV_DLL_PATH)

import mpv as _mpv_mod

# ── Win32 constants for embedding ─────────────────────────────────────────────
user32 = ctypes.windll.user32
SWP_NOACTIVATE = 0x0010
SWP_SHOWWINDOW = 0x0040

# ── Theme engine ──────────────────────────────────────────────────────────────
# Every colour literal in this file is the original "Midnight" palette wrapped in
# _c("#rrggbb"). _c() translates it through the active theme, so all themes are
# generated from the same base colours and switching is live (see
# VideoPlayerApp._retheme_widgets). To add a theme, add one entry to THEMES.
import colorsys

_SETTINGS_PATH = os.path.join(_DATA_DIR, "settings.json")


# ── Start with Windows (per-user Run key; no admin needed) ───────────────────
_STARTUP_KEY  = r"Software\Microsoft\Windows\CurrentVersion\Run"
_STARTUP_NAME = "Feather Player"
_STARTUP_LEGACY_NAME = "Clips"      # name used before the rename

def _startup_command():
    """Command Windows runs at login. --minimized makes the app start in the tray."""
    if getattr(sys, "frozen", False):
        return f'"{sys.executable}" --minimized'
    exe = sys.executable
    pyw = os.path.join(os.path.dirname(exe), "pythonw.exe")      # no console window
    if os.path.isfile(pyw):
        exe = pyw
    return f'"{exe}" "{os.path.abspath(__file__)}" --minimized'

def _refresh_startup_entry():
    """After an update/rename: if "Start with Windows" is on (under the new or the
    old "Clips" name), re-point it at the current exe under the new name.
    Only for the installed exe, so running the script never overwrites it."""
    if not getattr(sys, "frozen", False):
        return
    try:
        import winreg
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, _STARTUP_KEY, 0,
                            winreg.KEY_SET_VALUE | winreg.KEY_QUERY_VALUE) as k:
            was_on = False
            for name in (_STARTUP_NAME, _STARTUP_LEGACY_NAME):
                try:
                    winreg.QueryValueEx(k, name)
                    was_on = True
                except FileNotFoundError:
                    pass
            try:
                winreg.DeleteValue(k, _STARTUP_LEGACY_NAME)
            except FileNotFoundError:
                pass
            if was_on:
                winreg.SetValueEx(k, _STARTUP_NAME, 0, winreg.REG_SZ, _startup_command())
    except Exception:
        pass

def _startup_enabled():
    try:
        import winreg
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, _STARTUP_KEY) as k:
            winreg.QueryValueEx(k, _STARTUP_NAME)
            return True
    except Exception:
        return False

def _set_startup(on):
    """Add / remove the Run-key entry. Returns True on success."""
    try:
        import winreg
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, _STARTUP_KEY, 0, winreg.KEY_SET_VALUE) as k:
            if on:
                winreg.SetValueEx(k, _STARTUP_NAME, 0, winreg.REG_SZ, _startup_command())
            else:
                try:
                    winreg.DeleteValue(k, _STARTUP_NAME)
                except FileNotFoundError:
                    pass
        return True
    except Exception:
        return False


def _read_settings():
    try:
        with open(_SETTINGS_PATH, "r", encoding="utf-8") as f:
            d = json.load(f)
        return d if isinstance(d, dict) else {}
    except Exception:
        return {}


def _write_settings(d):
    """Settings live in their own small file so they can never interfere with
    the clip prefs file (starred / liked / trims / ...)."""
    try:
        os.makedirs(os.path.dirname(_SETTINGS_PATH), exist_ok=True)
        tmp = _SETTINGS_PATH + ".tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(d, f, indent=2)
        os.replace(tmp, _SETTINGS_PATH)
    except Exception:
        pass


def _load_accent_overrides():
    """{theme name: '#rrggbb'} custom accents saved in settings.json."""
    raw = _read_settings().get("accent_overrides")
    out = {}
    if isinstance(raw, dict):
        for k, v in raw.items():
            if isinstance(v, str) and re.fullmatch(r"#[0-9a-fA-F]{6}", v):
                out[k] = v.lower()
    return out


_ACCENT_OVERRIDES = _load_accent_overrides()


def _accent_override(name):
    """The user's custom accent for this theme, or None (= use the theme's own)."""
    v = _ACCENT_OVERRIDES.get(name)
    return v.lower() if isinstance(v, str) else None


# hue: 0-360 · sat: scales the base colours' saturation · l_scale / l_shift:
# adjust surface lightness · accent: the highlight colour (buttons, sliders, ...)
THEMES = {
    "Midnight": dict(identity=True, accent="#FF2A6D"),                       # original look
    "Graphite": dict(hue=0,   sat=0.0,  l_shift=0.010, accent="#E8B923"),    # neutral grey + amber
    "Ocean":    dict(hue=214, sat=1.05, accent="#2F8CFF"),                   # deep blue
    "Forest":   dict(hue=152, sat=0.90, accent="#2FD28A"),                   # dark green + mint
    "Ember":    dict(hue=18,  sat=0.75, accent="#F2662B"),                   # warm dark + orange
    "Amethyst": dict(hue=268, sat=1.05, accent="#A06BFF"),                   # purple
    "Obsidian": dict(hue=0,   sat=0.0,  l_scale=0.85, l_shift=-0.050, accent="#00C9A7"),  # near-black
    "Mocha":    dict(hue=28,  sat=0.55, accent="#D4A373"),                   # dark coffee brown + latte tan
}
THEME_ORDER = ["Obsidian", "Midnight", "Graphite", "Ocean", "Forest", "Ember", "Amethyst", "Mocha"]

_BASE_SURFACES = ["#0f0f1a", "#13131f", "#1a1a2e", "#242444", "#2a2a4a", "#2c2c50",
                  "#161628", "#33335a", "#3a3a5e", "#45456e", "#52527e", "#2a2a3d",
                  "#23233a", "#12121e"]
_BASE_TEXT = ["#ababc2", "#8b8b9e"]
_BASE_ACCENT, _BASE_ACCENT_HOVER, _BASE_ACCENT_PRESS = "#ff2a6d", "#ff5c8d", "#cc1f57"
# Colours that are deliberately NOT themed (status reds/greens, pure black/white,
# neutral greys). Themed colours are never allowed to equal one of these.
_RESERVED_COLORS = {"#000000", "#ffffff", "#111111", "#333333", "#bbbbbb", "#dddddd",
                    "#e0e0e0", "#f1f1f1", "#e05555", "#e0685f", "#e0a05f", "#7ec97e",
                    "#c8c864", "#ccc364", "#e8454a", "#e8c84a", "#9e2a2a", "#7a1f1f",
                    "#0a1a18"}


def _hex_to_rgb(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) / 255.0 for i in (0, 2, 4))


def _rgb_to_hex(r, g, b):
    return "#%02x%02x%02x" % tuple(max(0, min(255, int(round(v * 255)))) for v in (r, g, b))


def _clamp01(v):
    return max(0.0, min(1.0, v))


def _make_unique(used):
    """Live switching translates old->new colour by colour, so two base
    colours must never end up as the same value, nor equal a fixed (unthemed)
    colour. This returns a function that nudges a colour by one step until
    that holds (and records it in `used`)."""
    def unique(hexv):
        hexv = hexv.lower()
        r, g, b = (int(hexv[i:i + 2], 16) for i in (1, 3, 5))
        step = 1
        for _ in range(600):                    # (guard: can never loop forever)
            if hexv not in used and hexv not in _RESERVED_COLORS:
                break
            if max(r, g, b) >= 255:             # can't get any brighter -> go darker
                step = -1
            r, g, b = (max(0, min(255, c + step)) for c in (r, g, b))
            hexv = "#%02x%02x%02x" % (r, g, b)
        used.add(hexv)
        return hexv
    return unique


def _set_accent_trio(m, acc, unique):
    """Fill the accent / hover / pressed entries of a theme map from one colour."""
    h, l, s = colorsys.rgb_to_hls(*_hex_to_rgb(acc))
    m[_BASE_ACCENT] = unique(acc)
    m[_BASE_ACCENT_HOVER] = unique(_rgb_to_hex(*colorsys.hls_to_rgb(h, min(l + 0.09, 0.88), s)))
    m[_BASE_ACCENT_PRESS] = unique(_rgb_to_hex(*colorsys.hls_to_rgb(h, max(l - 0.10, 0.10), s)))


def _build_theme_map(name):
    """{base colour -> themed colour} for every themeable colour."""
    t = THEMES.get(name) or THEMES["Midnight"]
    ov = _accent_override(name)            # user's own accent for this theme, if any
    m = {}
    if t.get("identity"):
        for c in _BASE_SURFACES + _BASE_TEXT + [_BASE_ACCENT, _BASE_ACCENT_HOVER, _BASE_ACCENT_PRESS]:
            m[c] = c
        if ov:
            _set_accent_trio(m, ov, _make_unique(set(_BASE_SURFACES + _BASE_TEXT)))
        return m
    hue = t["hue"] / 360.0
    sat = t["sat"]
    ls, lsh = t.get("l_scale", 1.0), t.get("l_shift", 0.0)
    unique = _make_unique(set())

    for c in _BASE_SURFACES:
        h, l, s = colorsys.rgb_to_hls(*_hex_to_rgb(c))
        m[c] = unique(_rgb_to_hex(*colorsys.hls_to_rgb(hue, _clamp01(l * ls + lsh), _clamp01(s * sat))))
    for c in _BASE_TEXT:                       # text keeps its lightness, just re-tinted
        h, l, s = colorsys.rgb_to_hls(*_hex_to_rgb(c))
        m[c] = unique(_rgb_to_hex(*colorsys.hls_to_rgb(hue, l, _clamp01(s * sat))))
    _set_accent_trio(m, ov or t["accent"].lower(), unique)
    return m


def _theme_swatch_colors(name):
    m = _build_theme_map(name)
    return m["#0f0f1a"], m["#242444"], m[_BASE_ACCENT]


def _accent_fg(acc):
    """Readable text colour to put on top of the accent colour."""
    def lin(v):
        return v / 12.92 if v <= 0.03928 else ((v + 0.055) / 1.055) ** 2.4
    r, g, b = _hex_to_rgb(acc)
    lum = 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    return "#111111" if lum > 0.30 else "#ffffff"


_CUR_THEME = _read_settings().get("theme", "Obsidian")
if _CUR_THEME not in THEMES:
    _CUR_THEME = "Obsidian"
_CUR_MAP = _build_theme_map(_CUR_THEME)


def _c(hexcolor):
    """Translate a base-palette colour through the active theme."""
    try:
        return _CUR_MAP.get(hexcolor.lower(), hexcolor)
    except AttributeError:
        return hexcolor


# ── Palette ───────────────────────────────────────────────────────────────────
BG           = _c("#0F0F1A")
SIDEBAR_BG   = _c("#13131f")
CARD_BG      = _c("#1A1A2E")
CARD_HOVER   = _c("#242444")
ACCENT       = _c("#FF2A6D")
TEXT_PRIMARY = _c("#F1F1F1")
TEXT_MUTED   = _c("#ABABC2")
TEXT_DIM     = _c("#8B8B9E")
PLAYER_BG    = _c("#000000")
BAR_TRACK    = _c("#2A2A4A")
BAR_HANDLE   = _c("#FF2A6D")
ACCENT_FG    = _accent_fg(ACCENT)   # text colour on top of ACCENT

FONT_UI    = ("Segoe UI", 9)
FONT_BOLD  = ("Segoe UI", 9, "bold")
FONT_TITLE = ("Segoe UI", 11, "bold")
FONT_TINY  = ("Segoe UI", 8)

# ── Design tokens (keep every control the same height / radius / border) ──────
DIVIDER    = _c("#2A2A3D")
BORDER     = _c("#2A2A3D")
CTRL_H     = 30                              # standard control height (px)
RADIUS     = 8                               # standard corner radius (px)

def _refresh_palette():
    """Re-evaluate the palette globals after the active theme changed."""
    global BG, SIDEBAR_BG, CARD_BG, CARD_HOVER, ACCENT, TEXT_PRIMARY, TEXT_MUTED
    global TEXT_DIM, PLAYER_BG, BAR_TRACK, BAR_HANDLE, DIVIDER, BORDER, ACCENT_FG
    BG           = _c("#0F0F1A")
    SIDEBAR_BG   = _c("#13131f")
    CARD_BG      = _c("#1A1A2E")
    CARD_HOVER   = _c("#242444")
    ACCENT       = _c("#FF2A6D")
    TEXT_PRIMARY = _c("#F1F1F1")
    TEXT_MUTED   = _c("#ABABC2")
    TEXT_DIM     = _c("#8B8B9E")
    PLAYER_BG    = _c("#000000")
    BAR_TRACK    = _c("#2A2A4A")
    BAR_HANDLE   = _c("#FF2A6D")
    DIVIDER      = _c("#2A2A3D")
    BORDER       = _c("#2A2A3D")
    ACCENT_FG    = _accent_fg(ACCENT)


def _set_active_theme(name):
    """Switch the active theme. Returns (old_map, new_map)."""
    global _CUR_THEME, _CUR_MAP
    if name not in THEMES:
        name = "Obsidian"
    old = _CUR_MAP
    _CUR_MAP = _build_theme_map(name)
    _CUR_THEME = name
    _refresh_palette()
    return old, _CUR_MAP


from PIL import Image as _PILImage, ImageDraw as _PILDraw, ImageTk as _PILTk

THUMB_W    = 140
THUMB_H    = int(THUMB_W * 9 / 16)
CARD_BORDER = 3

import sys
_BASE_DIR = _DATA_DIR

_DEBUG_LOG_PATH = os.path.join(_BASE_DIR, "mpv_debug.log")

def _debug_log(msg):
    """Append a timestamped line to a debug log file (app has no console)."""
    try:
        os.makedirs(_BASE_DIR, exist_ok=True)
        with open(_DEBUG_LOG_PATH, "a", encoding="utf-8") as f:
            f.write(f"{datetime.datetime.now().isoformat()} {msg}\n")
    except Exception:
        pass

CACHE_DIR = os.path.join(_BASE_DIR, ".clipplayer_cache")

GAME_PREFIXES = {
    "All": (),
}

# ── Game name aliases ─────────────────────────────────────────────────────────
# Maps normalised prefix → canonical display name shown in the category list.
# Add more entries here to rename any auto-detected game label.
_GAME_ALIASES = {
    "cs2":    "Counter-Strike 2",
    "tarkov": "Escape from Tarkov",
}

# ── UUID filename aliases ─────────────────────────────────────────────────────
# Maps UUID prefix (the part before the date, lowercased) → category display name.
_UUID_ALIASES = {
    "c2258301-017b-4b6b-89e8-9c5ffea5767c": "ERA Season 7",
    "8e9a5856-640d-43de-a1f0-65d04b073e5a": "ERA Season 6",
}

# ── Filename helpers ──────────────────────────────────────────────────────────
def date_from_filename(name):
    name = os.path.basename(name)
    # "clip-from-Feb-25-2022" pattern
    _MONTHS = {"jan":"01","feb":"02","mar":"03","apr":"04","may":"05","jun":"06",
               "jul":"07","aug":"08","sep":"09","oct":"10","nov":"11","dec":"12"}
    m = re.search(r'clip-from-([A-Za-z]{3})-(\d{1,2})-(\d{4})', name, re.IGNORECASE)
    if m:
        mon = _MONTHS.get(m.group(1).lower())
        if mon:
            try:
                return datetime.datetime(int(m.group(3)), int(mon), int(m.group(2)))
            except Exception:
                pass
    m = re.search(r'(\d{4})[-_](\d{2})[-_](\d{2})[\s_-]+(\d{2})[-_](\d{2})[-_](\d{2})', name)
    if m:
        try:
            return datetime.datetime(int(m.group(1)), int(m.group(2)), int(m.group(3)),
                                     int(m.group(4)), int(m.group(5)), int(m.group(6)))
        except Exception:
            pass
    m = re.search(r'(\d{4})\.(\d{2})\.(\d{2})\s*-\s*(\d{2})\.(\d{2})\.(\d{2})(?:\.\d+)?', name)
    if m:
        try:
            return datetime.datetime(int(m.group(1)), int(m.group(2)), int(m.group(3)),
                                     int(m.group(4)), int(m.group(5)), int(m.group(6)))
        except Exception:
            pass
    return None

def mtime_from_filename(name, folder_path=None):
    dt = date_from_filename(name)
    if dt:
        return dt.timestamp()
    if folder_path:
        try:
            return os.path.getmtime(os.path.join(folder_path, name))
        except Exception:
            pass
    return 0

def game_name_from_filename(name):
    name = os.path.basename(name)
    stem = os.path.splitext(name)[0]
    # Strip trailing .DVR suffix (case-insensitive) before parsing
    stem = re.sub(r'\.DVR$', '', stem, flags=re.IGNORECASE)
    # "Name-clip-from-Mon-DD-YYYY" or "Name-clip-from-Apr-12-2022" pattern
    m = re.search(r'^(.+?)-clip-from-[A-Za-z]+-\d{1,2}-\d{4}', stem, re.IGNORECASE)
    if m:
        label = m.group(1).replace('-', ' ').replace('_', ' ').strip()
        if not label:
            return "Moments"
        alias = _GAME_ALIASES.get(_normalize_game(label))
        return alias if alias else label
    if '__' in stem:
        prefix = stem.split('__')[0]
    else:
        m = re.search(r'^(.+?)_(\d{4}-\d{2}-\d{2})', stem)
        if m:
            prefix = m.group(1)
        else:
            # DVR format: "Game Name YYYY.MM.DD - HH.MM.SS"
            m = re.search(r'^(.+?)\s+\d{4}\.\d{2}\.\d{2}', stem)
            if m:
                prefix = m.group(1)
            else:
                return "Moments"
    # Check UUID aliases on the raw prefix before normalising
    uuid_alias = _UUID_ALIASES.get(prefix.strip().lower())
    if uuid_alias:
        return uuid_alias
    label = prefix.replace('-', ' ').replace('_', ' ').strip()
    if not label:
        return "Moments"
    # Apply alias mapping (case-insensitive) so e.g. "CS2" → "Counter-Strike 2"
    alias = _GAME_ALIASES.get(_normalize_game(label))
    return alias if alias else label

def _normalize_game(label):
    """Lowercase + strip all non-alphanumeric for fuzzy grouping."""
    return re.sub(r'[^a-z0-9]', '', label.lower())

def resource_path(relative_path):
    base = getattr(sys, "_MEIPASS", os.path.dirname(os.path.abspath(__file__)))
    return os.path.join(base, relative_path)

# ── ffmpeg helpers ────────────────────────────────────────────────────────────
def find_ffmpeg():
    """Find ffmpeg.exe. The copy that ships with the app is preferred (so it
    works on PCs that have no ffmpeg installed); a system-wide ffmpeg on PATH
    or in a common folder is used as a fallback. ffprobe.exe is looked up next
    to whichever ffmpeg.exe is returned, so both must live in the same folder."""
    import shutil
    bundled_dirs = []
    if getattr(sys, "frozen", False):
        # Installed / PyInstaller build: <install folder>\ffmpeg
        bundled_dirs.append(os.path.join(os.path.dirname(sys.executable), "ffmpeg"))
        meipass = getattr(sys, "_MEIPASS", None)
        if meipass:
            bundled_dirs.append(os.path.join(meipass, "ffmpeg"))
    # Running from source: <folder of FeatherPlayer.py>\ffmpeg
    bundled_dirs.append(os.path.join(os.path.dirname(os.path.abspath(__file__)), "ffmpeg"))
    for d in bundled_dirs:
        p = os.path.join(d, "ffmpeg.exe")
        if os.path.exists(p): return p
    f = shutil.which("ffmpeg")
    if f: return f
    for p in [r"C:\ffmpeg\bin\ffmpeg.exe", r"C:\Program Files\ffmpeg\bin\ffmpeg.exe"]:
        if os.path.exists(p): return p
    return None

def cache_key(video_path):
    stat = os.stat(video_path)
    uid  = f"{video_path}|{stat.st_size}|{stat.st_mtime}|tw{THUMB_W}"
    return hashlib.md5(uid.encode()).hexdigest() + ".jpg"

def get_video_duration(video_path, ffmpeg_path):
    if not ffmpeg_path:
        return 0
    ffprobe = os.path.join(os.path.dirname(ffmpeg_path), "ffprobe.exe")
    if not os.path.exists(ffprobe):
        ffprobe = "ffprobe"
    try:
        result = subprocess.run([
            ffprobe, "-v", "error", "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1", video_path
        ], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
           timeout=8, creationflags=0x08000000)
        return float(result.stdout.strip())
    except Exception:
        return 0

def get_audio_handlers(video_path, ffmpeg_path):
    """Handler names of the clip's audio streams, in order (e.g. ["Game", "Mic"]).
    Returns None when ffprobe can't be run."""
    if not ffmpeg_path:
        return None
    ffprobe = os.path.join(os.path.dirname(ffmpeg_path), "ffprobe.exe")
    if not os.path.exists(ffprobe):
        ffprobe = "ffprobe"
    try:
        r = subprocess.run([
            ffprobe, "-v", "error", "-select_streams", "a",
            "-show_entries", "stream_tags=handler_name", "-of", "json", video_path
        ], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
           timeout=10, creationflags=0x08000000)
        if r.returncode != 0:
            return None
        streams = json.loads(r.stdout.decode("utf-8", "ignore") or "{}").get("streams", [])
        return [str((st.get("tags") or {}).get("handler_name") or "") for st in streams]
    except Exception:
        return None

def get_audio_bitrates(video_path, ffmpeg_path):
    """Bitrate (bits/s, 0 = unknown) of every audio stream, in order, or None
    when ffprobe can't be run."""
    if not ffmpeg_path:
        return None
    ffprobe = os.path.join(os.path.dirname(ffmpeg_path), "ffprobe.exe")
    if not os.path.exists(ffprobe):
        ffprobe = "ffprobe"
    try:
        r = subprocess.run([
            ffprobe, "-v", "error", "-select_streams", "a",
            "-show_entries", "stream=bit_rate", "-of", "json", video_path
        ], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
           timeout=10, creationflags=0x08000000)
        if r.returncode != 0:
            return None
        streams = json.loads(r.stdout.decode("utf-8", "ignore") or "{}").get("streams", [])
        out = []
        for st in streams:
            try:
                out.append(int(st.get("bit_rate") or 0))
            except Exception:
                out.append(0)
        return out
    except Exception:
        return None

def get_video_bitrate_kbps(video_path, ffmpeg_path):
    """Video bitrate of the first video stream in kb/s (falls back to the
    whole file's average bitrate), or 0 if it can't be worked out."""
    if not ffmpeg_path:
        return 0
    ffprobe = os.path.join(os.path.dirname(ffmpeg_path), "ffprobe.exe")
    if not os.path.exists(ffprobe):
        ffprobe = "ffprobe"
    def _probe(entries, select=None):
        cmd = [ffprobe, "-v", "error"]
        if select:
            cmd += ["-select_streams", select]
        cmd += ["-show_entries", entries, "-of", "default=noprint_wrappers=1:nokey=1", video_path]
        r = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                           timeout=8, creationflags=0x08000000)
        return r.stdout.decode().strip().splitlines()
    try:
        for entries, select in (("stream=bit_rate", "v:0"), ("format=bit_rate", None)):
            try:
                for line in _probe(entries, select):
                    if line.strip().isdigit() and int(line) > 0:
                        return int(int(line) / 1000)
            except Exception:
                pass
        dur = get_video_duration(video_path, ffmpeg_path)
        if dur > 0:
            return int(os.path.getsize(video_path) * 8 / dur / 1000)
    except Exception:
        pass
    return 0

def get_video_info(video_path, ffmpeg_path):
    """Returns (width, height, fps) for the first video stream, or (0, 0, 0)."""
    if not ffmpeg_path:
        return (0, 0, 0)
    ffprobe = os.path.join(os.path.dirname(ffmpeg_path), "ffprobe.exe")
    if not os.path.exists(ffprobe):
        ffprobe = "ffprobe"
    try:
        result = subprocess.run([
            ffprobe, "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=width,height,r_frame_rate",
            "-of", "csv=p=0:s=,", video_path
        ], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
           timeout=8, creationflags=0x08000000)
        w_str, h_str, fps_str = result.stdout.decode().strip().split(",")
        num, _, den = fps_str.partition("/")
        fps = float(num) / float(den) if den else float(num)
        return (int(w_str), int(h_str), fps or 30.0)
    except Exception:
        return (0, 0, 0)

def get_thumb(video_path, ffmpeg_path):
    os.makedirs(CACHE_DIR, exist_ok=True)
    cached = os.path.join(CACHE_DIR, cache_key(video_path))
    if os.path.exists(cached) and os.path.getsize(cached) > 0:
        return cached
    if not ffmpeg_path:
        return None
    try:
        duration = get_video_duration(video_path, ffmpeg_path)
        seek_sec = 0 if duration <= 15 else max(1, duration / 2) if duration > 2 else 1
        tw = THUMB_W * 2
        subprocess.run([
            ffmpeg_path, "-y", "-ss", str(int(seek_sec)), "-i", video_path,
            "-vframes", "1", "-update", "1",
            "-vf", f"scale={tw}:-2",
            "-q:v", "2", cached
        ], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
           timeout=10, creationflags=0x08000000)
        if os.path.exists(cached) and os.path.getsize(cached) > 0:
            return cached
    except Exception:
        pass
    return None

# ── Rounded rect helper ───────────────────────────────────────────────────────
_AA_SCALE     = 4      # supersampling factor
_AA_CACHE_MAX = 300    # per-canvas image cache size (LRU)


def _aa_draw(canvas, kind, x0, y0, x1, y1, r, fill, outline, width, tags, corners):
    """Draw an anti-aliased rounded rect / oval as an image on `canvas`.

    The shape is rendered at 4x size with PIL and shrunk with LANCZOS, which
    blends the edge pixels (what browsers do). The image has an alpha
    channel, so edges blend into whatever the canvas background is.
    """
    x0, y0, x1, y1 = int(round(x0)), int(round(y0)), int(round(x1)), int(round(y1))
    w, h = x1 - x0, y1 - y0
    if w < 2 or h < 2:
        return
    r = int(max(0, min(r, w // 2, h // 2)))
    key = (kind, w, h, r, fill, outline, width, corners)

    # The PhotoImage MUST stay referenced somewhere or Tk drops it, so cache
    # it on the canvas. Dict order is used as an LRU.
    cache = getattr(canvas, "_aa_cache", None)
    if cache is None:
        cache = canvas._aa_cache = {}
    photo = cache.pop(key, None)

    if photo is None:
        S = _AA_SCALE
        img = _PILImage.new("RGBA", (w * S, h * S), (0, 0, 0, 0))
        d = _PILDraw.Draw(img)
        flags = ("tl" in corners, "tr" in corners, "br" in corners, "bl" in corners)

        def shape(color, inset):
            i = inset * S
            box = [i, i, w * S - 1 - i, h * S - 1 - i]
            if box[2] <= box[0] or box[3] <= box[1]:
                return
            if kind == "oval":
                d.ellipse(box, fill=color)
            else:
                d.rounded_rectangle(box, radius=max(r * S - i, 0),
                                    fill=color, corners=flags)

        if outline:
            shape(outline, 0)
            # fill the inside, or punch it out transparent for outline-only
            shape(fill if fill else (0, 0, 0, 0), width)
        elif fill:
            shape(fill, 0)

        img = img.resize((w, h), _PILImage.LANCZOS)
        photo = _PILTk.PhotoImage(img)

    cache[key] = photo                       # (re)insert as most recent
    while len(cache) > _AA_CACHE_MAX:
        cache.pop(next(iter(cache)))

    kw = {"tags": tags} if tags else {}
    canvas.create_image(x0, y0, image=photo, anchor="nw", **kw)


def _rounded_rect(canvas, x0, y0, x1, y1, r, fill, outline="", width=1, tags=None,
                   corners=("tl", "tr", "br", "bl")):
    """Anti-aliased rounded rectangle. `corners` controls which corners are
    rounded (any subset of 'tl','tr','br','bl'); the rest are square."""
    _aa_draw(canvas, "rect", x0, y0, x1, y1, r, fill, outline, width, tags,
             tuple(sorted(corners)))


def _aa_oval(canvas, x0, y0, x1, y1, fill="", outline="", width=1, tags=None):
    """Anti-aliased replacement for canvas.create_oval."""
    _aa_draw(canvas, "oval", x0, y0, x1, y1, 0, fill, outline, width, tags, ())

# ── Vector icons ──────────────────────────────────────────────────────────────
# Every icon is drawn with PIL at 8x size on a 24-unit grid, then shrunk with
# LANCZOS, so strokes have clean anti-aliased edges at any size and any colour
# (no icon font or PNG files needed). Use:  ui_icon("folder", 18, color)

_ICON_CACHE = {}
_ICON_SS    = 8


def _rounded_poly(pts, r, closed=True, steps=6):
    """Dense point list tracing `pts` with every corner rounded by radius r."""
    n = len(pts)
    out = []
    if not closed:
        out.append(pts[0])
    idxs = range(n) if closed else range(1, n - 1)
    for i in idxs:
        p0, p1, p2 = pts[(i - 1) % n], pts[i], pts[(i + 1) % n]
        v1 = (p0[0] - p1[0], p0[1] - p1[1])
        v2 = (p2[0] - p1[0], p2[1] - p1[1])
        l1, l2 = _math.hypot(*v1), _math.hypot(*v2)
        if l1 == 0 or l2 == 0:
            out.append(p1)
            continue
        rr = min(r, l1 / 2, l2 / 2)
        a = (p1[0] + v1[0] / l1 * rr, p1[1] + v1[1] / l1 * rr)
        b = (p1[0] + v2[0] / l2 * rr, p1[1] + v2[1] / l2 * rr)
        for k in range(steps + 1):
            t = k / steps
            out.append(((1 - t) ** 2 * a[0] + 2 * (1 - t) * t * p1[0] + t * t * b[0],
                        (1 - t) ** 2 * a[1] + 2 * (1 - t) * t * p1[1] + t * t * b[1]))
    if not closed:
        out.append(pts[-1])
    else:
        out.append(out[0])
    return out


def _arc_pts(cx, cy, r, a0, a1, n=48):
    return [(cx + r * _math.cos(_math.radians(a0 + (a1 - a0) * i / n)),
             cy + r * _math.sin(_math.radians(a0 + (a1 - a0) * i / n)))
            for i in range(n + 1)]


def _icon_image(name, N, color, filled, stroke):
    S = _ICON_SS
    px = N * S
    img = _PILImage.new("RGBA", (px, px), (0, 0, 0, 0))
    d = _PILDraw.Draw(img)
    k = px / 24.0
    sw = max(1, int(round(stroke * k)))

    def P(pts):
        return [(x * k, y * k) for x, y in pts]

    def line(pts, w=None):
        w = w or sw
        pp = P(pts)
        d.line(pp, fill=color, width=w, joint="curve")
        rad = w / 2.0
        for (x, y) in (pp[0], pp[-1]):                    # round caps
            d.ellipse([x - rad, y - rad, x + rad, y + rad], fill=color)

    def shape(pts, fill):
        pp = P(pts)
        if fill:
            d.polygon(pp, fill=color)
        d.line(pp, fill=color, width=sw, joint="curve")
        rad = sw / 2.0
        x, y = pp[0]
        d.ellipse([x - rad, y - rad, x + rad, y + rad], fill=color)

    def ring(cx, cy, r):
        R = r * k + sw / 2.0
        d.ellipse([cx * k - R, cy * k - R, cx * k + R, cy * k + R],
                  outline=color, width=sw)

    def rrect(x0, y0, x1, y1, r):
        d.rounded_rectangle([x0 * k, y0 * k, x1 * k, y1 * k], radius=r * k, fill=color)

    if name == "scissors":
        ring(6, 6, 2.7); ring(6, 18, 2.7)
        line([(20, 4), (8.4, 15.6)])
        line([(14.5, 14.5), (20, 20)])
        line([(8.4, 8.4), (12, 12)])
    elif name == "plus":
        line([(12, 5), (12, 19)]); line([(5, 12), (19, 12)])
    elif name == "fullscreen":
        line([(4, 9), (4, 4), (9, 4)]);     line([(15, 4), (20, 4), (20, 9)])
        line([(20, 15), (20, 20), (15, 20)]); line([(9, 20), (4, 20), (4, 15)])
    elif name == "fullscreen-exit":
        line([(9, 4), (9, 9), (4, 9)]);     line([(15, 4), (15, 9), (20, 9)])
        line([(20, 15), (15, 15), (15, 20)]); line([(4, 15), (9, 15), (9, 20)])
    elif name == "x":
        line([(6.5, 6.5), (17.5, 17.5)]); line([(17.5, 6.5), (6.5, 17.5)])
    elif name == "check":
        line([(5, 12.5), (10, 17.5), (19, 7)])
    elif name == "chevron-down":
        line([(6, 9), (12, 15), (18, 9)])
    elif name == "chevron-left":
        line([(15, 5.5), (8.5, 12), (15, 18.5)])
    elif name == "calendar":
        shape(_rounded_poly([(3.8, 5.6), (20.2, 5.6), (20.2, 20.2), (3.8, 20.2)], 2.4), False)
        line([(3.8, 10.4), (20.2, 10.4)])
        line([(8.2, 3.2), (8.2, 7.4)]); line([(15.8, 3.2), (15.8, 7.4)])
    elif name == "arrow-left":
        line([(19, 12), (5, 12)]); line([(11, 6), (5, 12), (11, 18)])
    elif name == "folder":
        shape(_rounded_poly([(3, 6.2), (9.4, 6.2), (11.8, 9), (21, 9),
                             (21, 18.8), (3, 18.8)], 2.0), False)
    elif name == "folder-plus":
        shape(_rounded_poly([(3, 6.2), (9.4, 6.2), (11.8, 9), (21, 9),
                             (21, 18.8), (3, 18.8)], 2.0), False)
        line([(12, 11.8), (12, 16.2)]); line([(9.8, 14), (14.2, 14)])
    elif name == "share":
        ring(18, 5.5, 2.8); ring(6, 12, 2.8); ring(18, 18.5, 2.8)
        line([(8.5, 10.7), (15.5, 6.8)]); line([(8.5, 13.3), (15.5, 17.2)])
    elif name == "compress":
        line([(4.5, 12), (19.5, 12)])
        line([(12, 3.5), (12, 9.5)]); line([(8.5, 6.5), (12, 10), (15.5, 6.5)])
        line([(12, 20.5), (12, 14.5)]); line([(8.5, 17.5), (12, 14), (15.5, 17.5)])
    elif name == "refresh":
        a0, a1 = -35, 245
        line(_arc_pts(12, 12, 7.8, a0, a1, 64))
        ex, ey = 12 + 7.8 * _math.cos(_math.radians(a1)), 12 + 7.8 * _math.sin(_math.radians(a1))
        tx, ty = -_math.sin(_math.radians(a1)), _math.cos(_math.radians(a1))   # tangent
        for sgn in (1, -1):
            ang = _math.atan2(ty, tx) + _math.pi + sgn * _math.radians(38)
            line([(ex, ey), (ex + 4.4 * _math.cos(ang), ey + 4.4 * _math.sin(ang))])
    elif name == "gear":
        pts = []
        for i in range(8):
            a = i * 45.0
            for da, rad in ((-15, 7.3), (-10, 9.6), (10, 9.6), (15, 7.3)):
                t = _math.radians(a + da)
                pts.append((12 + rad * _math.cos(t), 12 + rad * _math.sin(t)))
        shape(_rounded_poly(pts, 0.9, steps=4), False)
        ring(12, 12, 3.1)
    elif name == "play":
        path = _rounded_poly([(7.6, 4.8), (19.4, 12), (7.6, 19.2)], 2.2)
        pp = P(path); d.polygon(pp, fill=color)
        d.line(pp, fill=color, width=int(1.1 * k), joint="curve")
    elif name == "pause":
        rrect(6.3, 4.6, 10.4, 19.4, 1.3); rrect(13.6, 4.6, 17.7, 19.4, 1.3)
    elif name == "skip-prev":
        rrect(5, 5, 7.8, 19, 0.9)
        pp = P(_rounded_poly([(19.6, 5.2), (9.4, 12), (19.6, 18.8)], 1.6))
        d.polygon(pp, fill=color); d.line(pp, fill=color, width=int(0.9 * k), joint="curve")
    elif name == "skip-next":
        rrect(16.2, 5, 19, 19, 0.9)
        pp = P(_rounded_poly([(4.4, 5.2), (14.6, 12), (4.4, 18.8)], 1.6))
        d.polygon(pp, fill=color); d.line(pp, fill=color, width=int(0.9 * k), joint="curve")
    elif name in ("volume", "mute"):
        pp = P(_rounded_poly([(3.8, 9.6), (7.8, 9.6), (12.6, 5.4),
                              (12.6, 18.6), (7.8, 14.4), (3.8, 14.4)], 1.0))
        d.polygon(pp, fill=color); d.line(pp, fill=color, width=int(0.8 * k), joint="curve")
        if name == "volume":
            line(_arc_pts(12.6, 12, 4.6, -48, 48, 24))
            line(_arc_pts(12.6, 12, 8.4, -48, 48, 32))
        else:
            line([(16.4, 9.4), (21.2, 14.6)]); line([(21.2, 9.4), (16.4, 14.6)])
    elif name == "trash":
        line([(4.5, 7), (19.5, 7)])
        line([(9.5, 7), (9.5, 4.6), (14.5, 4.6), (14.5, 7)])
        shape(_rounded_poly([(6.4, 7.6), (7.4, 19.6), (16.6, 19.6), (17.6, 7.6)], 1.2), False)
        line([(10.2, 11), (10.2, 16)]); line([(13.8, 11), (13.8, 16)])
    elif name == "alert":
        shape(_rounded_poly([(12, 3.8), (21.2, 19.8), (2.8, 19.8)], 1.8), False)
        line([(12, 9.8), (12, 14.2)]); line([(12, 17), (12, 17)])
    elif name == "star":
        pts = []
        for i in range(10):
            rad = 10.2 if i % 2 == 0 else 4.5
            t = _math.radians(-90 + i * 36)
            pts.append((12 + rad * _math.cos(t), 12.8 + rad * _math.sin(t)))
        shape(_rounded_poly(pts, 1.1, steps=4), filled)
    elif name == "heart":
        raw = []
        for i in range(160):
            t = 2 * _math.pi * i / 160
            raw.append((16 * _math.sin(t) ** 3,
                        -(13 * _math.cos(t) - 5 * _math.cos(2 * t)
                          - 2 * _math.cos(3 * t) - _math.cos(4 * t))))
        xs = [p[0] for p in raw]; ys = [p[1] for p in raw]
        sc = min(17.6 / (max(xs) - min(xs)), 16.4 / (max(ys) - min(ys)))
        cx = (max(xs) + min(xs)) / 2; cy = (max(ys) + min(ys)) / 2
        pts = [(12 + (x - cx) * sc, 12.4 + (y - cy) * sc) for x, y in raw]
        pts.append(pts[0])
        shape(pts, filled)
    elif name == "music":
        # beamed pair of notes (same symbol as the audio tracks button)
        hr = 2.9
        for hx, hy in ((7.0, 18.2), (17.0, 16.2)):
            d.ellipse([(hx - hr) * k, (hy - hr) * k, (hx + hr) * k, (hy + hr) * k], fill=color)
        line([(9.5, 17.4), (9.5, 6.4)], w=max(sw, int(round(1.9 * k))))
        line([(19.5, 15.4), (19.5, 4.4)], w=max(sw, int(round(1.9 * k))))
        d.polygon(P([(9.5, 6.0), (19.5, 4.0), (19.5, 7.6), (9.5, 9.6)]), fill=color)

    return img.resize((N, N), _PILImage.LANCZOS)


def ui_icon(name, size, color, filled=False, stroke=1.9):
    """Cached, anti-aliased vector icon as a Tk PhotoImage."""
    key = (name, size, str(color).lower(), filled, stroke)
    ph = _ICON_CACHE.get(key)
    if ph is None:
        ph = _PILTk.PhotoImage(_icon_image(name, size, color, filled, stroke))
        _ICON_CACHE[key] = ph
    return ph


def ui_icon_shadow(name, size, color, filled=False, stroke=1.9,
                   pad=4, blur=1.8, gain=1.8):
    """Like ui_icon, but with a soft dark shadow behind the glyph so it stays
    readable over bright thumbnails. The image is `pad` px larger on every
    side than the icon (room for the blur), so offset its anchor by `pad`."""
    key = ("shadow", name, size, str(color).lower(), filled, stroke, pad, blur, gain)
    ph = _ICON_CACHE.get(key)
    if ph is None:
        from PIL import ImageFilter as _PILFilter
        glyph = _icon_image(name, size, color, filled, stroke)
        n = size + 2 * pad
        base = _PILImage.new("RGBA", (n, n), (0, 0, 0, 0))
        base.paste(glyph, (pad, pad))
        a = _PILImage.new("L", (n, n), 0)
        a.paste(base.split()[3], (0, 1))                       # 1px down
        a = a.filter(_PILFilter.GaussianBlur(blur)).point(
            lambda v: min(255, int(v * gain)))
        shadow = _PILImage.new("RGBA", (n, n), (0, 0, 0, 0))
        shadow.putalpha(a)
        ph = _PILTk.PhotoImage(_PILImage.alpha_composite(shadow, base))
        _ICON_CACHE[key] = ph
    return ph


# ── Slim progress slider ──────────────────────────────────────────────────────
class SlimSlider(tk.Canvas):
    TRACK_H  = 6    # half the track height (track is a rounded pill)
    HANDLE_R = 6    # kept for _x_to_val/_val_to_x pad calc
    LINE_W   = 5    # playhead / volume handle: a thick rounded pill
    HEIGHT   = 22   # canvas height
    PH_PAD_BOTTOM = 3   # playhead stops this many px above the canvas bottom

    def __init__(self, parent, from_=0, to=100, value=0,
                 on_change=None, on_press=None, on_release=None,
                 fill_color=ACCENT, **kwargs):
        kwargs.setdefault("bg", BG)
        kwargs.setdefault("highlightthickness", 0)
        kwargs.setdefault("bd", 0)
        # Taller canvas so the trim handle lines have room
        super().__init__(parent, height=self.HEIGHT, **kwargs)
        self._from       = from_
        self._to         = to
        self._value      = value
        self._fill       = fill_color
        self._on_change  = on_change
        self._on_press   = on_press
        self._on_release = on_release
        self._dragging   = False
        self._id_fill_rect = None
        self._id_fill_cap  = None
        self._id_fill_poly = None
        self._id_handle    = None

        self.bind("<Configure>",       self._full_redraw)
        self.bind("<ButtonPress-1>",   self._press)
        self.bind("<B1-Motion>",       self._drag)
        self.bind("<ButtonRelease-1>", self._release)

    def _x_to_val(self, x):
        pad   = self.HANDLE_R + 2
        w     = self.winfo_width() - 2 * pad
        ratio = max(0.0, min(1.0, (x - pad) / max(w, 1)))
        return self._from + ratio * (self._to - self._from)

    def _val_to_x(self, val):
        pad   = self.HANDLE_R + 2
        w     = self.winfo_width() - 2 * pad
        ratio = (val - self._from) / max(self._to - self._from, 1)
        return pad + ratio * w

    def _full_redraw(self, event=None):
        self.delete("all")
        pad = self.HANDLE_R + 2
        h   = self.winfo_height()
        w   = self.winfo_width()
        if w < 4 or h < 4:
            return
        cy  = h // 2
        th  = self.TRACK_H
        # Rounded pill track + fill (round-capped lines: endpoints are inset by
        # the radius so the visible ends sit exactly on pad .. w-pad)
        a = pad + th
        b = max(w - pad - th, a)
        self.create_line(a, cy, b, cy, width=2*th, capstyle="round",
                         fill=BAR_TRACK, tags="track")
        # Fill: rounded on the left (oval cap), flat on the right so it ends
        # flush against the playhead / volume handle.
        self._id_fill_cap  = self.create_line(a, cy, a+1, cy, width=2*th, capstyle="round",
                                              fill=self._fill, tags="fill")
        self._id_fill_rect = self.create_rectangle(a, cy-th, a, cy+th,
                                                   fill=self._fill, outline="", tags="fill")
        # Short fill (playhead still inside the rounded left end): a shape cut
        # to the pill's curve, so the fill stays rounded instead of going square.
        self._id_fill_poly = self.create_polygon(pad, cy, pad, cy, pad, cy,
                                                 fill=self._fill, outline="",
                                                 state="hidden", tags="fill")
        # Playhead / handle: thick rounded pill, slightly inset from the top and bottom
        self._id_handle = self.create_line(0, 3, 0, h-self.PH_PAD_BOTTOM, width=self.LINE_W, capstyle="round",
                                           fill=BAR_HANDLE, tags="handle")
        self._move_items()

    def _move_items(self):
        if self._id_handle is None:
            return
        pad = self.HANDLE_R + 2
        h   = self.winfo_height()
        w   = self.winfo_width()
        if w < 4 or h < 4:
            return
        cy  = h // 2
        th  = self.TRACK_H
        hx  = self._val_to_x(self._value)
        show_fill = hx > pad + 1
        full_cap = hx - pad >= 2 * th
        long_fill = show_fill and full_cap
        self.itemconfigure(self._id_fill_rect, state="normal" if long_fill else "hidden")
        self.itemconfigure(self._id_fill_cap, state="normal" if long_fill else "hidden")
        if long_fill:
            self.coords(self._id_fill_rect, pad + th, cy-th, hx, cy+th)
        if show_fill and not full_cap:
            # Playhead is still inside the rounded left end: draw the fill as a
            # shape following the pill's curve so it stays rounded.
            c0 = pad + th
            top, bot = [], []
            x = pad
            while True:
                xx = min(x, hx)
                dx = xx - c0
                hh = _math.sqrt(max(th * th - dx * dx, 0))
                top.append((xx, cy - hh))
                bot.append((xx, cy + hh))
                if xx >= hx:
                    break
                x += 1
            flat = [c for pt in (top + bot[::-1]) for c in pt]
            self.coords(self._id_fill_poly, *flat)
            self.itemconfigure(self._id_fill_poly, state="normal")
        else:
            self.itemconfigure(self._id_fill_poly, state="hidden")
        self.coords(self._id_handle, hx, 3, hx, h-self.PH_PAD_BOTTOM)

    def _press(self, event):
        self._dragging = True
        self._value = self._x_to_val(event.x)
        self._move_items()
        if self._on_press:  self._on_press()
        if self._on_change: self._on_change(self._value)

    def _drag(self, event):
        if self._dragging:
            self._value = self._x_to_val(event.x)
            self._move_items()
            if self._on_change: self._on_change(self._value)

    def _release(self, event):
        self._dragging = False
        self._value = self._x_to_val(event.x)
        self._move_items()
        if self._on_release: self._on_release(self._value)
        if self._on_change:  self._on_change(self._value)

    def get(self):
        return self._value

    def set(self, val, redraw=True):
        self._value = max(self._from, min(self._to, val))
        if redraw:
            self._move_items()

# ── Apply-trim helpers ───────────────────────────────────────────────────────
def _copy_file_times(src, dst):
    """Give dst the same modified + creation time as src (Windows)."""
    try:
        st = os.stat(src)
        os.utime(dst, ns=(st.st_atime_ns, st.st_mtime_ns))
        created = getattr(st, "st_birthtime", st.st_ctime)
        ft = int((created + 11644473600) * 10_000_000)

        class _FT(ctypes.Structure):
            _fields_ = [("lo", ctypes.c_uint32), ("hi", ctypes.c_uint32)]

        k32 = ctypes.windll.kernel32
        k32.CreateFileW.restype = ctypes.c_void_p
        h = k32.CreateFileW(dst, 0x100, 7, None, 3, 0x80, None)  # FILE_WRITE_ATTRIBUTES
        if h and h != ctypes.c_void_p(-1).value:
            c = _FT(ft & 0xFFFFFFFF, ft >> 32)
            k32.SetFileTime(ctypes.c_void_p(h), ctypes.byref(c), None, None)
            k32.CloseHandle(ctypes.c_void_p(h))
    except Exception:
        pass

_TRIM_FORMATS = {".mp4": "mp4", ".m4v": "mp4", ".mov": "mov", ".mkv": "matroska",
                 ".webm": "webm", ".avi": "avi", ".wmv": "asf", ".flv": "flv"}


# ── Trim slider ──────────────────────────────────────────────────────────────
class TrimSlider(SlimSlider):
    """Progress bar with two draggable trim handle lines and snap-to-playhead."""
    TRIM_HIT   = 12   # px hit radius for grabbing a trim handle
    SNAP_PX    = 15   # px distance at which trim snaps to playhead
    PH_SNAP_PX = 10   # px distance at which the dragged playhead snaps to a trim line
    TRIM_CLR   = _c("#ffffff")    # white trim handles
    TRIM_FADE  = _c("#0a1a18")    # unused now but kept for reference
    HEIGHT     = 28   # a bit taller so the trim brackets can stand clear of the track
    PH_PAD_BOTTOM = 4  # 1 px more than the base class: centres the playhead on the track
    TRACK_H    = 8    # half the track height: playback bar is 16 px thick (volume slider keeps 12)
    KEPT_TINT  = 0.22 # how far the kept section is blended toward the track colour

    def __init__(self, *args, **kwargs):
        self._trim_start    = 0
        self._trim_end      = 10000
        self._trim_drag     = None
        self._trim_visible  = False  # only show handles when trim is enabled
        self._snap_playhead = False  # True only while the playhead itself is being dragged
        self._app        = None
        super().__init__(*args, **kwargs)
        self.bind("<ButtonPress-1>",   self._trim_press,   add=False)
        self.bind("<B1-Motion>",       self._trim_drag_,   add=False)
        self.bind("<ButtonRelease-1>", self._trim_release, add=False)
        self.bind("<Motion>",          self._trim_cursor,  add=True)
        self.bind("<Leave>",           lambda e: self.configure(cursor=""), add=True)

    def set_trim(self, start, end):
        self._trim_start = max(0, min(10000, start))
        self._trim_end   = max(0, min(10000, end))
        self._move_items()

    def _full_redraw(self, event=None):
        super()._full_redraw(event)
        self._draw_trim_overlays()

    def _move_items(self):
        super()._move_items()
        self._draw_trim_overlays()

    def _cap_zone(self, x0, x1, cy, th, pad, w, left, dim):
        """Dim block for a narrow excluded zone at the very end of the track,
        cut to the shape of the rounded pill end so it has the round corners."""
        top, bot = [], []
        x = x0
        while True:
            xx = min(x, x1)
            if left:
                dx = xx - (pad + th)
                hh = th if dx >= 0 else _math.sqrt(max(th * th - dx * dx, 0))
            else:
                dx = xx - (w - pad - th)
                hh = th if dx <= 0 else _math.sqrt(max(th * th - dx * dx, 0))
            top.append((xx, cy - hh))
            bot.append((xx, cy + hh))
            if xx >= x1:
                break
            x += 1
        flat = [c for p in (top + bot[::-1]) for c in p]
        if len(flat) >= 6:
            self.create_polygon(*flat, fill=dim, outline="", tags="trim_overlay")

    def _draw_trim_overlays(self):
        self.delete("trim_overlay")
        if not self._trim_visible:
            for i in (self._id_fill_cap, self._id_fill_rect):
                if i:
                    self.itemconfigure(i, fill=self._fill)
            return
        pad = self.HANDLE_R + 2
        h   = self.winfo_height()
        w   = self.winfo_width()
        if w < 4 or h < 4:
            return
        cy  = h // 2
        th  = self.TRACK_H
        sx  = self._val_to_x(self._trim_start)
        ex  = self._val_to_x(self._trim_end)

        # Excluded zones: flat dimmed blocks that follow the rounded track ends
        dim = _mix_hex(BAR_TRACK, _c("#000000"), 0.6)
        if sx > pad + 1:
            if sx - pad >= 2 * th:
                self.create_line(pad+th, cy, pad+th+1, cy, width=2*th, capstyle="round",
                                 fill=dim, tags="trim_overlay")
                self.create_rectangle(pad+th, cy-th, sx, cy+th,
                                      fill=dim, outline="", tags="trim_overlay")
            else:
                self._cap_zone(pad, sx, cy, th, pad, w, True, dim)
        if ex < w - pad - 1:
            if (w - pad) - ex >= 2 * th:
                self.create_line(w-pad-th-1, cy, w-pad-th, cy, width=2*th, capstyle="round",
                                 fill=dim, tags="trim_overlay")
                self.create_rectangle(ex, cy-th, w-pad-th, cy+th,
                                      fill=dim, outline="", tags="trim_overlay")
            else:
                self._cap_zone(ex, w - pad, cy, th, pad, w, False, dim)

        # Trim handles: thin white brackets  [  ]  whose little flags point
        # into the kept section (clearly different from the round playhead pill)
        # Brackets stand clear of the track (taller than the playhead) and are
        # built from solid rectangles, so the flags sit flush on the track top
        # and bottom with no notch where flag meets bar.
        y0, y1, fl = 2, h - 2, 5
        clr = self.TRIM_CLR
        tt, tb = cy - th, cy + th          # track top / bottom edge
        for (bx, d) in ((sx, 1), (ex, -1)):
            xa, xb = sorted((bx - 1, bx + 2))
            self.create_rectangle(xa, y0, xb, y1, fill=clr, outline="",
                                  tags="trim_overlay")                    # bar
            fx0, fx1 = sorted((bx - 1, bx + d * fl))
            fx1 += 1
            self.create_rectangle(fx0, y0, fx1, y0 + 2, fill=clr, outline="",
                                  tags="trim_overlay")                    # top flag
            self.create_rectangle(fx0, y1 - 2, fx1, y1, fill=clr, outline="",
                                  tags="trim_overlay")                    # bottom flag

        # The playhead can sit anywhere, also outside the trim lines. When it is
        # right on a trim line it is drawn a little inside it (right of the start
        # line, left of the end line) so it doesn't sit on top of the bracket.
        if self._id_handle:
            gap, tol = 6, 3
            hx = self._val_to_x(self._value)
            if ex - sx >= 2 * gap + 2:
                if sx - tol <= hx <= sx + gap:
                    hx = sx + gap
                elif ex - gap <= hx <= ex + tol:
                    hx = ex - gap
            self.coords(self._id_handle, hx, 3, hx, h - self.PH_PAD_BOTTOM)
            # Fill: square ends only. It starts exactly on the left trim
            # line (no rounded cap, so no gap against the bracket) and ends
            # flush on the playhead.
            kept = _mix_hex(self._fill, BAR_TRACK, self.KEPT_TINT)
            self.itemconfigure(self._id_fill_poly, state="hidden")
            self.itemconfigure(self._id_fill_cap, state="hidden", fill=kept)
            self.itemconfigure(self._id_fill_rect, state="normal", fill=kept)
            self.coords(self._id_fill_rect, sx, cy - th, max(sx, min(hx, ex)), cy + th)
        # Keep playhead on top
        if self._id_handle:
            self.tag_raise(self._id_handle)

    def _nearest_trim(self, x):
        sx = self._val_to_x(self._trim_start)
        ex = self._val_to_x(self._trim_end)
        ds = abs(x - sx)
        de = abs(x - ex)
        hit = self.TRIM_HIT
        if ds <= hit and ds <= de:
            return "start"
        if de <= hit:
            return "end"
        return None

    def _x_to_val(self, x):
        """While the playhead is dragged, snap it to a trim line when close."""
        val = super()._x_to_val(x)
        if self._snap_playhead and self._trim_visible:
            vx = self._val_to_x(val)
            sx = self._val_to_x(self._trim_start)
            ex = self._val_to_x(self._trim_end)
            ds, de = abs(vx - sx), abs(vx - ex)
            if min(ds, de) <= self.PH_SNAP_PX:
                return self._trim_start if ds <= de else self._trim_end
        return val

    def _snap_to_playhead(self, raw_val):
        """If the raw drag value is within SNAP_PX of the playhead, snap to it."""
        hx    = self._val_to_x(self._value)
        raw_x = self._val_to_x(raw_val)
        if abs(raw_x - hx) <= self.SNAP_PX:
            return self._value
        return raw_val

    def _trim_press(self, event):
        which = self._nearest_trim(event.x) if self._trim_visible else None
        if which:
            self._trim_drag = which
            # Pause and freeze progress bar so seeks show a clear still frame
            if self._app:
                self._app._seeking = True
                if self._app._mpv_playing:
                    self._app._mpv_pause()
                    self._app._set_play_icon(False)
        else:
            self._trim_drag = None
            self._snap_playhead = True
            self._press(event)

    def _trim_drag_(self, event):
        if self._trim_drag == "start":
            val = self._snap_to_playhead(self._x_to_val(event.x))
            self._trim_start = max(0, min(self._trim_end - self._min_trim_gap(), val))
            self._move_items()
            # Seek to trim start so the player shows that frame live
            if self._app and self._app._mpv_duration > 0:
                self._app._mpv_seek(
                    (self._trim_start / 10000.0) * self._app._mpv_duration)
        elif self._trim_drag == "end":
            val = self._snap_to_playhead(self._x_to_val(event.x))
            self._trim_end = max(self._trim_start + self._min_trim_gap(), min(10000, val))
            self._move_items()
            # Seek to trim end so the player shows that frame live
            if self._app and self._app._mpv_duration > 0:
                self._app._mpv_seek(
                    (self._trim_end / 10000.0) * self._app._mpv_duration)
        else:
            self._drag(event)

    def _trim_cursor(self, event):
        if self._trim_visible and self._nearest_trim(event.x):
            self.configure(cursor="sb_h_double_arrow")
        else:
            self.configure(cursor="")

    def _min_trim_gap(self):
        try:
            dur = self._app._mpv_duration if self._app else 0
            if dur > 0:
                return max(1, int(10000 / dur))
        except Exception:
            pass
        return 100

    def _trim_release(self, event):
        was_trim_drag = self._trim_drag is not None
        self._trim_drag = None
        if self._app:
            self._app._seeking = False  # re-enable progress bar updates
        if self._dragging:
            self._release(event)
        self._snap_playhead = False
        if was_trim_drag and self._app is not None:
            self._app._apply_trim()


# ── Rounded Button ────────────────────────────────────────────────────────────
class RoundedButton(tk.Canvas):
    RADIUS = 9

    def __init__(self, parent, text="", command=None,
                 fg=TEXT_PRIMARY, bg=_c("#242444"),
                 hover_bg=_c("#2A2A4A"), active_bg=None,
                 font=FONT_UI, padx=10, pady=5,
                 width=0, px_width=0, arrow=False,
                 corners=("tl", "tr", "br", "bl"), **kwargs):
        kwargs.setdefault("highlightthickness", 0)
        kwargs.setdefault("bd", 0)
        kwargs["bg"] = parent.cget("bg") if hasattr(parent, "cget") else BG
        super().__init__(parent, **kwargs)
        self._text       = text
        self._command    = command
        self._fg         = fg
        self._bg         = bg
        self._hover_bg   = hover_bg
        self._act_bg     = active_bg or hover_bg
        self._font       = font
        self._padx       = padx
        self._pady       = pady
        self._fixed_w    = width
        self._px_width   = px_width
        self._arrow      = arrow
        self._corners    = corners
        self._current_bg = bg
        self._pressed    = False
        self._measure()
        self.bind("<Configure>",       lambda e: self._draw())
        self.bind("<Enter>",           self._on_enter)
        self.bind("<Leave>",           self._on_leave)
        self.bind("<ButtonPress-1>",   self._on_press)
        self.bind("<ButtonRelease-1>", self._on_release)
        self.configure(cursor="hand2")
        self.after(1, self._draw)

    def _measure(self):
        import tkinter.font as tkfont
        try:
            f  = tkfont.Font(font=self._font)
            th = f.metrics("linespace")
        except Exception:
            th = 16
        if self._px_width:
            self.configure(width=max(self._px_width, 28))
        elif self._fixed_w:
            f = __import__("tkinter.font", fromlist=["Font"]).Font(font=self._font)
            w = self._fixed_w * f.measure("0") + self._padx * 2
            self.configure(width=max(w, 28))
        h = th + self._pady * 2
        self.configure(height=max(h, 22))

    def _border_color(self, bg):
        try:
            r = int(bg[1:3], 16); g = int(bg[3:5], 16); b = int(bg[5:7], 16)
            r = min(255, r + 28); g = min(255, g + 28); b = min(255, b + 28)
            return f"#{r:02x}{g:02x}{b:02x}"
        except Exception:
            return bg

    def _draw(self):
        self.delete("all")
        w = self.winfo_width()
        h = self.winfo_height()
        if w < 4 or h < 4:
            return
        r = min(self.RADIUS, w // 2, h // 2)
        border = self._border_color(self._current_bg)
        _rounded_rect(self, 0, 0, w, h, r, fill=border, corners=self._corners)
        _rounded_rect(self, 1, 1, w-1, h-1, max(r-1, 1), fill=self._current_bg,
                      corners=self._corners)
        if self._arrow:
            arrow_w = 22
            self.create_text(10, h//2, text=self._text, fill=self._fg,
                             font=self._font, anchor="w", tags="btn_text")
            ar = min(self.RADIUS, h // 2)
            clip_x = w - arrow_w
            self.create_rectangle(clip_x + 1, 2, clip_x + 1 + ar, h - 2,
                                  fill=self._current_bg, outline="", tags="arrow_bg")
            arrow_corners = tuple(c for c in ("tr", "br") if c in self._corners)
            _rounded_rect(self, clip_x + 1, 2, w - 1, h - 2, ar,
                          fill=self._current_bg, tags="arrow_bg",
                          corners=arrow_corners or ("tr", "br"))
            self.create_image(w - arrow_w//2 + 1, h//2,
                              image=ui_icon("chevron-down", 12, self._fg, stroke=2.3),
                              anchor="center", tags="arrow_icon")
        elif self._text == "▾":
            self.create_image(w//2, h//2,
                              image=ui_icon("chevron-down", 12, self._fg, stroke=2.3),
                              anchor="center", tags="btn_text")
        else:
            self.create_text(w//2, h//2, text=self._text, fill=self._fg,
                             font=self._font, anchor="center", tags="btn_text")

    def configure(self, **kw):
        if "text" in kw:
            self._text = kw.pop("text")
            self.after(1, self._draw)
        super().configure(**kw)

    def _on_enter(self, e):
        self._current_bg = self._hover_bg
        self._draw()

    def _on_leave(self, e):
        self._pressed    = False
        self._current_bg = self._bg
        self._draw()

    def _on_press(self, e):
        self._pressed    = True
        self._current_bg = self._act_bg
        self._draw()

    def _on_release(self, e):
        if self._pressed:
            self._pressed    = False
            self._current_bg = self._hover_bg
            self._draw()
            if self._command:
                self._command()


class FolderIconButton(RoundedButton):
    """Same rounded icon button as RoundedButton, but draws a small folder
    glyph out of plain canvas shapes instead of a text/emoji character —
    so it always renders, regardless of which emoji font (if any) is
    installed."""

    def _draw(self):
        super()._draw()
        w = self.winfo_width()
        h = self.winfo_height()
        if w < 4 or h < 4:
            return
        iw, ih = 15, 11
        x0 = w / 2 - iw / 2
        y0 = h / 2 - ih / 2 + 1
        tab_w = iw * 0.45
        # Tab (top-left) then body — two simple rounded-rect outlines read
        # clearly as a folder at this size.
        _rounded_rect(self, x0, y0, x0 + tab_w, y0 + 3, 1,
                      fill="", outline=self._fg, width=1, tags="btn_text")
        _rounded_rect(self, x0, y0 + 2, x0 + iw, y0 + ih, 2,
                      fill="", outline=self._fg, width=1, tags="btn_text")


class Tooltip:
    """Small hover tooltip (needed because icon-only buttons have no label)."""
    import weakref as _weakref
    _live = _weakref.WeakSet()

    def __init__(self, widget, text, delay=450):
        self.w, self.text, self.delay = widget, text, delay
        self._tip = None
        self._job = None
        Tooltip._live.add(self)
        widget.bind("<Enter>", self._schedule, add="+")
        widget.bind("<Leave>", self._hide, add="+")
        # Specific sequences on purpose: a widget that binds <ButtonPress-1>
        # itself would otherwise shadow a generic <ButtonPress> binding.
        widget.bind("<ButtonPress-1>", self._hide, add="+")
        widget.bind("<Unmap>", self._hide, add="+")
        widget.bind("<Destroy>", self._hide, add="+")

    @classmethod
    def hide_all(cls):
        for t in list(cls._live):
            t._hide()

    def _schedule(self, e=None):
        self._cancel()
        try:
            self._job = self.w.after(self.delay, self._show)
        except Exception:
            pass

    def _cancel(self):
        if self._job is not None:
            try:
                self.w.after_cancel(self._job)
            except Exception:
                pass
            self._job = None

    def _show(self):
        self._job = None
        if self._tip is not None:
            return
        try:
            if not self.w.winfo_viewable():
                return
            x = self.w.winfo_rootx() + self.w.winfo_width() // 2
            y = self.w.winfo_rooty() + self.w.winfo_height() + 6
            tw = tk.Toplevel(self.w)
            tw.wm_overrideredirect(True)
            tw.attributes("-topmost", True)
            tk.Label(tw, text=self.text, font=FONT_TINY, fg=TEXT_PRIMARY,
                     bg=_c("#23233a"), padx=8, pady=4, bd=0,
                     highlightthickness=1, highlightbackground=BORDER).pack()
            tw.update_idletasks()
            tw.wm_geometry(f"+{x - tw.winfo_width() // 2}+{y}")
            self._tip = tw
        except Exception:
            self._tip = None

    def _hide(self, e=None):
        self._cancel()
        if self._tip is not None:
            try:
                self._tip.destroy()
            except Exception:
                pass
            self._tip = None


class Pill(tk.Canvas):
    """Rounded container that groups related controls (put children in
    `.inner`). Like BlurAutoClicker's `50 | Clicks Per | Second | Rate`.
    stretch=True makes it fill its parent's width instead of hugging content."""

    def __init__(self, parent, fill=None, border=None, radius=RADIUS,
                 pad=2, stretch=False):
        fill   = fill   or CARD_BG
        border = border or BORDER
        super().__init__(parent, bg=parent.cget("bg"), highlightthickness=0, bd=0)
        self._fill, self._border, self._r, self._pad = fill, border, radius, pad
        self._stretch = stretch
        self._danger = False
        self.inner = tk.Frame(self, bg=fill)
        self._win = self.create_window(pad, pad, window=self.inner, anchor="nw")
        self.inner.bind("<Configure>", self._fit)
        if stretch:
            self.bind("<Configure>", self._on_canvas_cfg)

    def set_danger(self, on):
        """Turn the pill's outline red (used while a destructive button is hovered)."""
        on = bool(on)
        if on != self._danger:
            self._danger = on
            try:
                self._fit()
            except Exception:
                pass

    def _paint(self, w, h):
        self.delete("pillbg")
        _border = "#e8454a" if self._danger else self._border
        _rounded_rect(self, 0, 0, w, h, self._r, fill=_border, tags="pillbg")
        _rounded_rect(self, 1, 1, w - 1, h - 1, max(self._r - 1, 1),
                      fill=self._fill, tags="pillbg")
        self.tag_lower("pillbg")

    def _fit(self, e=None):
        h = self.inner.winfo_reqheight() + self._pad * 2
        if self._stretch:
            w = max(self.winfo_width(), 2)
            self.configure(height=h)
            self.itemconfigure(self._win, width=max(w - self._pad * 2, 1))
        else:
            w = self.inner.winfo_reqwidth() + self._pad * 2
            self.configure(width=w, height=h)
        self._paint(w, h)

    def _on_canvas_cfg(self, e):
        self.itemconfigure(self._win, width=max(e.width - self._pad * 2, 1))
        self._paint(e.width, e.height)

    def divider(self, **grid):
        """Thin vertical divider. Packs by default; pass grid kwargs
        (column=..., row=...) to grid it instead."""
        f = tk.Frame(self.inner, bg=DIVIDER, width=1)
        if grid:
            f.grid(sticky="ns", pady=7, **grid)
        else:
            f.pack(side="left", fill="y", pady=7)
        return f


def _bind_danger_pill(pill, btn):
    """Make the pill around a destructive button glow red while it is hovered."""
    btn.bind("<Enter>", lambda e: pill.set_danger(True), add="+")
    btn.bind("<Leave>", lambda e: pill.set_danger(False), add="+")


class GhostButton(tk.Canvas):
    """Flat button: transparent until hovered/pressed. Use inside a Pill.
    icon="name" draws a vector icon (see ui_icon); arrow=True gives a
    dropdown-style button (text left, chevron right)."""

    def __init__(self, parent, text="", command=None, font=FONT_UI,
                 fg=None, hover_fg=None, hover_bg=None, press_bg=None,
                 padx=12, height=CTRL_H - 4, width=0, arrow=False,
                 icon=None, icon_size=18, icon_filled=False, accent_icon=False,
                 autosize=False, text_left=False, reserve_text=None):
        import tkinter.font as tkfont
        try:
            tw = tkfont.Font(font=font).measure(text) if text else 0
        except Exception:
            tw = 40
        base = (icon_size if icon else 0) + tw + (8 if (icon and text) else 0)
        self._reserve_w = 0
        if reserve_text and icon:
            try:
                self._reserve_w = tkfont.Font(font=font).measure(reserve_text)
            except Exception:
                self._reserve_w = 24
            base = icon_size + 8 + self._reserve_w
        w = width or (base + padx * 2 + (14 if arrow else 0))
        super().__init__(parent, width=w, height=height, bg=parent.cget("bg"),
                         highlightthickness=0, bd=0, cursor="hand2")
        self._text, self._command, self._font = text, command, font
        self._autosize, self._padx = autosize, padx
        self._fg       = fg       or TEXT_MUTED
        self._hover_fg = hover_fg or _c("#ffffff")
        self._hover_bg = hover_bg or CARD_HOVER
        self._press_bg = press_bg or _c("#12121e")
        self._arrow = arrow
        self._icon, self._icon_size, self._icon_filled = icon, icon_size, icon_filled
        self._accent_icon = accent_icon
        self._text_left = text_left
        self._state = "idle"
        self.bind("<Configure>",       lambda e: self._draw())
        self.bind("<Enter>",           self._on_enter)
        self.bind("<Leave>",           self._on_leave)
        self.bind("<ButtonPress-1>",   self._on_press)
        self.bind("<ButtonRelease-1>", self._on_release)
        self.after(1, self._draw)

    def _draw(self):
        self.delete("all")
        w, h = self.winfo_width(), self.winfo_height()
        if w < 4 or h < 4:
            return
        if self._state != "idle":
            bg = self._press_bg if self._state == "press" else self._hover_bg
            _rounded_rect(self, 0, 0, w, h, max(RADIUS - 2, 4), fill=bg)
        col = self._fg if self._state == "idle" else self._hover_fg
        if self._icon and self._reserve_w:
            # Fixed slot: icon stays put at the right, the label (if any) sits to
            # its left, so nothing moves when the label appears or disappears.
            icol = ACCENT if self._accent_icon else col
            self.create_image(w - self._padx - self._icon_size // 2, h // 2,
                              image=ui_icon(self._icon, self._icon_size, icol,
                                            filled=self._icon_filled),
                              anchor="center")
            if self._text:
                self.create_text(w - self._padx - self._icon_size - 8, h // 2,
                                 text=self._text, fill=col, font=self._font, anchor="e")
        elif self._icon and self._text:                     # icon + label side by side
            import tkinter.font as tkfont
            tw = tkfont.Font(font=self._font).measure(self._text)
            gap = 8
            x0 = (w - (self._icon_size + gap + tw)) // 2
            icol = ACCENT if self._accent_icon else col
            if self._text_left:                             # label, then icon
                self.create_text(x0, h // 2, text=self._text,
                                 fill=col, font=self._font, anchor="w")
                self.create_image(x0 + tw + gap + self._icon_size // 2, h // 2,
                                  image=ui_icon(self._icon, self._icon_size, icol,
                                                filled=self._icon_filled),
                                  anchor="center")
            else:
                self.create_image(x0 + self._icon_size // 2, h // 2,
                                  image=ui_icon(self._icon, self._icon_size, icol,
                                                filled=self._icon_filled),
                                  anchor="center")
                self.create_text(x0 + self._icon_size + gap, h // 2, text=self._text,
                                 fill=col, font=self._font, anchor="w")
        elif self._icon:
            icol = ACCENT if self._accent_icon else col
            self.create_image(w // 2, h // 2,
                              image=ui_icon(self._icon, self._icon_size, icol,
                                            filled=self._icon_filled),
                              anchor="center")
        elif self._arrow:
            # Arrow sits on its own small rounded background chip, and a label
            # that is too long is cut with "…" before the chip instead of
            # running over / under the arrow.
            chip_w, chip_m = 18, 3
            cx0, cx1 = w - chip_m - chip_w, w - chip_m
            inset = max(4, (h - 20) // 2)
            if self._state == "idle":
                chip = self.cget("bg")          # same colour as the background
            else:
                chip = self._press_bg if self._state == "press" else self._hover_bg
            _rounded_rect(self, cx0, inset, cx1, h - inset, 6, fill=chip)
            label = _ellipsize(self._text, self._font, cx0 - 4 - 10)
            self.create_text(10, h // 2, text=label, fill=col,
                             font=self._font, anchor="w")
            self.create_image((cx0 + cx1) // 2, h // 2,
                              image=ui_icon("chevron-down", 12, col, stroke=2.3),
                              anchor="center")
        else:
            self.create_text(w // 2, h // 2, text=self._text, fill=col,
                             font=self._font, anchor="center")

    def configure(self, cnf=None, **kw):
        if "text" in kw:
            self._text = kw.pop("text")
            if getattr(self, "_autosize", False) and not self._reserve_w:
                import tkinter.font as tkfont
                try:
                    tw = tkfont.Font(font=self._font).measure(self._text) if self._text else 0
                except Exception:
                    tw = 40
                base = (self._icon_size if self._icon else 0) + tw + (8 if (self._icon and self._text) else 0)
                tk.Canvas.configure(self, width=base + self._padx * 2)
            self.after(1, self._draw)
        if cnf or kw:
            super().configure(cnf, **kw)

    def _on_enter(self, e):
        if self._state != "press":
            self._state = "hover"
            self._draw()

    def _on_leave(self, e):
        self._state = "idle"
        self._draw()

    def _on_press(self, e):
        self._state = "press"
        self._draw()

    def _on_release(self, e):
        if self._state == "press":
            self._state = "hover"
            self._draw()
            if self._command:
                self._command()


_FONT_CACHE = {}


def _ellipsize(text, font, maxw, keep="end"):
    """Shorten `text` with an ellipsis so it fits in `maxw` pixels.
    keep="end" keeps the start of the text, keep="start" keeps the tail
    (better for file paths)."""
    import tkinter.font as tkfont
    try:
        f = _FONT_CACHE.get(font)
        if f is None:
            f = _FONT_CACHE[font] = tkfont.Font(font=font)
        if maxw <= 0 or f.measure(text) <= maxw:
            return text
        ell = "\u2026"
        while len(text) > 1:
            text = text[:-1] if keep == "end" else text[1:]
            cand = text + ell if keep == "end" else ell + text
            if f.measure(cand) <= maxw:
                return cand
        return ell
    except Exception:
        return text


class RoundedPopup(tk.Canvas):
    """Floating dropdown panel: rounded corners, thin border, same look as
    Pill. Put rows in `.inner`. place() it like any widget; give place() a
    width to make it match a button."""

    def __init__(self, parent, fill=None, border=None, radius=RADIUS + 2, pad=5):
        self._fill   = fill   or CARD_BG
        self._border = border or _c("#33335a")
        # The canvas background IS the border colour; the window itself is then
        # clipped to a rounded shape (SetWindowRgn), so the corners are really
        # transparent and show whatever is underneath the dropdown.
        super().__init__(parent, bg=self._border, highlightthickness=0, bd=0)
        self._r, self._pad = radius, pad
        self._rgn_size = None
        self.inner = tk.Frame(self, bg=self._fill)
        self._win = self.create_window(pad, pad, window=self.inner, anchor="nw")
        self.inner.bind("<Configure>", self._fit)
        self.bind("<Configure>", self._on_cfg)

    def lift(self, aboveThis=None):
        # tk.Canvas redefines lift()/tkraise() to raise canvas *items*, which
        # raises a TclError here. Raise the widget itself above its siblings.
        if aboveThis is None:
            self.tk.call("raise", self._w)
        else:
            self.tk.call("raise", self._w, str(aboveThis))
    tkraise = lift

    def _fit(self, e=None):
        self.configure(width=self.inner.winfo_reqwidth() + self._pad * 2,
                       height=self.inner.winfo_reqheight() + self._pad * 2)

    def _clip_round(self, w, h):
        """Clip this window to a rounded rectangle (real transparent corners)."""
        if self._rgn_size == (w, h):
            return
        try:
            import ctypes
            gdi, usr = ctypes.windll.gdi32, ctypes.windll.user32
            gdi.CreateRoundRectRgn.restype = ctypes.c_void_p
            gdi.CreateRoundRectRgn.argtypes = [ctypes.c_int] * 6
            usr.SetWindowRgn.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_int]
            rgn = gdi.CreateRoundRectRgn(0, 0, w + 1, h + 1, self._r * 2, self._r * 2)
            if usr.SetWindowRgn(ctypes.c_void_p(self.winfo_id()), ctypes.c_void_p(rgn), True):
                self._rgn_size = (w, h)       # the system now owns the region
        except Exception:
            pass

    def _on_cfg(self, e):
        self.itemconfigure(self._win, width=max(e.width - self._pad * 2, 1))
        self.delete("popbg")
        # 1px border = the canvas background showing around the inner fill
        _rounded_rect(self, 1, 1, e.width - 1, e.height - 1, max(self._r - 1, 1),
                      fill=self._fill, tags="popbg")
        self.tag_lower("popbg")
        self._clip_round(e.width, e.height)


class PopupRow(tk.Canvas):
    """One row inside a RoundedPopup, with a rounded hover highlight.
    Optional: check (None = no checkbox, True/False = checkbox state),
    subtitle (small line under the text), right_text (e.g. a count),
    icon (accent icon on the left), active (marks the current choice)."""

    def __init__(self, parent, text, command=None, font=FONT_UI, height=30,
                 active=False, check=None, subtitle="", right_text="",
                 icon=None, on_wheel=None, on_right=None, on_middle=None,
                 count_left=False, count_col="9999"):
        super().__init__(parent, width=10, height=height, bg=parent.cget("bg"),
                         highlightthickness=0, bd=0, cursor="hand2")
        self._count_left = count_left      # show right_text as a number column on the left
        self._count_col  = count_col       # sample text that sets the column's width
        self._text, self._command, self._font = text, command, font
        self._active, self._check = active, check
        self._sub, self._right, self._icon = subtitle, right_text, icon
        self._hover = False
        self.bind("<Configure>", lambda e: self._draw())
        self.bind("<Enter>", self._on_enter)
        self.bind("<Leave>", self._on_leave)
        self.bind("<ButtonRelease-1>", self._on_click)
        if on_wheel:
            self.bind("<MouseWheel>", on_wheel)
        if on_right:
            self.bind("<Button-3>", on_right)
        if on_middle:
            self.bind("<Button-2>", on_middle)

    def set_active(self, v):
        self._active = v
        self._draw()

    def set_checked(self, v):
        self._check = v
        self._draw()

    def _on_enter(self, e):
        self._hover = True
        self._draw()

    def _on_leave(self, e):
        self._hover = False
        self._draw()

    def _on_click(self, e):
        if self._command and 0 <= e.x < self.winfo_width() and 0 <= e.y < self.winfo_height():
            self._command()

    def _draw(self):
        try:
            self.delete("all")
            w, h = self.winfo_width(), self.winfo_height()
        except Exception:
            return
        if w < 16 or h < 8:
            return
        cy = h // 2
        if self._hover:
            _rounded_rect(self, 1, 1, w - 1, h - 1, 7, fill=_c("#2c2c50"))
        elif self._active:
            _rounded_rect(self, 1, 1, w - 1, h - 1, 7, fill=_c("#2A2A4A"))
        checked = bool(self._check)
        if self._check is not None:
            fg = _c("#ffffff") if (self._hover or checked) else TEXT_MUTED
        else:
            fg = _c("#ffffff") if (self._hover or self._active) else TEXT_MUTED
        x = 12
        if self._check is not None:
            if checked:
                _rounded_rect(self, x, cy - 8, x + 16, cy + 8, 5, fill=ACCENT)
                self.create_image(x + 8, cy, anchor="center",
                                  image=ui_icon("check", 12, ACCENT_FG, stroke=2.6))
            else:
                _rounded_rect(self, x, cy - 8, x + 16, cy + 8, 5,
                              fill=_c("#242444"), outline=_c("#52527e"), width=1)
            x += 26
        elif self._icon:
            self.create_image(x + 9, cy, anchor="center",
                              image=ui_icon(self._icon, 18, ACCENT))
            x += 28
        if self._count_left and self._right:
            # number column on the left (left-aligned); its width comes from
            # count_col (as many digits as the biggest count in the list)
            import tkinter.font as _tkf
            try:
                _col_w = _tkf.Font(font=FONT_TINY).measure(self._count_col)
            except Exception:
                _col_w = 30
            self.create_text(x, cy, text=self._right, anchor="w",
                             fill=TEXT_DIM, font=FONT_TINY)
            x += _col_w + 12
        right_w = 12
        if self._right and not self._count_left:
            self.create_text(w - 12, cy, text=self._right, anchor="e",
                             fill=TEXT_DIM, font=FONT_TINY)
            right_w = 12 + len(self._right) * 6 + 8
        elif self._active and self._check is None and not self._icon:
            self.create_image(w - 16, cy, anchor="center",
                              image=ui_icon("check", 12, ACCENT, stroke=2.4))
            right_w = 32
        room = max(w - x - right_w, 20)
        if self._sub:
            self.create_text(x, cy - 7, text=_ellipsize(self._text, self._font, room),
                             anchor="w", fill=fg, font=self._font)
            self.create_text(x, cy + 8, text=_ellipsize(self._sub, FONT_TINY, room, keep="start"),
                             anchor="w", fill=TEXT_DIM, font=FONT_TINY)
        else:
            self.create_text(x, cy, text=_ellipsize(self._text, self._font, room),
                             anchor="w", fill=fg, font=self._font)


class SpeedRow(PopupRow):
    """Centered label row for the playback-speed dropdown; the current speed
    is shown in the accent colour but stays in the list."""

    def _draw(self):
        try:
            self.delete("all")
            w, h = self.winfo_width(), self.winfo_height()
        except Exception:
            return
        if w < 16 or h < 8:
            return
        if self._hover:
            _rounded_rect(self, 1, 1, w - 1, h - 1, 7, fill=_c("#2c2c50"))
        elif self._active:
            _rounded_rect(self, 1, 1, w - 1, h - 1, 7, fill=_c("#2A2A4A"))
        if self._active:
            fg = ACCENT
        else:
            fg = _c("#ffffff") if self._hover else TEXT_MUTED
        self.create_text(w // 2, h // 2, text=self._text, fill=fg, font=self._font)


class TrackRow(PopupRow):
    """Audio-track row: tick box, name and a small volume slider (0-200 %).
    Click the name = tick/untick the track. Drag the slider = volume (applied
    on release). Right-click the slider = back to 100 %."""
    SL_W  = 80
    PCT_W = 38
    MAXV  = 2.0

    def __init__(self, parent, text, on_toggle, on_volume, vol=1.0,
                 checked=False, height=30):
        super().__init__(parent, text, None, font=FONT_UI, height=height,
                         check=checked)
        self._on_toggle, self._on_volume = on_toggle, on_volume
        self._vol = vol
        self._drag = False
        self.unbind("<ButtonRelease-1>")
        self.bind("<ButtonPress-1>",   self._press)
        self.bind("<B1-Motion>",       self._motion)
        self.bind("<ButtonRelease-1>", self._release)
        self.bind("<Button-3>",        self._right)

    def set_vol(self, v):
        self._vol = v
        self._draw()

    def _sl_x0(self):
        return self.winfo_width() - 12 - self.PCT_W - self.SL_W

    def _in_slider(self, x):
        return x >= self._sl_x0() - 8

    def _val_at(self, x):
        v = (x - self._sl_x0()) / self.SL_W * self.MAXV
        v = max(0.0, min(self.MAXV, v))
        return round(round(v * 10) / 10, 1)     # snap to steps of 10 % (0 - 200 %)

    def _set(self, v, final):
        self._vol = v
        self._draw()
        self._on_volume(v, final)

    def _press(self, e):
        self._drag = self._in_slider(e.x)
        if self._drag:
            self._set(self._val_at(e.x), False)

    def _motion(self, e):
        if self._drag:
            self._set(self._val_at(e.x), False)

    def _release(self, e):
        if self._drag:
            self._drag = False
            self._set(self._val_at(e.x), True)
        elif 0 <= e.x < self.winfo_width() and 0 <= e.y < self.winfo_height():
            self._on_toggle()

    def _right(self, e):
        if self._in_slider(e.x):
            self._set(1.0, True)

    def _draw(self):
        try:
            self.delete("all")
            w, h = self.winfo_width(), self.winfo_height()
        except Exception:
            return
        if w < 16 or h < 8:
            return
        cy = h // 2
        checked = bool(self._check)
        if self._hover:
            _rounded_rect(self, 1, 1, w - 1, h - 1, 7, fill=_c("#2c2c50"))
        fg = _c("#ffffff") if (self._hover or checked) else TEXT_MUTED
        x = 12
        if checked:
            _rounded_rect(self, x, cy - 8, x + 16, cy + 8, 5, fill=ACCENT)
            self.create_image(x + 8, cy, anchor="center",
                              image=ui_icon("check", 12, ACCENT_FG, stroke=2.6))
        else:
            _rounded_rect(self, x, cy - 8, x + 16, cy + 8, 5,
                          fill=_c("#242444"), outline=_c("#52527e"), width=1)
        x += 26
        sl0 = self._sl_x0()
        room = max(sl0 - 14 - x, 20)
        self.create_text(x, cy, text=_ellipsize(self._text, self._font, room),
                         anchor="w", fill=fg, font=self._font)
        # volume slider
        frac = self._vol / self.MAXV
        hx = sl0 + frac * self.SL_W
        on = checked
        self.create_line(sl0, cy, sl0 + self.SL_W, cy, fill=_c("#52527e"),
                         width=4, capstyle="butt")
        if hx > sl0 + 1:
            self.create_line(sl0, cy, hx, cy, fill=(ACCENT if on else TEXT_DIM),
                             width=4, capstyle="butt")
        mx = sl0 + self.SL_W / 2                       # 100 % notch
        self.create_line(mx, cy - 9, mx, cy - 6, fill=TEXT_DIM)
        self.create_rectangle(hx - 3, cy - 6, hx + 3, cy + 6,
                              fill=(_c("#ffffff") if on else TEXT_MUTED), outline="")
        off = abs(self._vol - 1.0) > 0.004
        self.create_text(w - 12, cy, text=f"{round(self._vol * 100)}%", anchor="e",
                         fill=(ACCENT if (off and on) else TEXT_DIM), font=FONT_TINY)


class ThemeRow(PopupRow):
    """Row of the theme dropdown: colour swatch + name, rounded hover highlight,
    check mark on the current theme."""

    def __init__(self, parent, name, command, current=False):
        super().__init__(parent, name, command, font=FONT_UI, height=32, active=current)
        self._name = name

    def _draw(self):
        try:
            self.delete("all")
            w, h = self.winfo_width(), self.winfo_height()
        except Exception:
            return
        if w < 16 or h < 8:
            return
        cy = h // 2
        if self._hover:
            _rounded_rect(self, 1, 1, w - 1, h - 1, 7, fill=_c("#2c2c50"))
        elif self._active:
            _rounded_rect(self, 1, 1, w - 1, h - 1, 7, fill=_c("#2A2A4A"))
        d, y = 14, cy - 7
        for i, colr in enumerate(_theme_swatch_colors(self._name)):
            _aa_oval(self, 12 + i * 15, y, 12 + i * 15 + d, y + d,
                     fill=colr, outline=_c("#52527e"), width=1)
        fg = _c("#ffffff") if (self._hover or self._active) else TEXT_MUTED
        self.create_text(12 + 47 + 8, cy, text=self._text, anchor="w", fill=fg, font=self._font)
        if self._active:
            self.create_image(w - 16, cy, anchor="center",
                              image=ui_icon("check", 12, ACCENT, stroke=2.4))


def _hex_to_rgb255(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def _mix_hex(c1, c2, t):
    """Blend colour c1 towards c2 by t (0..1); both '#rrggbb'."""
    a, b = _hex_to_rgb255(c1), _hex_to_rgb255(c2)
    return "#%02x%02x%02x" % tuple(int(round(x + (y - x) * t)) for x, y in zip(a, b))


_TB_CACHE = {}


class TitleButton(tk.Canvas):
    """Window-control button for the custom title bar (min / max / restore / close).
    Icons are drawn anti-aliased and pixel-aligned (thin 1px strokes); hover/press
    show a soft rounded square, close turns red. Canvas size must be odd x odd so
    the glyph centre lands on a pixel centre."""
    S = 8   # supersampling factor

    def __init__(self, parent, kind, command, danger=False, w=37, h=29):
        super().__init__(parent, width=w, height=h, bg=parent.cget("bg"),
                         highlightthickness=0, bd=0)
        self._kind, self._command, self._danger = kind, command, danger
        self._state = "idle"
        self._img = None
        self.bind("<Configure>",       lambda e: self._draw())
        self.bind("<Enter>",           lambda e: self._set_state("hover"))
        self.bind("<Leave>",           lambda e: self._set_state("idle"))
        self.bind("<ButtonPress-1>",   lambda e: self._set_state("press"))
        self.bind("<ButtonRelease-1>", self._on_release)
        self.after(1, self._draw)

    def set_kind(self, kind):
        self._kind = kind
        self._draw()

    def _set_state(self, st):
        if st != self._state:
            self._state = st
            self._draw()

    def _on_release(self, e):
        inside = 0 <= e.x < self.winfo_width() and 0 <= e.y < self.winfo_height()
        self._state = "hover" if inside else "idle"
        self._draw()
        if inside and self._command:
            self._command()

    def _colors(self):
        bg = self.master.cget("bg")
        if self._state == "idle":
            return bg, bg, TEXT_MUTED
        if self._danger:
            amt = 0.24 if self._state == "hover" else 0.38
            return bg, _mix_hex(bg, "#e8454a", amt), "#ff6b6b"
        amt = 0.09 if self._state == "hover" else 0.15
        return bg, _mix_hex(bg, "#ffffff", amt), "#ffffff"

    def _render(self, w, h, bg, pill, fg):
        S = self.S
        key = (self._kind, w, h, bg, pill, fg)
        ph = _TB_CACHE.get(key)
        if ph is not None:
            return ph
        im = _PILImage.new("RGB", (w * S, h * S), bg)
        d = _PILDraw.Draw(im)
        if pill != bg:
            d.rounded_rectangle([3 * S, 1 * S, (w - 3) * S - 1, (h - 1) * S - 1],
                                radius=7 * S, fill=pill)
        cx, cy = w // 2, h // 2          # pixel containing the centre
        sw = S                           # 1px stroke
        k = self._kind

        def rect(x0, y0, x1, y1):        # pixel-aligned filled rect [x0,x1) x [y0,y1)
            d.rectangle([x0 * S, y0 * S, x1 * S - 1, y1 * S - 1], fill=fg)

        if k == "min":
            rect(cx - 5, cy, cx + 6, cy + 1)
        elif k == "max":
            d.rounded_rectangle([(cx - 5) * S, (cy - 5) * S, (cx + 6) * S - 1, (cy + 6) * S - 1],
                                radius=int(1.5 * S), outline=fg, width=sw)
        elif k == "restore":
            d.rounded_rectangle([(cx - 5) * S, (cy - 2) * S, (cx + 4) * S - 1, (cy + 6) * S - 1],
                                radius=int(1.5 * S), outline=fg, width=sw)
            rect(cx - 2, cy - 5, cx + 6, cy - 4)          # back window: top edge
            rect(cx + 5, cy - 5, cx + 6, cy + 3)          # back window: right edge
        elif k == "close":
            c = (cx + 0.5) * S, (cy + 0.5) * S
            r = 5 * S
            d.line([c[0] - r, c[1] - r, c[0] + r, c[1] + r], fill=fg, width=int(S * 1.25))
            d.line([c[0] - r, c[1] + r, c[0] + r, c[1] - r], fill=fg, width=int(S * 1.25))
        im = im.resize((w, h), _PILImage.LANCZOS)
        ph = _PILTk.PhotoImage(im)
        _TB_CACHE[key] = ph
        return ph

    def _draw(self):
        w, h = int(self.cget("width")), int(self.cget("height"))
        bg, pill, fg = self._colors()
        self.delete("all")
        try:
            self._img = self._render(w, h, bg, pill, fg)
            self.create_image(0, 0, image=self._img, anchor="nw")
        except Exception:
            pass


def _can_recycle(path):
    """True if Windows has a Recycle Bin for the drive this file lives on
    (fixed local drives only - network shares and USB sticks don't)."""
    try:
        drive = os.path.splitdrive(os.path.abspath(path))[0]
        if not drive or drive.startswith("\\\\"):
            return False
        return ctypes.windll.kernel32.GetDriveTypeW(drive + "\\") == 3   # DRIVE_FIXED
    except Exception:
        return False


def _recycle_file(path):
    """Move one file to the Windows Recycle Bin. Returns True on success."""
    class SHFILEOPSTRUCTW(ctypes.Structure):
        _pack_ = 1 if ctypes.sizeof(ctypes.c_void_p) == 4 else 8
        _fields_ = [("hwnd", ctypes.c_void_p), ("wFunc", ctypes.c_uint),
                    ("pFrom", ctypes.c_wchar_p), ("pTo", ctypes.c_wchar_p),
                    ("fFlags", ctypes.c_ushort), ("fAnyOperationsAborted", ctypes.c_int),
                    ("hNameMappings", ctypes.c_void_p), ("lpszProgressTitle", ctypes.c_wchar_p)]
    try:
        full = os.path.abspath(path)
        op = SHFILEOPSTRUCTW()
        op.hwnd = None
        op.wFunc = 0x0003                      # FO_DELETE
        op.pFrom = full + "\0"                 # list is double-NUL terminated
        op.pTo = None
        # FOF_ALLOWUNDO (= Recycle Bin) | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI
        op.fFlags = 0x0040 | 0x0010 | 0x0004 | 0x0400
        shell32 = ctypes.windll.shell32
        shell32.SHFileOperationW.argtypes = [ctypes.POINTER(SHFILEOPSTRUCTW)]
        shell32.SHFileOperationW.restype = ctypes.c_int
        rc = shell32.SHFileOperationW(ctypes.byref(op))
        return rc == 0 and not op.fAnyOperationsAborted and not os.path.exists(full)
    except Exception:
        return False


def _round_mask(w, h, r):
    S = 4
    m = _PILImage.new("L", (w * S, h * S), 0)
    _PILDraw.Draw(m).rounded_rectangle([0, 0, w * S - 1, h * S - 1], radius=r * S, fill=255)
    return m.resize((w, h), _PILImage.LANCZOS)


def _sv_image(hue, w, h, bg):
    """Saturation/value square for the colour picker: white at the left,
    the pure hue at the right, fading to black at the bottom."""
    base = _PILImage.new("RGB", (w, h), _rgb_to_hex(*colorsys.hsv_to_rgb(hue, 1, 1)))
    grad = _PILImage.linear_gradient("L")
    white_mask = grad.rotate(-90).resize((w, h), _PILImage.BILINEAR)   # 255 left -> 0 right
    black_mask = grad.resize((w, h), _PILImage.BILINEAR)               # 0 top -> 255 bottom
    img = _PILImage.composite(_PILImage.new("RGB", (w, h), (255, 255, 255)), base, white_mask)
    img = _PILImage.composite(_PILImage.new("RGB", (w, h), (0, 0, 0)), img, black_mask)
    out = _PILImage.new("RGB", (w, h), bg)
    out.paste(img, (0, 0), _round_mask(w, h, 8))
    return out


def _hue_bar_image(w, h, bg):
    row = _PILImage.new("RGB", (w, 1))
    row.putdata([tuple(int(round(c * 255)) for c in colorsys.hsv_to_rgb(i / (w - 1), 1, 1))
                 for i in range(w)])
    img = row.resize((w, h), _PILImage.NEAREST)
    out = _PILImage.new("RGB", (w, h), bg)
    out.paste(img, (0, 0), _round_mask(w, h, h // 2))
    return out


class ToggleSwitch(tk.Canvas):
    """Anti-aliased on/off switch with a short slide animation. Colours come
    from the active theme at draw time, so live theme switching just works."""
    W, H = 42, 24

    def __init__(self, parent, value=False, command=None):
        super().__init__(parent, width=self.W, height=self.H, bg=parent.cget("bg"),
                         highlightthickness=0, bd=0, cursor="hand2")
        self._value, self._command = bool(value), command
        self._pos = 1.0 if self._value else 0.0
        self._hover = False
        self._anim = None
        self.bind("<Enter>", lambda e: self._set_hover(True))
        self.bind("<Leave>", lambda e: self._set_hover(False))
        self.bind("<ButtonRelease-1>", self._on_release)
        self.after(1, self._draw)

    def get(self):
        return self._value

    def set(self, value, animate=True):
        self._value = bool(value)
        if animate:
            self._start_anim()
        else:
            self._pos = 1.0 if self._value else 0.0
            self._draw()

    def _set_hover(self, v):
        self._hover = v
        self._draw()

    def _on_release(self, e):
        if 0 <= e.x < self.winfo_width() and 0 <= e.y < self.winfo_height():
            self._value = not self._value
            self._start_anim()
            if self._command:
                self._command(self._value)

    def _start_anim(self):
        if self._anim is not None:
            try:
                self.after_cancel(self._anim)
            except Exception:
                pass
        self._step()

    def _step(self):
        target = 1.0 if self._value else 0.0
        d = target - self._pos
        if abs(d) < 0.04:
            self._pos, self._anim = target, None
        else:
            self._pos += d * 0.45
            self._anim = self.after(16, self._step)
        self._draw()

    def _draw(self):
        try:
            self.delete("all")
        except Exception:
            return
        w, h, t = self.W, self.H, self._pos
        off_track  = _c("#33335a") if self._hover else _c("#2A2A4A")
        track  = _mix_hex(off_track, ACCENT, t)
        border = _mix_hex(_c("#52527e"), ACCENT, t)
        _rounded_rect(self, 0, 0, w, h, h // 2, fill=border)
        _rounded_rect(self, 1, 1, w - 1, h - 1, h // 2 - 1, fill=track)
        d = h - 8
        x = 4 + t * (w - 8 - d)
        _aa_oval(self, x, 4, x + d, 4 + d, fill=_mix_hex(TEXT_MUTED, "#ffffff", t))


# ── Main App ──────────────────────────────────────────────────────────────────
# -- Phone sync (ClipSync Android app) ---------------------------------------
import http.client as _http_client
import urllib.request as _urlreq
import urllib.parse as _urlparse
import io as _io

_PHONE_PORT = 8765


def _phone_ping(ip, timeout=1.5):
    """True if a ClipSync receiver answers on this address."""
    try:
        with _urlreq.urlopen("http://%s:%d/ping" % (ip, _PHONE_PORT), timeout=timeout) as r:
            return json.loads(r.read().decode("utf-8", "ignore")).get("app") == "ClipSync"
    except Exception:
        return False


def _local_ipv4s():
    ips = set()
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(("10.255.255.255", 1))          # sends nothing, just picks the LAN adapter
        ips.add(s.getsockname()[0])
        s.close()
    except Exception:
        pass
    try:
        for ip in socket.gethostbyname_ex(socket.gethostname())[2]:
            ips.add(ip)
    except Exception:
        pass
    return [i for i in ips if i.startswith(("192.168.", "10.", "172."))]


def _find_phone(saved_ip="", manual_ip=""):
    """Phone address, or "". Tries the typed-in address, then the last one that
    worked, then scans the local network (/24) for the ClipSync receiver."""
    if manual_ip:
        return manual_ip if _phone_ping(manual_ip) else ""
    if saved_ip and _phone_ping(saved_ip, 1.0):
        return saved_ip
    mine = set(_local_ipv4s())
    subnets = sorted({i.rsplit(".", 1)[0] for i in mine})
    cands = ["%s.%d" % (n, h) for n in subnets for h in range(1, 255)
             if "%s.%d" % (n, h) not in mine]

    def probe(ip):
        try:
            with socket.create_connection((ip, _PHONE_PORT), timeout=0.4):
                pass
        except Exception:
            return None
        return ip if _phone_ping(ip) else None

    ex = concurrent.futures.ThreadPoolExecutor(max_workers=64)
    try:
        futs = [ex.submit(probe, ip) for ip in cands]
        for f in concurrent.futures.as_completed(futs):
            r = f.result()
            if r:
                return r
    finally:
        ex.shutdown(wait=False, cancel_futures=True)
    return ""


def _phone_request(ip, token, method, path, body=None, size=0, progress=None, stats=None):
    """One request to the phone. Streams `body` (file-like) in chunks.
    If `stats` (a dict) is given it gets connect / send / wait-for-phone times."""
    t0 = time.monotonic()
    conn = _http_client.HTTPConnection(ip, _PHONE_PORT, timeout=60)
    try:
        conn.connect()
        try:
            conn.sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            # Windows' default send buffer is small and caps Wi-Fi throughput
            conn.sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 2 * 1024 * 1024)
        except Exception:
            pass
        t1 = time.monotonic()
        conn.putrequest(method, _urlparse.quote(path))
        conn.putheader("X-Token", token)
        conn.putheader("Content-Length", str(size))
        conn.endheaders()
        if body is not None:
            sent = 0
            while True:
                chunk = body.read(1024 * 1024)
                if not chunk:
                    break
                conn.sock.sendall(chunk)
                sent += len(chunk)
                if progress:
                    progress(sent, size)
        t2 = time.monotonic()
        resp = conn.getresponse()
        data = resp.read()
        if stats is not None:
            stats.update(connect=t1 - t0, send=t2 - t1, wait=time.monotonic() - t2)
        return resp.status, data
    finally:
        conn.close()


class VideoPlayerApp(tk.Tk):
    def __init__(self):
        super().__init__()
        # Background threads must never touch Tk directly: they hand work to the
        # main thread through this queue (drained by _pump_main_q below).
        self._main_q = queue.SimpleQueue()
        self.after(15, self._pump_main_q)
        self.title("Feather Player")
        self.geometry("1160x650")
        self.minsize(860, 480)
        self.configure(bg=BG)
        try:
            self.iconbitmap(resource_path(os.path.join("pics", "feather1.ico")))
        except Exception:
            pass

        # Load icon
        def _load_icon():
            try:
                from PIL import Image, ImageTk
                _icon_path = resource_path(os.path.join("pics", "feather2.png"))
                img  = Image.open(_icon_path).convert("RGBA").resize((64, 64), Image.BILINEAR)
                photo = ImageTk.PhotoImage(img)
                self.after(0, lambda: (setattr(self, "_icon_photo", photo),
                                       self.iconphoto(True, photo)))
            except Exception:
                pass
        threading.Thread(target=_load_icon, daemon=True).start()

        # Transport button images (drawn vectors — always crisp, no PNGs needed)
        self._btn_imgs = {}
        try:
            _wh = _c("#ffffff")
            self._btn_imgs["play"]  = ui_icon("play",      20, _wh)
            self._btn_imgs["pause"] = ui_icon("pause",     20, _wh)
            self._btn_imgs["prev"]  = ui_icon("skip-prev", 18, _wh)
            self._btn_imgs["next"]  = ui_icon("skip-next", 18, _wh)
            self._btn_imgs["vol"]   = ui_icon("volume",    18, _wh)
            self._btn_imgs["mute"]  = ui_icon("mute",      18, _wh)
        except Exception:
            pass

        # ── State ─────────────────────────────────────────────────────────────
        self.folder_path     = None
        self.videos          = []
        self.thumbnails      = {}
        self.current_idx     = None
        self._selected_clips = set()
        self._ffmpeg         = find_ffmpeg()
        self._durations      = {}
        self._panel_width    = 290
        self._resize_job     = None
        self._win_sizing       = False   # True while the window is being resized with the mouse
        self._win_size_pending = False   # set on release -> one final layout pass
        self._thumb_thread_id = 0
        self._scan_thread_id  = 0
        self._ui_ready       = False
        self._visible_count  = 50
        self._rb_dragging    = False
        self._rb_toggled     = set()
        self._ignore_next_click = False
        self._starred_clips  = set()
        self._liked_clips    = set()
        self._filter_var     = "All clips"
        self._date_from      = None     # calendar filter: first day (datetime.date) or None
        self._date_to        = None     # calendar filter: last day (same as _date_from for one day)
        self._date_pending   = False    # True after the 1st click, waiting for the end date
        self._date_open      = False
        self._cal_ym         = None     # (year, month) shown in the calendar popup
        self._game_var       = "All"
        self._game_multi     = set()   # right-click multi-select of game categories
        self._dynamic_games  = []
        self._game_norm_map  = {}   # norm_key -> display label (after any user rename)
        self._game_auto_map  = {}   # norm_key -> auto-detected default label
        self._category_renames = {}  # norm_key -> user-chosen display label
        self._keyword_groups = {}    # norm_key -> {"label": str, "keywords": [norm_kw, ...]}
        self._file_stat_cache = {}
        self._settings_open  = False
        self._settings_page  = None
        self._settings_built = False
        self._settings_pending = False
        self._theme_popup    = None
        self._theme_popup_open = False

        # MPV state
        self._mpv            = None   # in-process libmpv instance (python-mpv)
        self._mpv_proc       = None   # kept for back-compat truthiness checks
        self._mpv_hwnd       = None
        self._mpv_generation = 0   # bumped on every (re)start/kill so stale
                                    # property-observer callbacks from a
                                    # previous mpv session know to stop
        self._mpv_duration  = 0.0
        self._mpv_pos       = 0.0
        self._mpv_playing   = False
        self._seeking       = False
        self._seek_last_val = -1
        self._mpv_volume    = 55
        self._mpv_ipc_ready = False
        self._at_trim_end   = False   # True only when auto-paused at trim end
        self._trim_past_ok  = False   # user skipped past the trim end -> let it play on
        self._trim_seek_t   = 0.0     # when that manual skip happened
        self._trim_mode     = False   # False = handles hidden; True = trim mode active

        # Load prefs
        _prefs = self._load_prefs_data()
        _init_vol = _prefs.get("volume", 55)
        self._starred_clips = set(_prefs.get("starred", []))
        self._liked_clips   = set(_prefs.get("liked", []))
        # (old prefs stored this folder under "drive_sync_folder" - still read as a fallback)
        self._star_copy_folder  = _prefs.get("star_copy_folder", _prefs.get("drive_sync_folder", ""))
        self._star_copy_enabled = bool(_prefs.get("star_copy_enabled", False))
        _st = _read_settings()
        self._use_recycle_bin = bool(_st.get("recycle_bin", True))
        self._close_to_tray   = bool(_st.get("close_to_tray", False))
        self._auto_refresh    = bool(_st.get("auto_refresh", True))
        self._loop_clip       = bool(_st.get("loop_clip", False))   # default: no loop
        self._trim_gpu        = bool(_st.get("trim_use_gpu", True))  # GPU encoder first when applying a trim
        self._loop_seek_t     = 0.0    # when the last loop jump happened
        self._loop_prev_playing = False
        try:
            self._see_more_step = int(_st.get("see_more_step", 50))
        except Exception:
            self._see_more_step = 50
        if self._see_more_step not in (25, 50, 100, 200, 500, 1000):
            self._see_more_step = 50
        # Playback speed is saved per clip (path -> speed); a clip without an
        # entry plays at 1x. _play_speed is always the speed of the open clip.
        self._play_speed = 1.0
        self._clip_speed = {}
        try:
            for _p, _v in (_st.get("clip_speeds") or {}).items():
                _v = float(_v)
                if _v in self._SPEEDS and _v != 1.0:
                    self._clip_speed[_p] = _v
        except Exception:
            pass
        # Clips that open at 0:00 instead of the middle: trims baked into a clip /
        # trimmed copies (already cut down), plus the "always start at the
        # beginning" setting
        self._start_always = bool(_st.get("always_start_beginning", False))
        self._start_zero   = set(p for p in (_st.get("start_from_beginning_clips") or [])
                                 if isinstance(p, str))
        # SteelSeries GG Moments clips: play only the first track, no mix at all
        self._steel_first_only = bool(_st.get("steelseries_first_track_only", False))
        # Audio tracks editor (prototype)
        self._at_enabled   = bool(_st.get("audio_tracks_btn", True))
        self._at_defaults  = self._at_clean(_st.get("default_audio_tracks", [1]))
        self._at_def_vol   = {}        # default volume per track number (1.0 = left out)
        try:
            for _k, _v in (_st.get("default_audio_volumes") or {}).items():
                if 1 <= int(_k) <= 6 and 0.0 <= float(_v) <= 2.0 and float(_v) != 1.0:
                    self._at_def_vol[int(_k)] = round(float(_v), 1)
        except Exception:
            pass
        self._at_clip_sel  = {}        # clip path -> tracks picked for that clip
        self._at_probe     = {}        # clip path -> audio stream handler names (ffprobe, background)
        self._at_probe_busy = set()    # paths being probed right now
        self._at_probe_pool = concurrent.futures.ThreadPoolExecutor(max_workers=2)
        self._at_prepared  = None      # audio mix already set up before the clip was loaded
        self._at_clip_vol  = {}        # clip path -> {track number: volume}
        self._at_vol       = {}        # this clip's volumes that differ from the default volumes
        try:
            for _p, _d in (_st.get("clip_audio_volumes") or {}).items():
                _m = {int(k): float(v) for k, v in _d.items()
                      if 1 <= int(k) <= 6 and 0.0 <= float(v) <= 2.0}
                if _m:
                    self._at_clip_vol[_p] = _m
        except Exception:
            pass
        try:
            for _p, _v in (_st.get("clip_audio_tracks") or {}).items():
                self._at_clip_sel[_p] = self._at_clean(_v)
        except Exception:
            pass
        self._at_tracks    = []        # this clip's audio tracks (mpv track-list entries)
        self._at_sel       = None      # selected track numbers (1-based), None = no control
        self._at_ctl       = False     # True when the current clip can be controlled
        self._at_popup     = None
        self._at_rows      = {}
        self._at_menu_shown = False
        self._at_poll_id   = None
        self._loaded_once     = False
        self._remember_size   = bool(_st.get("remember_window_size", True))
        _w = _st.get("window")
        self._saved_win       = _w if isinstance(_w, dict) and _w.get("w") and _w.get("h") else None
        self._size_ready      = not (self._remember_size and self._saved_win)   # don't overwrite the saved size before restoring it
        self._size_save_job   = None
        self._thumbs_dirty    = False
        self._durations     = _prefs.get("durations", {})
        self._trims         = _prefs.get("trims", {})   # {filename: [start_0_10000, end_0_10000]}
        self._trim_enabled  = set(_prefs.get("trim_enabled", []))  # clips with trim toggled on
        self._game_var       = _prefs.get("game_filter", "All")
        self._game_multi     = set(_prefs.get("game_multi", []))
        if self._game_multi:
            self._game_var = "All"
        self._category_renames = _prefs.get("category_renames", {})
        self._keyword_groups   = _prefs.get("keyword_groups", {})
        self._last_folder    = _prefs.get("folder", "")
        self._saved_folders  = _prefs.get("saved_folders", [])
        self._active_folder  = _prefs.get("active_folder", self._last_folder)
        self._checked_folders = _prefs.get("checked_folders", [])
        # Back-compat: seed saved_folders from old single-folder pref
        if self._last_folder and self._last_folder not in self._saved_folders:
            self._saved_folders.append(self._last_folder)
        if self._active_folder and self._active_folder not in self._checked_folders:
            self._checked_folders = [self._active_folder] if self._active_folder else []
        # Ensure active_folder is first so _load_folders opens it as the primary folder
        if self._active_folder and self._checked_folders and self._checked_folders[0] != self._active_folder:
            self._checked_folders = [self._active_folder] + [f for f in self._checked_folders if f != self._active_folder]
        # Cloud sync (copy new clips into a cloud folder)
        self._cloud_enabled = bool(_st.get("cloud_enabled", False))
        _cf = _st.get("cloud_folder", "")
        self._cloud_folder  = _cf if isinstance(_cf, str) else ""
        try:
            self._cloud_since = float(_st.get("cloud_since", 0) or 0)
        except Exception:
            self._cloud_since = 0.0
        self._cloud_copied  = self._cloud_load_copied()
        _cm = _st.get("cloud_mode", "new")
        self._cloud_mode    = _cm if _cm in ("new", "all") else "new"   # which clips get copied
        self._cloud_busy    = False
        _cg = _st.get("cloud_games", [])
        self._cloud_games   = set(x for x in _cg if isinstance(x, str)) if isinstance(_cg, list) else set()
        self._cloud_games_popup = None
        self._cloud_prev    = {}
        self._cloud_status  = "Up to date" if self._cloud_enabled else "Off"
        # Phone sync (off by default; which clips get sent is chosen in Settings)
        self._phone_enabled = bool(_st.get("phone_enabled", False))
        _pk = _st.get("phone_kinds", ["starred"])
        _pk = set(x for x in _pk if x in ("starred", "liked", "trimmed", "all")) if isinstance(_pk, list) else set()
        self._phone_kinds   = _pk or {"starred"}
        self._phone_status  = "Ready. Open the ClipSync app on your phone first"
        self._mpv_volume    = _init_vol

        self._build_titlebar()
        self._build_ui(_init_vol)
        self._start_progress_loop()
        self._start_click_watcher()
        self.after(500, self._win_size_watch)
        self.after(500, self._fit_video_16_9)
        self.after(120, self._restore_saved_size)
        self.after(4000, self._folder_watch_tick)
        self.after(6000, self._cloud_tick)
        self.bind("<Configure>", self._queue_size_save, add="+")
        self.after(3000, lambda: setattr(self, "_sash_armed", True))   # sidebar auto-fit only after startup
        self.after(50, self._apply_dark_titlebar)
        self.after(700, self._refresh_frame)     # repaint the window frame once startup has settled
        self.after(1800, self._refresh_frame)
        self.bind("<FocusIn>", lambda e: self._apply_dark_titlebar() if e.widget is self else None)

        # Start MPV and load last folder once embedded
        self.after(300, self._start_mpv)
        # Build the (hidden) settings page once startup has settled, so the
        # first click on Settings opens it instantly.

        self.bind("<space>", lambda e: None if self._settings_open else self._toggle_play())
        self.bind("<Left>",  lambda e: None if self._settings_open else self._nav_clip("newer"))
        self.bind("<Right>", lambda e: None if self._settings_open else self._nav_clip("older"))
        def _mute_key(e):
            if self._settings_open or isinstance(e.widget, (tk.Entry, tk.Text)):
                return
            self._toggle_mute()
        self.bind("<m>", _mute_key)
        self.bind("<M>", _mute_key)
        def _fs_key(e):
            if (self._settings_open or isinstance(e.widget, (tk.Entry, tk.Text))
                    or e.state & 0x4):          # ignore Ctrl+F etc.
                return
            self._toggle_fullscreen()
        self.bind("<f>", _fs_key)
        self.bind("<F>", _fs_key)
        self.bind("<Escape>", self._fullscreen_escape, add="+")
        # fullscreen sits above the taskbar only while the app is the active window
        def _fs_focus(e, on):
            if self._fullscreen and e.widget is self:
                try:
                    self.attributes("-topmost", on)
                except Exception:
                    pass
        self.bind("<FocusOut>", lambda e: _fs_focus(e, False), add="+")
        self.bind("<FocusIn>",  lambda e: _fs_focus(e, True),  add="+")
        self.bind("<Escape>", self._settings_escape, add="+")
        self.bind("<Button-1>", self._theme_popup_outside, add="+")
        self.bind("<Button-1>", self._cloud_games_outside, add="+")

    # ── Settings page ─────────────────────────────────────────────────────────
    # To add more settings later: inside _build_settings_page, call
    #     card = section("SOME TITLE")           # a rounded card
    #     right = row(card, "Label", "hint")     # returns a frame for your control
    # and put any widget in `right`. Use row_divider(card) between rows.

    def _open_settings(self):
        if self._settings_open:
            return
        Tooltip.hide_all()
        try:
            self._close_all_dropdowns()
            if self._folder_dropdown_open:
                self._toggle_folder_dropdown()
        except Exception:
            pass
        if self._settings_page is None:
            # First time: switch to a blank screen in the settings background
            # (so it is clear the click worked) while the page is built hidden,
            # in small pieces so the app never freezes. The finished page then
            # replaces the blank screen in one go (see _build_settings_page).
            if self._mpv_playing:
                self._mpv_pause()
                self._set_play_icon(False)
            self._build_settings_page()
            self._settings_blank = tk.Frame(self, bg=BG)
            self._pane.pack_forget()
            self._settings_blank.pack(fill="both", expand=True)
            self._settings_pending = True
            self._settings_open = True
            return
        self._show_settings()

    def _show_settings(self):
        if self._settings_open:
            return
        _ot0 = time.monotonic()
        # Don't leave the clip playing behind the settings page
        if self._mpv_playing:
            self._mpv_pause()
            self._set_play_icon(False)
        if self._settings_built:
            self._refresh_star_settings()
            self._refresh_accent_settings()
            self._refresh_cloud_settings()
            self._refresh_cache_info()
            _size = (self.winfo_width(), self.winfo_height())
            if _size != getattr(self, "_settings_layout_size", None):
                # lay the page out off-screen first, so it doesn't reflow in front of you
                self._settings_page.place(x=-10000, y=0, relwidth=1, relheight=1)
                self.update_idletasks()
                self._settings_page.place_forget()
                self._settings_layout_size = _size
        _ot2 = time.monotonic()
        self._pane.pack_forget()
        self._settings_page.pack(fill="both", expand=True)
        self._settings_open = True
        _debug_log("settings open: refresh/layout %.0f ms, swap %.0f ms"
                   % ((_ot2 - _ot0) * 1000, (time.monotonic() - _ot2) * 1000))

    def _close_settings(self):
        if not self._settings_open:
            return
        self._close_theme_popup()
        self._close_cloud_games_popup()
        self._settings_page.pack_forget()
        if getattr(self, "_settings_pending", False):
            self._settings_pending = False
            try:
                self._settings_blank.destroy()
            except Exception:
                pass
        self._pane.pack(fill="both", expand=True)
        self._settings_open = False
        if self._thumbs_dirty:                 # cache was cleared -> rebuild the grid now
            self._thumbs_dirty = False
            self.after(50, self._rebuild_after_cache_clear)

    def _settings_escape(self, event=None):
        if self._cloud_games_popup is not None:
            self._close_cloud_games_popup()
        elif self._theme_popup_open:
            self._close_theme_popup()
        elif self._settings_open:
            self._close_settings()

    def _build_settings_page(self):
        page = tk.Frame(self, bg=BG)
        self._settings_page = page

        # Top bar
        top = tk.Frame(page, bg=BG, padx=16, pady=10)
        top.pack(fill="x")
        back = Pill(top)
        back.pack(side="left")
        _bb = GhostButton(back.inner, "", self._close_settings,
                          icon="arrow-left", icon_size=18, padx=8)
        _bb.pack(side="left")
        Tooltip(_bb, "Back")
        tk.Label(top, text="Settings", font=FONT_TITLE, bg=BG,
                 fg=TEXT_PRIMARY).pack(side="left", padx=(14, 0))
        tk.Frame(page, bg=_c("#242444"), height=1).pack(fill="x")

        # Centered, scrollable content column
        body = tk.Frame(page, bg=BG)
        body.pack(fill="both", expand=True)
        sbar = tk.Canvas(body, width=12, bg=BG, highlightthickness=0, bd=0)
        sbar.pack(side="right", fill="y", pady=4)
        sc = tk.Canvas(body, bg=BG, highlightthickness=0, bd=0, yscrollincrement=30)
        sc.pack(side="left", fill="both", expand=True)
        self._settings_canvas = sc
        col = tk.Frame(sc, bg=BG)
        win = sc.create_window(0, 26, window=col, anchor="n", state="hidden")

        def _layout(e=None):
            w = sc.winfo_width()
            sc.itemconfigure(win, width=max(300, min(620, w - 48)))
            sc.coords(win, w // 2, 26)
            sc.configure(scrollregion=(0, 0, w, col.winfo_reqheight() + 52))
        sc.bind("<Configure>", _layout)
        col.bind("<Configure>", _layout)

        # rounded scrollbar -- identical size/look/behaviour to the clip list's
        sb = {"lo": 0.0, "hi": 1.0, "drag": None}

        def _sb_geom():
            h    = sbar.winfo_height()
            span = max(0.0, min(1.0, sb["hi"] - sb["lo"]))
            L    = min(max(span * h - 4, 28), max(h - 4, 1))
            room = max(h - 4 - L, 0)
            rng  = 1.0 - span
            y0   = 2 + ((sb["lo"] / rng) * room if rng > 1e-6 else 0)
            return h, y0, L, room, rng

        def _sb_draw(e=None):
            sbar.delete("all")
            h, _y0, _L, _room, _rng = _sb_geom()
            if h < 2 or _rng <= 1e-6:
                return
            y0 = int(round(_y0))
            y1 = int(round(_y0 + _L))
            r = 4; x0, x1 = 2, 10
            _aa_oval(sbar, x0, y0, x1, y0 + r * 2, fill=_c("#3a3a5e"))
            _aa_oval(sbar, x0, y1 - r * 2, x1, y1, fill=_c("#3a3a5e"))
            sbar.create_rectangle(x0, y0 + r, x1, y1 - r, fill=_c("#3a3a5e"), outline="")

        def _sb_set(lo, hi):
            sb["lo"], sb["hi"] = float(lo), float(hi)
            _sb_draw()

        def _sb_press(e):
            # Remember where the drag started. (The canvas scrolls in 30px steps,
            # so adding up tiny per-event moves rounded back to the same spot and
            # the bar never moved.) 1px steps while dragging keep it smooth.
            sb["drag"] = e.y
            sb["start_lo"] = sb["lo"]
            sc.configure(yscrollincrement=1)

        def _sb_drag(e):
            if sb["drag"] is None:
                return
            h, _y0, _L, room, rng = _sb_geom()
            if room <= 0:
                return
            target = sb["start_lo"] + (e.y - sb["drag"]) / room * rng
            sc.yview_moveto(max(0.0, min(rng, target)))

        def _sb_release(e):
            sb["drag"] = None
            sc.configure(yscrollincrement=30)
        sbar._draw = _sb_draw
        sbar.bind("<Configure>", _sb_draw)
        sbar.bind("<ButtonPress-1>", _sb_press)
        sbar.bind("<B1-Motion>", _sb_drag)
        sbar.bind("<ButtonRelease-1>", _sb_release)
        sc.configure(yscrollcommand=_sb_set)

        def section(title):
            tk.Label(col, text=title, font=FONT_BOLD, bg=BG, fg=TEXT_MUTED,
                     anchor="w").pack(fill="x", pady=(0, 8))
            card = Pill(col, fill=CARD_BG, radius=10, pad=4, stretch=True)
            card.pack(fill="x", pady=(0, 22))
            return card.inner

        def row(card_inner, label, desc=""):
            r = tk.Frame(card_inner, bg=CARD_BG, padx=16, pady=14)
            r.pack(fill="x")
            left = tk.Frame(r, bg=CARD_BG)
            left.pack(side="left")
            tk.Label(left, text=label, font=FONT_BOLD, bg=CARD_BG,
                     fg=TEXT_PRIMARY, anchor="w").pack(anchor="w")
            desc_lbl = None
            if desc:
                desc_lbl = tk.Label(left, text=desc, font=FONT_TINY, bg=CARD_BG,
                                    fg=TEXT_MUTED, anchor="w")
                desc_lbl.pack(anchor="w", pady=(2, 0))
            right = tk.Frame(r, bg=CARD_BG)
            right.pack(side="right")
            right.desc_lbl = desc_lbl
            return right

        def row_divider(card_inner):
            tk.Frame(card_inner, bg=DIVIDER, height=1).pack(fill="x", padx=16)

        # The page is built one section per event-loop turn, so the app never
        # freezes for the whole build (each section is small, then Tk gets to
        # handle clicks / drawing / playback before the next one).

        def _hint_card(title, items):
            card = section(title)
            for _i, (_keys, _label, _desc) in enumerate(items):
                if _i:
                    row_divider(card)
                _right = row(card, _label, _desc)
                if _right.desc_lbl is not None:
                    _right.desc_lbl.configure(wraplength=360, justify="left")
                tk.Label(_right, text=_keys, font=FONT_TINY, bg=_c("#161628"),
                         fg=TEXT_PRIMARY, padx=9, pady=4).pack()

        steps = []

        def _step0():
            # ── Appearance ──
            appearance = section("APPEARANCE")
            right = row(appearance, "Theme", "Choose a color scheme for the app")
            field = Pill(right, fill=_c("#161628"), radius=8, pad=3)
            field.pack()
            self._theme_field = field
            self._theme_swatch = tk.Canvas(field.inner, width=47, height=CTRL_H - 4,
                                           bg=_c("#161628"), highlightthickness=0, bd=0)
            self._theme_swatch.pack(side="left", padx=(8, 0))
            self._theme_btn = GhostButton(field.inner, _CUR_THEME, self._toggle_theme_popup,
                                          font=FONT_UI, fg=TEXT_PRIMARY, arrow=True, width=150)
            self._theme_btn.pack(side="left")
            self._draw_theme_swatch()

            row_divider(appearance)
            right = row(appearance, "Accent color", "Using the theme's own accent")
            self._accent_desc = right.desc_lbl
            apill = Pill(right, fill=_c("#161628"), radius=8, pad=3)
            apill.pack()
            self._accent_swatch = tk.Canvas(apill.inner, width=24, height=CTRL_H - 4,
                                            bg=_c("#161628"), highlightthickness=0, bd=0)
            self._accent_swatch.pack(side="left", padx=(8, 0))
            self._accent_btn = GhostButton(apill.inner, "#FF2A6D", self._open_accent_dialog,
                                           font=FONT_UI, fg=TEXT_PRIMARY, padx=10, autosize=True)
            self._accent_btn.pack(side="left")
            Tooltip(self._accent_btn, "Pick a custom accent color")
            self._accent_div = apill.divider()
            self._accent_reset = GhostButton(apill.inner, "Reset", self._reset_accent,
                                             font=FONT_UI, fg=TEXT_PRIMARY, icon="refresh",
                                             icon_size=16, padx=10, autosize=True)
            self._accent_reset.pack(side="left")
            Tooltip(self._accent_reset, "Go back to the theme's own accent")
        steps.append(("Appearance", _step0))

        def _step1():
            # ── Starred clips ──
            starring = section("STARRED CLIPS")
            right = row(starring, "Copy starred clips to a folder",
                        "When you star a clip, a copy is saved to the folder below")
            self._star_toggle = ToggleSwitch(right, value=self._star_copy_enabled,
                                             command=self._on_star_copy_toggle)
            self._star_toggle.pack()
            row_divider(starring)
            right = row(starring, "Destination folder", "No folder chosen yet")
            self._star_path_lbl = right.desc_lbl
            pick = Pill(right, fill=_c("#161628"), radius=8, pad=3)
            pick.pack()
            self._star_pick_btn = GhostButton(pick.inner, "Choose folder", self._choose_star_folder_settings,
                                              font=FONT_UI, fg=TEXT_PRIMARY, icon="folder",
                                              icon_size=16, padx=12, autosize=True)
            self._star_pick_btn.pack()
        steps.append(("Starred clips", _step1))

        def _step2():
            # ── Cloud sync ──
            cloud = section("CLOUD SYNC")
            right = row(cloud, "Copy clips to a cloud folder",
                        "Copies clips to the selected folder")
            self._cloud_toggle = ToggleSwitch(right, value=self._cloud_enabled,
                                              command=self._on_cloud_toggle)
            self._cloud_toggle.pack()
            row_divider(cloud)
            right = row(cloud, "Cloud folder", "No folder chosen yet")
            self._cloud_path_lbl = right.desc_lbl
            cpick = Pill(right, fill=_c("#161628"), radius=8, pad=3)
            cpick.pack()
            self._cloud_pick_btn = GhostButton(cpick.inner, "Choose folder", self._choose_cloud_folder,
                                               font=FONT_UI, fg=TEXT_PRIMARY, icon="folder",
                                               icon_size=16, padx=12, autosize=True)
            self._cloud_pick_btn.pack()
            row_divider(cloud)
            right = row(cloud, "Games", "Only clips from the ticked games are copied")
            gfield = Pill(right, fill=_c("#161628"), radius=8, pad=3)
            gfield.pack()
            self._cloud_games_field = gfield
            self._cloud_games_btn = GhostButton(gfield.inner, self._cloud_games_text(),
                                                self._toggle_cloud_games_popup, font=FONT_UI,
                                                fg=TEXT_PRIMARY, arrow=True, width=170)
            self._cloud_games_btn.pack()
            row_divider(cloud)
            right = row(cloud, "Clips to copy",
                        "New clips: only clips recorded from now on. All clips: also the old ones")
            mpill = Pill(right, fill=_c("#161628"), radius=8, pad=3)
            mpill.pack()
            self._cloud_mode_btns = {}
            for _i, (_key, _label) in enumerate((("new", "New clips"), ("all", "All clips"))):
                if _i:
                    mpill.divider()
                _mb = GhostButton(mpill.inner, _label, lambda k=_key: self._set_cloud_mode(k),
                                  font=FONT_UI, fg=TEXT_MUTED, padx=12, autosize=True)
                _mb.pack(side="left")
                self._cloud_mode_btns[_key] = _mb
            self._cloud_refresh_mode_btns()
            row_divider(cloud)
            right = row(cloud, "Status", "x")
            _old_lbl = right.desc_lbl
            _left = _old_lbl.master
            _old_lbl.pack_forget()
            _hold = tk.Frame(_left, bg=CARD_BG)
            _hold.pack(anchor="w", pady=(2, 0))
            self._cloud_dot_cv = tk.Canvas(_hold, width=22, height=22, bg=CARD_BG,
                                           highlightthickness=0, bd=0)
            self._cloud_dot_cv.pack(side="left", padx=(0, 2))
            self._cloud_status_lbl = tk.Label(_hold, text="", font=FONT_TINY, bg=CARD_BG,
                                              fg=TEXT_MUTED, anchor="w")
            self._cloud_status_lbl.pack(side="left")
            self._cloud_paint_status()
            spill2 = Pill(right, fill=_c("#161628"), radius=8, pad=3)
            spill2.pack()
            GhostButton(spill2.inner, "Sync", self._cloud_sync_now, font=FONT_UI,
                        fg=TEXT_PRIMARY, padx=14, autosize=True).pack()
            row_divider(cloud)
            right = row(cloud, "Delete cloud clips",
                        "Deletes all the clips in the cloud folder. Your originals stay put")
            dpill = Pill(right, fill=_c("#161628"), radius=8, pad=3)
            dpill.pack()
            GhostButton(dpill.inner, "Delete all", self._cloud_delete_all, font=FONT_UI,
                        fg=TEXT_PRIMARY, icon="trash", icon_size=16, padx=12,
                        autosize=True).pack()
        steps.append(("Cloud sync", _step2))

        def _step_phone():
            # ── Phone sync ──
            ph = section("PHONE SYNC")
            right = row(ph, "Sync clips to phone",
                        "Send clips to the ClipSync app on your phone")
            self._phone_toggle = ToggleSwitch(right, value=self._phone_enabled,
                                              command=self._on_phone_toggle)
            self._phone_toggle.pack()
            # Everything below the switch lives in one frame that is hidden while it is off
            self._phone_body = body = tk.Frame(ph, bg=CARD_BG)
            row_divider(body)
            right = row(body, "Pairing code", "Shown at the top of the ClipSync app on your phone")
            self._phone_code_var = tk.StringVar(value=_read_settings().get("phone_token", ""))
            tk.Entry(right, textvariable=self._phone_code_var, font=FONT_UI, width=10,
                     bg=_c("#242444"), fg=TEXT_PRIMARY, insertbackground=TEXT_PRIMARY,
                     relief="flat").pack(ipady=6)
            row_divider(body)
            right = row(body, "Phone address (optional)",
                        "Leave empty: your phone is found automatically on the same network")
            self._phone_ip_var = tk.StringVar(value=_read_settings().get("phone_manual_ip", ""))
            tk.Entry(right, textvariable=self._phone_ip_var, font=FONT_UI, width=16,
                     bg=_c("#242444"), fg=TEXT_PRIMARY, insertbackground=TEXT_PRIMARY,
                     relief="flat").pack(ipady=6)
            row_divider(body)
            right = row(body, "Clips to send",
                        "Pick one or more. All clips sends every clip in your folders")
            kpill = Pill(right, fill=_c("#161628"), radius=8, pad=3)
            kpill.pack()
            self._phone_kind_btns = {}
            for _i, (_key, _label) in enumerate((("starred", "Starred"), ("liked", "Liked"),
                                                 ("trimmed", "Trimmed"), ("all", "All clips"))):
                if _i:
                    kpill.divider()
                _kb = GhostButton(kpill.inner, _label, lambda k=_key: self._phone_toggle_kind(k),
                                  font=FONT_UI, fg=TEXT_MUTED, padx=12, autosize=True)
                _kb.pack(side="left")
                self._phone_kind_btns[_key] = _kb
            self._phone_refresh_kind_btns()
            row_divider(body)
            right = row(body, "Status", "x")
            _old_lbl = right.desc_lbl
            _left = _old_lbl.master
            _old_lbl.pack_forget()
            _hold = tk.Frame(_left, bg=CARD_BG)
            _hold.pack(anchor="w", pady=(2, 0))
            self._phone_dot_cv = tk.Canvas(_hold, width=22, height=22, bg=CARD_BG,
                                           highlightthickness=0, bd=0)
            self._phone_dot_cv.pack(side="left", padx=(0, 2))
            self._phone_status_lbl = tk.Label(_hold, text="", font=FONT_TINY, bg=CARD_BG,
                                              fg=TEXT_MUTED, anchor="w", wraplength=330,
                                              justify="left")
            self._phone_status_lbl.pack(side="left")
            self._phone_paint_status()
            spill = Pill(right, fill=_c("#161628"), radius=8, pad=3)
            spill.pack()
            GhostButton(spill.inner, "Sync", self._phone_sync_now, font=FONT_UI,
                        fg=TEXT_PRIMARY, padx=14, autosize=True).pack()
            if self._phone_enabled:
                body.pack(fill="x")
        steps.append(("Phone sync", _step_phone))

        def _step3():
            # ── Playback ──
            playback = section("PLAYBACK")
            right = row(playback, "Loop clips",
                        "Replay the clip when it ends (a trimmed clip loops its trimmed part)")
            self._loop_toggle = ToggleSwitch(right, value=self._loop_clip,
                                             command=self._on_loop_toggle)
            self._loop_toggle.pack()
            row_divider(playback)
            right = row(playback, "Always start clips from the beginning",
                        "Long clips normally open in the middle; this makes every clip "
                        "start at 0:00 (trimmed clips still start at their trim)")
            right.desc_lbl.configure(wraplength=300, justify="left")
            self._start_toggle = ToggleSwitch(right, value=self._start_always,
                                              command=self._on_start_always_toggle)
            self._start_toggle.pack()
        steps.append(("Playback", _step3))

        def _step4():
            # ── Audio ──
            audio = section("AUDIO")
            right = row(audio, "Audio track controls",
                        "Show the audio tracks button under the player and the default "
                        "audio track settings below. Turn off to hide both; every clip "
                        "then plays with the default audio tracks")
            right.desc_lbl.configure(wraplength=300, justify="left")
            self._at_toggle = ToggleSwitch(right, value=self._at_enabled,
                                           command=self._on_audio_btn_toggle)
            self._at_toggle.pack()
            row_divider(audio)
            right = row(audio, "SteelSeries GG Moments: first track only",
                        "SteelSeries clips start with only the first audio track (no "
                        "Game + Mic mix), which avoids the black screen. Turn the Mic on "
                        "for a single clip with the audio tracks button under the player")
            right.desc_lbl.configure(wraplength=300, justify="left")
            self._steel_toggle = ToggleSwitch(right, value=self._steel_first_only,
                                              command=self._on_steel_first_toggle)
            self._steel_toggle.pack()
            # Everything below is hidden together with the audio tracks button
            self._at_def_group = tk.Frame(audio, bg=CARD_BG)
            row_divider(self._at_def_group)
            right = row(self._at_def_group, "Default audio tracks",
                        "Tracks played and the volume of each, by default, when a clip "
                        "has several tracks")
            right.desc_lbl.configure(wraplength=420, justify="left")
            self._build_at_default_rows(self._at_def_group)
            if self._at_enabled:
                self._at_def_group.pack(fill="x")
        steps.append(("Audio", _step4))

        def _step5():
            # ── Trimming ──
            trimming = section("TRIMMING")
            right = row(trimming, "Use the GPU encoder when applying a trim",
                        "Much faster on NVIDIA cards, but files can come out a bit bigger. "
                        "Falls back to the CPU if no GPU works")
            self._trim_gpu_toggle = ToggleSwitch(right, value=self._trim_gpu,
                                                 command=self._on_trim_gpu_toggle)
            self._trim_gpu_toggle.pack()
        steps.append(("Trimming", _step5))

        def _step6():
            # ── Clips ──
            clips = section("CLIPS")
            right = row(clips, "Move deleted clips to the Recycle Bin",
                        "Turn off to delete clips permanently instead")
            self._bin_toggle = ToggleSwitch(right, value=self._use_recycle_bin,
                                            command=self._on_recycle_toggle)
            self._bin_toggle.pack()
            row_divider(clips)
            right = row(clips, "Auto-refresh the folder",
                        "New clips show up on their own once a recording finishes")
            self._autorefresh_toggle = ToggleSwitch(right, value=self._auto_refresh,
                                                    command=self._on_auto_refresh_toggle)
            self._autorefresh_toggle.pack()
            row_divider(clips)
            right = row(clips, "Clips added by \"Load more\"",
                        "How many more clips load each time you press Load more")
            spill = Pill(right, fill=_c("#161628"), radius=8, pad=3)
            spill.pack()
            GhostButton(spill.inner, "−", lambda: self._change_see_more(-1),
                        font=FONT_UI, fg=TEXT_PRIMARY, padx=12, autosize=True).pack(side="left")
            self._see_more_lbl = tk.Label(spill.inner, text=str(self._see_more_step),
                                          font=FONT_BOLD, bg=_c("#161628"),
                                          fg=TEXT_PRIMARY, width=5)
            self._see_more_lbl.pack(side="left")
            GhostButton(spill.inner, "+", lambda: self._change_see_more(1),
                        font=FONT_UI, fg=TEXT_PRIMARY, padx=12, autosize=True).pack(side="left")

            row_divider(clips)
            right = row(clips, "Thumbnail cache", "Calculating…")
            self._cache_info_lbl = right.desc_lbl
            cpill = Pill(right, fill=_c("#161628"), radius=8, pad=3)
            cpill.pack()
            self._cache_pill = cpill
            self._cache_btn = GhostButton(cpill.inner, "Clear cache", self._clear_thumbnail_cache,
                        font=FONT_UI, fg=TEXT_PRIMARY, padx=12, autosize=True,
                        hover_fg=_c("#ff8a94"),
                        hover_bg=_mix_hex(CARD_BG, "#ff4455", 0.22),
                        press_bg=_mix_hex(CARD_BG, "#ff4455", 0.10))
            self._cache_btn.pack()
            _bind_danger_pill(cpill, self._cache_btn)
        steps.append(("Clips", _step6))

        def _step7():
            # ── Window ──
            window = section("WINDOW")
            right = row(window, "Remember window size",
                        "Open the app at the size you last used")
            self._size_toggle = ToggleSwitch(right, value=self._remember_size,
                                             command=self._on_remember_size_toggle)
            self._size_toggle.pack()
            row_divider(window)
            right = row(window, "Close button hides to the tray",
                        "Turn off to quit the app when you close the window")
            self._tray_toggle = ToggleSwitch(right, value=self._close_to_tray,
                                             command=self._on_tray_toggle)
            self._tray_toggle.pack()
            row_divider(window)
            right = row(window, "Start with Windows",
                        "Launch Feather Player in the tray when you sign in")
            self._startup_toggle = ToggleSwitch(right, value=_startup_enabled(),
                                                command=self._on_startup_toggle)
            self._startup_toggle.pack()
        steps.append(("Window", _step7))

        def _step8():
            # ── Keyboard shortcuts ──
            _hint_card("KEYBOARD SHORTCUTS", [
                ("Space",    "Play / pause",           "Pause or resume the open clip"),
                ("\u2190",   "Newer clip",             "Jump to the next newer clip in the current filter"),
                ("\u2192",   "Older clip",             "Jump to the next older clip in the current filter"),
                ("M",        "Mute / unmute",          "Mute or unmute the volume"),
                ("Esc",      "Close settings or a popup",
                             "Also closes the theme and cloud game lists"),
                ("Ctrl + Q", "Quit completely",
                             "Quits even when the close button hides to the tray"),
            ])
        steps.append(("Keyboard shortcuts", _step8))

        def _step9():
            # ── Hidden features ──
            _hint_card("HIDDEN FEATURES", [
                ("Right-click clip",  "Select clips for deleting",
                                      "Hold and drag over clips to select several"),
                ("Right-click game",  "Pick several games",
                                      "In the game list: adds or removes it and keeps the list open"),
                ("Middle-click game", "Rename a game",
                                      "In the game list: renames that category"),
            ])
        steps.append(("Hidden features", _step9))

        def _finish():
            self._settings_built = True
            _debug_log("settings page built: %.0f ms total (spread over %d steps)"
                       % ((time.monotonic() - _bt0) * 1000, len(steps)))
            sc.itemconfigure(win, state="normal")
            if getattr(self, "_settings_pending", False):
                # you are looking at the blank screen: lay the finished page out
                # off-screen, then swap it in
                self._settings_pending = False
                self._refresh_star_settings()
                self._refresh_accent_settings()
                self._refresh_cloud_settings()
                self._refresh_cache_info()
                page.place(x=-10000, y=0, relwidth=1, relheight=1)
                self.update_idletasks()
                page.place_forget()
                _layout()
                self._settings_layout_size = (self.winfo_width(), self.winfo_height())
                try:
                    self._settings_blank.destroy()
                except Exception:
                    pass
                page.pack(fill="both", expand=True)
            else:
                _layout()

        def _run(i=0):
            if self._settings_page is not page:
                return                      # page was thrown away meanwhile
            if i >= len(steps):
                _finish()
                return
            _t = time.monotonic()
            try:
                steps[i][1]()
            except Exception as e:
                _debug_log(f"settings step {steps[i][0]} error: {e}")
            _debug_log("settings step %s: %.0f ms" % (steps[i][0], (time.monotonic() - _t) * 1000))
            self.after(1, lambda: _run(i + 1))

        _bt0 = time.monotonic()
        self._settings_built = False
        self.after(1, _run)     # shell shows first, sections follow

    # ── Cloud sync: copy new clips into a cloud folder ───────────────────────
    # Originals are never touched. Only clips recorded after the switch was
    # turned on are copied (the Sync button, added later, handles older ones).
    _CLOUD_COPIED_PATH = os.path.join(_DATA_DIR, "cloud_copied.json")

    def _cloud_load_copied(self):
        try:
            with open(self._CLOUD_COPIED_PATH, "r", encoding="utf-8") as f:
                d = json.load(f)
            return d if isinstance(d, dict) else {}
        except Exception:
            return {}

    def _cloud_save_copied(self):
        try:
            os.makedirs(os.path.dirname(self._CLOUD_COPIED_PATH), exist_ok=True)
            tmp = self._CLOUD_COPIED_PATH + ".tmp"
            with open(tmp, "w", encoding="utf-8") as f:
                json.dump(self._cloud_copied, f)
            os.replace(tmp, self._CLOUD_COPIED_PATH)
        except Exception:
            pass

    # Status dot: green = up to date, red = out of date / problem, amber = working, grey = off
    _CLOUD_DOT_COLORS = {"ok": "#35d07f", "bad": "#ff4d5a", "busy": "#f5b942", "off": "#6b6b80"}

    def _cloud_kind(self, text):
        t = (text or "").lower()
        if t == "off":
            return "off"
        if t.startswith(("up to date", "all ")):
            return "ok"
        if t.startswith(("transferring", "waiting", "syncing", "checking", "already")) \
                or "still recording" in t:
            return "busy"
        return "bad"

    def _cloud_dot_img(self, kind):
        """Glowing dot as an RGBA image (alpha, so it sits on any theme's card colour)."""
        cache = self.__dict__.setdefault("_cloud_dot_cache", {})
        if kind in cache:
            return cache[kind]
        from PIL import ImageFilter as _F
        S, N = 4, 22                                     # 4x supersampling, 22 px final
        col = _hex_to_rgb(self._CLOUD_DOT_COLORS[kind])
        rgb = tuple(int(v * 255) for v in col)
        c = N * S / 2.0
        img = _PILImage.new("RGBA", (N * S, N * S), rgb + (0,))
        if kind != "off":                                # the glow
            glow = _PILImage.new("L", (N * S, N * S), 0)
            r = 5.2 * S
            _PILDraw.Draw(glow).ellipse([c - r, c - r, c + r, c + r], fill=255)
            glow = glow.filter(_F.GaussianBlur(3.2 * S)).point(lambda v: min(255, int(v * 1.5)))
            img.putalpha(glow)
        dot = _PILImage.new("L", (N * S, N * S), 0)      # the solid dot
        r = 3.6 * S
        _PILDraw.Draw(dot).ellipse([c - r, c - r, c + r, c + r], fill=255)
        solid = _PILImage.new("RGBA", (N * S, N * S), rgb + (255,))
        solid.putalpha(dot)
        img.alpha_composite(solid)
        light = _PILImage.new("L", (N * S, N * S), 0)    # tiny highlight
        r = 1.3 * S
        _PILDraw.Draw(light).ellipse([c - 1.1 * S - r, c - 1.3 * S - r, c - 1.1 * S + r, c - 1.3 * S + r], fill=90)
        hi = _PILImage.new("RGBA", (N * S, N * S), (255, 255, 255, 0))
        hi.putalpha(light)
        img.alpha_composite(hi)
        img = img.resize((N, N), _PILImage.LANCZOS)
        cache[kind] = _PILTk.PhotoImage(img)
        return cache[kind]

    def _cloud_paint_status(self):
        """Show the current status text and its glowing dot in Settings."""
        try:
            txt = self._cloud_status
            self._cloud_status_lbl.configure(text=txt)
            cv = self._cloud_dot_cv
            cv.delete("all")
            cv.create_image(11, 11, image=self._cloud_dot_img(self._cloud_kind(txt)))
        except Exception:
            pass

    def _cloud_set_status(self, text, force=False):
        """Thread-safe: remember the status and show it in Settings (if built).
        While the switch is off only the Sync button (force=True) may show text."""
        if not self._cloud_enabled and not force:
            text = "Off"
        self._cloud_status = text

        self._post_main(self._cloud_paint_status)

    def _cloud_refresh_mode_btns(self):
        """Highlight the active 'Clips to copy' choice."""
        try:
            for key, b in self._cloud_mode_btns.items():
                b._fg = ACCENT if key == self._cloud_mode else TEXT_MUTED
                b._draw()
        except Exception:
            pass

    def _set_cloud_mode(self, mode):
        if mode == self._cloud_mode:
            return
        self._cloud_mode = mode
        self._save_setting("cloud_mode", mode)
        if mode == "new":
            # only clips recorded from now on
            self._cloud_since = time.time()
            self._cloud_prev = {}
            self._save_setting("cloud_since", self._cloud_since)
        self._cloud_refresh_mode_btns()
        if self._cloud_enabled:
            self._cloud_set_status("Up to date")

    def _on_cloud_toggle(self, value):
        value = bool(value)
        if value and not (self._cloud_folder and os.path.isdir(self._cloud_folder)):
            if not self._choose_cloud_folder():          # needs a folder first
                try:
                    self._cloud_toggle.set(False)
                except Exception:
                    pass
                return
        self._cloud_enabled = value
        if value:
            self._cloud_since = time.time()              # only clips from now on
            self._cloud_prev = {}
            self._save_setting("cloud_since", self._cloud_since)
            self._cloud_set_status("Up to date")
        else:
            self._cloud_set_status("Off")
        self._save_setting("cloud_enabled", value)

    def _choose_cloud_folder(self):
        kw = {}
        if os.path.isdir(self._cloud_folder or ""):
            kw["initialdir"] = self._cloud_folder
        path = filedialog.askdirectory(title="Choose your cloud folder (e.g. Google Drive)",
                                       parent=self, **kw)
        if not path:
            return False
        path = os.path.normpath(path)
        if path != self._cloud_folder:
            self._cloud_folder = path
            self._cloud_copied.clear()                   # new destination: start fresh
            self._cloud_save_copied()
            self._cloud_since = time.time()
            self._cloud_prev = {}
            self._save_setting("cloud_folder", path)
            self._save_setting("cloud_since", self._cloud_since)
        self._refresh_cloud_settings()
        return True

    def _refresh_cloud_settings(self):
        try:
            self._cloud_toggle.set(self._cloud_enabled, animate=False)
            folder = self._cloud_folder
            self._cloud_path_lbl.configure(
                text=_ellipsize(folder, FONT_TINY, 330, keep="start") if folder
                else "No folder chosen yet")
            self._cloud_pick_btn.configure(text="Change folder" if folder else "Choose folder")
        except Exception:
            pass
        try:
            self._cloud_games_btn.configure(text=self._cloud_games_text())
        except Exception:
            pass
        self._cloud_refresh_mode_btns()
        self._cloud_paint_status()
        self._refresh_phone_settings()

    # ── Cloud sync: only certain games ───────────────────────────────────────
    _CLOUD_NONE_KEY = "__none__"          # clips without a game name ("Moments")

    def _cloud_game_key(self, path):
        try:
            return self._category_key_for_video(path) or self._CLOUD_NONE_KEY
        except Exception:
            return self._CLOUD_NONE_KEY

    def _cloud_game_ok(self, path, games=None):
        """True if this clip's game is allowed (an empty selection = all games)."""
        games = self._cloud_games if games is None else games
        return (not games) or self._cloud_game_key(path) in games

    def _cloud_game_label(self, key):
        if key == self._CLOUD_NONE_KEY:
            return "Moments"
        return self._game_norm_map.get(key, key)

    def _cloud_games_text(self):
        g = self._cloud_games
        if not g:
            return "All games"
        names = sorted((self._cloud_game_label(k) for k in g), key=str.lower)
        if len(names) == 1:
            return names[0]
        joined = ", ".join(names)
        return joined if len(names) == 2 and len(joined) <= 20 else "%d games" % len(names)

    def _cloud_games_pick(self, key):
        """Tick / untick one game (the dropdown stays open). 'All' clears the selection."""
        g = set(self._cloud_games)
        if key == "All":
            g = set()
        else:
            g ^= {key}
        self._cloud_games = g
        self._save_setting("cloud_games", sorted(g))
        try:
            self._cloud_games_btn.configure(text=self._cloud_games_text())
            for k, r in self._cloud_game_rows.items():
                r.set_checked((not g) if k == "All" else (k in g))
        except Exception:
            pass

    def _toggle_cloud_games_popup(self):
        if self._cloud_games_popup is not None:
            self._close_cloud_games_popup()
            return
        self._close_theme_popup()
        page = self._settings_page
        pop = RoundedPopup(page)
        self._cloud_games_popup = pop
        cv = tk.Canvas(pop.inner, bg=CARD_BG, highlightthickness=0, bd=0)
        cv.pack(fill="both", expand=True)
        inner = tk.Frame(cv, bg=CARD_BG)
        win = cv.create_window(0, 0, window=inner, anchor="nw")

        def wheel(e):
            cv.yview_scroll(int(-1 * (e.delta / 120)), "units")
            return "break"

        counts = {}
        for v in list(self.videos):
            k = self._cloud_game_key(v)
            counts[k] = counts.get(k, 0) + 1
        keys = set(self._game_norm_map) | set(self._cloud_games)
        if counts.get(self._CLOUD_NONE_KEY):
            keys.add(self._CLOUD_NONE_KEY)                 # Moments is listed like any other game
        keys = sorted(keys, key=lambda k: self._cloud_game_label(k).lower())
        g = self._cloud_games
        self._cloud_game_rows = {}

        def add(key, text, checked, count):
            r = PopupRow(inner, text, lambda k=key: self._cloud_games_pick(k),
                         font=FONT_UI, height=30, check=checked,
                         right_text=str(count) if count else "", on_wheel=wheel)
            r.pack(fill="x", pady=1)
            self._cloud_game_rows[key] = r

        add("All", "All games", not g, len(self.videos))
        for k in keys:
            add(k, self._cloud_game_label(k), k in g, counts.get(k, 0))

        pop.update_idletasks()
        field = self._cloud_games_field
        pw = max(inner.winfo_reqwidth() + 12, field.winfo_width())
        ch = min(inner.winfo_reqheight(), 300)
        cv.configure(width=pw - 10, height=ch,
                     scrollregion=(0, 0, pw - 10, inner.winfo_reqheight()))
        cv.itemconfigure(win, width=pw - 10)
        ph = ch + 2 * 5                                  # canvas + the popup's own padding
        x = field.winfo_rootx() - page.winfo_rootx() + field.winfo_width() - pw
        y = field.winfo_rooty() - page.winfo_rooty() + field.winfo_height() + 4
        if y + ph > page.winfo_height():                 # no room below: open upwards
            y = max(4, field.winfo_rooty() - page.winfo_rooty() - ph - 4)
        self._place_popup_settled(pop, x, y, pw)
        pop.lift()

    def _close_cloud_games_popup(self):
        if self._cloud_games_popup is not None:
            try:
                self._cloud_games_popup.destroy()
            except Exception:
                pass
        self._cloud_games_popup = None

    def _cloud_games_outside(self, event):
        if self._cloud_games_popup is None:
            return
        w = event.widget
        while w is not None:
            if w is self._cloud_games_popup or w is self._cloud_games_btn:
                return
            try:
                w = w.master
            except Exception:
                break
        self._close_cloud_games_popup()

    def _cloud_copy_of(self, path):
        """Path of this clip's copy in the cloud folder (checked: still there,
        same size, directly inside the current cloud folder), or None."""
        try:
            rec = self._cloud_copied.get(path)
            if not rec or len(rec) < 3 or not self._cloud_folder:
                return None
            t = rec[2]
            base = os.path.normcase(os.path.normpath(self._cloud_folder))
            if os.path.normcase(os.path.normpath(os.path.dirname(t))) != base:
                return None
            if os.path.isfile(t) and os.path.getsize(t) == rec[0]:
                return t
        except Exception:
            pass
        return None

    def _cloud_sync_now(self):
        """Sync button: check access, then copy every clip that isn't in the
        cloud folder yet, and report the result. Does nothing while cloud sync is
        off. "New clips" mode only copies clips recorded since it was turned on;
        "All clips" mode also copies the old ones."""
        if not self._cloud_enabled:
            self._cloud_set_status("Cloud sync is off. Turn it on first", force=True)
            return
        if not self._cloud_folder:
            if not self._choose_cloud_folder():
                return
        if self._cloud_busy:
            self._cloud_set_status("Already transferring, try again in a moment", force=True)
            return
        self._cloud_busy = True
        self._cloud_set_status("Checking folder access…", force=True)
        paths = [p for p in self._checked_folders if os.path.isdir(p)]
        threading.Thread(target=self._cloud_sync_worker, daemon=True,
                         args=(paths, self._cloud_folder)).start()

    def _cloud_sync_worker(self, paths, dest):
        st = self._cloud_set_status
        try:
            if not os.path.isdir(dest):
                st("No access: folder not available", force=True)
                return
            test = os.path.join(dest, ".featherplayer_write_test")
            try:
                with open(test, "wb") as f:
                    f.write(b"ok")
                os.remove(test)
            except Exception as e:
                st("No access: can't write to the folder (%s)" % e, force=True)
                return
            sig = self._cloud_scan(paths, dest)
            games = set(self._cloud_games)
            if games:
                sig = {p: v for p, v in sig.items() if self._cloud_game_ok(p, games)}
            if self._cloud_mode == "new":
                since = self._cloud_since
                sig = {p: v for p, v in sig.items() if v[1] >= since - 2}
            now = time.time()
            todo, busy_files = [], 0
            for p, (size, mt) in sig.items():
                rec = self._cloud_copied.get(p)
                if rec and rec[0] == size and len(rec) > 3 and rec[3]:
                    continue                             # removed from the cloud on purpose
                if (rec and rec[0] == size and len(rec) > 2
                        and os.path.isfile(rec[2]) and os.path.getsize(rec[2]) == size):
                    continue                             # verified: it is in the folder
                if size == 0 or now - mt < 5:
                    busy_files += 1                      # still being recorded
                    continue
                todo.append((p, size, mt))
            total = len(sig)
            if not todo:
                if total == 0:
                    st("Up to date", force=True)
                    return
                if busy_files:
                    st("%d of %d clips transferred, %d still recording" %
                       (total - busy_files, total, busy_files), force=True)
                else:
                    st("All %d clips transferred" % total, force=True)
                return
            failed = 0
            for i, (p, size, mt) in enumerate(todo, 1):
                st("Syncing clip %d of %d…" % (i, len(todo)), force=True)
                try:
                    target = self._cloud_copy_one(p, dest, size)
                    self._cloud_copied[p] = [size, mt, target]
                    self._cloud_save_copied()
                except Exception:
                    failed += 1
            if failed:
                st("Synced %d, %d failed. Press Sync to retry" % (len(todo) - failed, failed),
                   force=True)
            elif busy_files:
                st("%d of %d clips transferred, %d still recording" %
                   (total - busy_files, total, busy_files), force=True)
            else:
                st("All %d clips transferred" % total, force=True)
        except Exception as e:
            st("Sync error: %s" % e, force=True)
        finally:
            self._cloud_busy = False

    def _cloud_delete_all(self):
        """Delete every clip in the cloud folder (only video files, directly
        inside it). Local originals are never touched, and the records are kept
        (marked as removed on purpose) so sync doesn't upload them again."""
        folder = self._cloud_folder
        if not folder or not os.path.isdir(folder):
            self._cloud_set_status("Choose a cloud folder first", force=True)
            return
        if self._cloud_busy:
            self._cloud_set_status("Already transferring, try again in a moment", force=True)
            return
        exts = self._WATCH_EXTS
        try:
            n = sum(1 for e in os.scandir(folder)
                    if e.is_file() and os.path.splitext(e.name)[1].lower() in exts)
        except Exception as e:
            self._cloud_set_status("No access: %s" % e, force=True)
            return
        if n == 0:
            self._cloud_set_status("No clips in the cloud folder", force=True)
            return
        if not self._dark_dialog(
                "Delete cloud clips",
                "Delete all %d clip%s in the cloud folder?\n\nYour original clips are "
                "not touched. This can't be undone." % (n, "" if n == 1 else "s"),
                yes_text="Delete"):
            return
        self._cloud_busy = True
        self._cloud_set_status("Deleting cloud clips…", force=True)
        threading.Thread(target=self._cloud_delete_worker, daemon=True,
                         args=(folder,)).start()

    def _cloud_delete_worker(self, folder):
        st = self._cloud_set_status
        done, failed = 0, 0
        try:
            with os.scandir(folder) as it:
                files = [e.path for e in it if e.is_file()
                         and os.path.splitext(e.name)[1].lower() in self._WATCH_EXTS]
            for f in files:
                try:
                    os.remove(f)
                    done += 1
                except Exception:
                    failed += 1
            for path, rec in list(self._cloud_copied.items()):
                try:
                    if len(rec) >= 3 and not os.path.exists(rec[2]):
                        self._cloud_copied[path] = [rec[0], rec[1], rec[2], True]
                except Exception:
                    pass
            self._cloud_save_copied()
            if failed:
                st("Deleted %d, %d failed" % (done, failed), force=True)
            else:
                st("Deleted %d clip%s from the cloud folder" % (done, "" if done == 1 else "s"),
                   force=True)
        except Exception as e:
            st("Delete error: %s" % e, force=True)
        finally:
            self._cloud_busy = False

    def _cloud_tick(self):
        """Every few seconds: copy finished new clips. Runs even while the
        window is hidden in the tray."""
        try:
            if self._cloud_enabled and not self._cloud_busy:
                if not self._cloud_folder:
                    self._cloud_set_status("Choose a folder first")
                else:
                    self._cloud_busy = True
                    paths = [p for p in self._checked_folders if os.path.isdir(p)]
                    threading.Thread(target=self._cloud_worker, daemon=True,
                                     args=(paths, self._cloud_folder,
                                           0.0 if self._cloud_mode == "all" else self._cloud_since)).start()
        except Exception:
            self._cloud_busy = False
        self.after(5000, self._cloud_tick)

    def _cloud_scan(self, paths, dest):
        """{clip path: (size, mtime)} for the watched folders (one level deep)."""
        sig = {}
        exts = self._WATCH_EXTS
        dest_n = os.path.normcase(os.path.normpath(dest))

        def scan(base, depth):
            try:
                with os.scandir(base) as it:
                    for entry in it:
                        try:
                            if entry.is_file(follow_symlinks=False):
                                if os.path.splitext(entry.name)[1].lower() in exts:
                                    st = entry.stat(follow_symlinks=False)
                                    sig[entry.path] = (st.st_size, st.st_mtime)
                            elif depth == 0 and entry.is_dir(follow_symlinks=False):
                                if os.path.normcase(os.path.normpath(entry.path)) != dest_n:
                                    scan(entry.path, 1)
                        except Exception:
                            pass
            except Exception:
                pass
        for base in paths:
            if os.path.normcase(os.path.normpath(base)) != dest_n:
                scan(base, 0)
        return sig

    def _cloud_copy_one(self, src, dest_dir, size):
        """Copy src into dest_dir. Same name + same size = already there (skip);
        same name + different size = save as 'name (1).ext'. Returns the target path."""
        name = os.path.basename(src)
        stem, ext = os.path.splitext(name)
        target = os.path.join(dest_dir, name)
        k = 0
        while os.path.exists(target):
            try:
                if os.path.getsize(target) == size:
                    return target
            except OSError:
                pass
            k += 1
            target = os.path.join(dest_dir, "%s (%d)%s" % (stem, k, ext))
        part = target + ".part"                          # cloud apps ignore it until it's complete
        try:
            shutil.copyfile(src, part)
            os.replace(part, target)
        finally:
            if os.path.exists(part):
                try:
                    os.remove(part)
                except Exception:
                    pass
        return target

    def _cloud_worker(self, paths, dest, since):
        try:
            if not os.path.isdir(dest):
                self._cloud_set_status("Folder not available, will retry")
                return
            sig = self._cloud_scan(paths, dest)
            games = set(self._cloud_games)
            if games:
                sig = {p: v for p, v in sig.items() if self._cloud_game_ok(p, games)}
            prev, self._cloud_prev = self._cloud_prev, {p: v[0] for p, v in sig.items()}
            now = time.time()
            todo, waiting = [], 0
            for p, (size, mt) in sig.items():
                if mt < since - 2:
                    continue                             # older than the switch: ignored
                rec = self._cloud_copied.get(p)
                if rec and rec[0] == size:
                    continue                             # already copied
                if size == 0 or prev.get(p) != size or now - mt < 5:
                    waiting += 1                         # still being recorded
                    continue
                todo.append((p, size, mt))
            if not todo:
                self._cloud_set_status("Waiting for the recording to finish…" if waiting
                                       else "Up to date")
                return
            failed = None
            for i, (p, size, mt) in enumerate(todo, 1):
                if not self._cloud_enabled:
                    return
                self._cloud_set_status("Transferring clip %d of %d…" % (i, len(todo)))
                try:
                    target = self._cloud_copy_one(p, dest, size)
                    self._cloud_copied[p] = [size, mt, target]
                    self._cloud_save_copied()
                except Exception as e:
                    failed = (os.path.basename(p), str(e))
            if failed:
                self._cloud_set_status("Failed: %s (%s). Will retry" % failed)
            else:
                self._cloud_set_status("Up to date")
        except Exception as e:
            self._cloud_set_status("Error: %s" % e)
        finally:
            self._cloud_busy = False

    # -- Phone sync: send starred clips + clips.json to the ClipSync phone app --

    def _phone_synced_path(self):
        return os.path.join(_DATA_DIR, "phone_synced.json")

    def _phone_load_synced(self):
        try:
            with open(self._phone_synced_path(), "r", encoding="utf-8") as f:
                d = json.load(f)
            return d if isinstance(d, dict) else {}
        except Exception:
            return {}

    def _phone_save_synced(self, d):
        try:
            tmp = self._phone_synced_path() + ".tmp"
            with open(tmp, "w", encoding="utf-8") as f:
                json.dump(d, f)
            os.replace(tmp, self._phone_synced_path())
        except Exception:
            pass

    # Status dot (same glowing dot as cloud sync): green = ok, red = problem, amber = working, grey = off
    def _phone_kind_of(self, text):
        t = (text or "").lower()
        if t == "off":
            return "off"
        if t.startswith(("ready", "done")) and "failed" not in t:
            return "ok"
        if t.startswith(("looking", "cutting", "sending", "already")):
            return "busy"
        return "bad"

    def _phone_paint_status(self):
        try:
            txt = self._phone_status
            self._phone_status_lbl.configure(text=txt)
            cv = self._phone_dot_cv
            cv.delete("all")
            cv.create_image(11, 11, image=self._cloud_dot_img(self._phone_kind_of(txt)))
        except Exception:
            pass

    def _phone_set_status(self, text):
        self._phone_status = text
        self._post_main(self._phone_paint_status)

    def _phone_refresh_kind_btns(self):
        """Highlight the chosen 'Clips to send' options."""
        try:
            for key, b in self._phone_kind_btns.items():
                b._fg = ACCENT if key in self._phone_kinds else TEXT_MUTED
                b._draw()
        except Exception:
            pass

    def _phone_toggle_kind(self, key):
        kinds = set(self._phone_kinds)
        if key == "all":
            kinds = {"all"}
        else:
            kinds.discard("all")
            if key in kinds:
                kinds.discard(key)
            else:
                kinds.add(key)
            if not kinds:                       # always keep at least one choice
                kinds = {key}
        self._phone_kinds = kinds
        self._save_setting("phone_kinds", sorted(kinds))
        self._phone_refresh_kind_btns()

    def _on_phone_toggle(self, value):
        self._phone_enabled = bool(value)
        self._save_setting("phone_enabled", self._phone_enabled)
        self._phone_apply_visibility()

    def _phone_apply_visibility(self):
        """Show or hide every phone control below the switch."""
        try:
            if self._phone_enabled:
                if not self._phone_body.winfo_ismapped():
                    self._phone_body.pack(fill="x")
            else:
                self._phone_body.pack_forget()
        except Exception:
            pass

    def _refresh_phone_settings(self):
        try:
            self._phone_toggle.set(self._phone_enabled, animate=False)
            self._phone_apply_visibility()
            self._phone_refresh_kind_btns()
            self._phone_paint_status()
        except Exception:
            pass

    def _phone_snapshot(self):
        """Cheap copy of everything the worker needs, taken on the main thread
        so the worker never reads live UI state."""
        return {
            "kinds":        set(self._phone_kinds),
            "starred":      set(self._starred_clips),
            "liked":        set(self._liked_clips),
            "trim_enabled": set(self._trim_enabled),
            "trims":        dict(self._trims),
            "folders":      list(self._checked_folders),
        }

    def _phone_collect_paths(self, snap):
        """Clips to send, following the 'Clips to send' choices (worker thread)."""
        kinds = snap["kinds"]
        found = set()
        if "all" in kinds:
            folders = [p for p in snap["folders"] if os.path.isdir(p)]
            try:
                found.update(self._cloud_scan(folders, "\0none").keys())
            except Exception:
                pass
        else:
            if "starred" in kinds:
                found.update(snap["starred"])
            if "liked" in kinds:
                found.update(snap["liked"])
            if "trimmed" in kinds:
                found.update(p for p in snap["trim_enabled"] if p in snap["trims"])
        return sorted(p for p in found if os.path.isfile(p))

    def _phone_build_items(self, snap):
        """Everything per clip: size, trim, game, and the unique name it gets on the phone."""
        items = []
        for p in self._phone_collect_paths(snap):
            try:
                size = os.path.getsize(p)
            except OSError:
                continue
            trim = None
            if p in snap["trim_enabled"] and p in snap["trims"]:
                try:
                    trim = [float(snap["trims"][p][0]), float(snap["trims"][p][1])]
                except Exception:
                    trim = None
            try:
                game = self._cloud_game_label(self._cloud_game_key(p))
            except Exception:
                game = "Moments"
            items.append({"path": p, "name": os.path.basename(p), "size": size, "game": game,
                          "trim": trim, "liked": p in snap["liked"], "starred": p in snap["starred"],
                          "created": mtime_from_filename(os.path.basename(p), os.path.dirname(p))})
        # Two clips with the same file name (different folders) used to overwrite
        # each other on the phone and were re-sent on every sync. The first keeps
        # its plain name, the others get a short path hash.
        seen = set()
        for it in items:
            key = it["name"].lower()
            if key not in seen:
                seen.add(key)
                it["remote"] = it["name"]
            else:
                h = hashlib.md5(os.path.normcase(it["path"]).encode("utf-8", "ignore")).hexdigest()[:6]
                stem, ext = os.path.splitext(it["name"])
                it["remote"] = "%s_%s%s" % (stem, h, ext)
                seen.add(it["remote"].lower())
        return items

    def _phone_sync_now(self):
        if not self._phone_enabled:
            return
        if getattr(self, "_phone_busy", False):
            self._phone_set_status("Already syncing, try again in a moment")
            return
        try:
            code = self._phone_code_var.get().strip()
            manual = self._phone_ip_var.get().strip()
        except Exception:
            s = _read_settings()
            code, manual = s.get("phone_token", ""), s.get("phone_manual_ip", "")
        if not code:
            self._phone_set_status("Type the pairing code shown in the ClipSync app")
            return
        self._save_setting("phone_token", code)
        self._save_setting("phone_manual_ip", manual)
        snap = self._phone_snapshot()
        self._phone_busy = True
        self._phone_set_status("Looking for your phone\u2026")
        # Scanning folders, building the list and the transfer all happen in the
        # worker, so pressing Sync never freezes the window.
        threading.Thread(target=self._phone_worker, args=(snap, code, manual),
                         daemon=True).start()

    def _phone_cut(self, src, trim):
        """Lossless trimmed copy in a temp file (same method as the star copy), or None."""
        tmp = None
        try:
            dur = get_video_duration(src, self._ffmpeg)
            if dur <= 0:
                return None
            t0, t1 = trim[0] / 10000.0 * dur, trim[1] / 10000.0 * dur
            if t1 <= t0 + 0.05:
                return None
            import tempfile
            fd, tmp = tempfile.mkstemp(suffix=os.path.splitext(src)[1] or ".mp4")
            os.close(fd)
            proc = subprocess.run(
                [self._ffmpeg, "-y", "-ss", str(t0), "-to", str(t1), "-i", src,
                 "-map", "0:v?", "-map", "0:a?", "-map_metadata", "0", "-c", "copy", tmp],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                creationflags=0x08000000 | 0x00004000)       # no window + below-normal priority
            if proc.returncode == 0 and os.path.getsize(tmp) > 0:
                return tmp
        except Exception as e:
            _debug_log("phone sync trim failed: %s" % e)
        if tmp and os.path.exists(tmp):
            try:
                os.remove(tmp)
            except Exception:
                pass
        return None

    def _phone_worker(self, snap, code, manual_ip):
        synced = {}
        dirty = False
        try:
            st = _read_settings()
            ip = _find_phone(st.get("phone_ip", ""), manual_ip)
            if not ip:
                self._phone_set_status("Phone not found. Open ClipSync on the phone (same network)")
                return
            self._save_setting("phone_ip", ip)
            status, data = _phone_request(ip, code, "GET", "/list")
            if status == 401:
                self._phone_set_status("Wrong pairing code")
                return
            if status != 200:
                self._phone_set_status("Phone answered with error %s" % status)
                return
            on_phone = {f["name"]: f["size"] for f in json.loads(data.decode("utf-8")).get("files", [])}

            self._phone_set_status("Checking your clips\u2026")
            items = self._phone_build_items(snap)
            if not items:
                self._phone_set_status("No clips match your selection")
                return
            synced = self._phone_load_synced()

            # Work out what really needs sending BEFORE sending, so the counter
            # is "3 of 91" and not the clip's position among all clips.
            todo, meta, skipped = [], [], 0
            for it in items:
                p, rn = it["path"], it["remote"]
                sig = [it["size"], it["trim"]]
                entry = {"file": rn, "game": it["game"], "tags": [],
                         "liked": bool(it.get("liked", False)), "starred": bool(it.get("starred", False)),
                         "trimmed": it["trim"] is not None,
                         "created": int((it.get("created") or 0) * 1000)}
                ps = on_phone.get(rn)
                # Already there = same name and (same size, or we recorded sending this exact version).
                # The size check also covers a lost/reset record (e.g. after the app folder rename).
                if ps is not None and ((it["trim"] is None and ps == it["size"]) or synced.get(p) == sig):
                    if synced.get(p) != sig:
                        synced[p] = sig
                        dirty = True
                    skipped += 1
                    meta.append(entry)
                else:
                    todo.append((it, sig, entry))
            if dirty:
                self._phone_save_synced(synced)
                dirty = False

            total = len(todo)
            sent = failed = 0
            last_ui = [0.0, -1]
            last_save = time.monotonic()
            for n, (it, sig, entry) in enumerate(todo, 1):
                p, rn = it["path"], it["remote"]
                send_path, tmp = p, None
                if it["trim"] and self._ffmpeg:
                    self._phone_set_status("Cutting trimmed clip %d of %d\u2026" % (n, total))
                    tmp = self._phone_cut(p, it["trim"])
                    if tmp:
                        send_path = tmp

                t_start = [time.monotonic()]

                def _prog(done, size_, n=n, rn=rn):
                    now = time.monotonic()
                    pct = int(done * 100 / size_) if size_ else 100
                    if now - last_ui[0] > 0.5 and pct != last_ui[1]:
                        last_ui[0], last_ui[1] = now, pct
                        mbs = done / 1048576.0 / max(0.001, now - t_start[0])
                        self._phone_set_status("Sending %d of %d: %s (%d%%, %.1f MB/s)"
                                               % (n, total, rn, pct, mbs))
                try:
                    size = os.path.getsize(send_path)
                    ok = False
                    for attempt in range(2):          # one quiet retry for a dropped connection
                        try:
                            last_ui[1] = -1
                            t_start[0] = time.monotonic()
                            stats = {}
                            with open(send_path, "rb") as f:
                                code_, _ = _phone_request(ip, code, "PUT", "/clip/" + rn, f, size, _prog, stats)
                            if stats:
                                _debug_log("phone sync: %s %.1f MB, connect %.2fs, send %.1fs (%.1f MB/s), phone wrote for %.1fs, HTTP %s"
                                           % (rn, size / 1048576.0, stats["connect"], stats["send"],
                                              size / 1048576.0 / max(0.001, stats["send"]), stats["wait"], code_))
                            if code_ == 200:
                                ok = True
                                break
                        except Exception as e:
                            _debug_log("phone sync send failed (try %d): %s" % (attempt + 1, e))
                    if ok:
                        sent += 1
                        synced[p] = sig
                        dirty = True
                        meta.append(entry)
                    else:
                        failed += 1
                except Exception as e:
                    _debug_log("phone sync send failed: %s" % e)
                    failed += 1
                finally:
                    if tmp and os.path.exists(tmp):
                        try:
                            os.remove(tmp)
                        except Exception:
                            pass
                if dirty and time.monotonic() - last_save > 5.0:   # not after every clip
                    self._phone_save_synced(synced)
                    dirty, last_save = False, time.monotonic()

            payload = json.dumps({"clips": meta}, ensure_ascii=False).encode("utf-8")
            _phone_request(ip, code, "PUT", "/clips.json", _io.BytesIO(payload), len(payload))
            msg = "Done: %d sent, %d already on phone" % (sent, skipped)
            if failed:
                msg += ", %d failed. Press Sync to retry" % failed
            self._phone_set_status(msg)
        except Exception as e:
            _debug_log("phone sync error: %s" % e)
            self._phone_set_status("Sync failed: %s" % e)
        finally:
            if dirty:
                self._phone_save_synced(synced)
            self._phone_busy = False

    def _on_star_copy_toggle(self, value):
        self._star_copy_enabled = bool(value)
        self._save_prefs()

    def _choose_star_folder_settings(self):
        kw = {}
        if os.path.isdir(self._star_copy_folder or ""):
            kw["initialdir"] = self._star_copy_folder
        path = filedialog.askdirectory(title="Choose a folder for starred clips",
                                       parent=self, **kw)
        if path:
            self._star_copy_folder = os.path.normpath(path)
            self._save_prefs()
            self._refresh_star_settings()

    def _refresh_star_settings(self):
        """Sync the Starred clips settings rows with the current values."""
        try:
            self._star_toggle.set(self._star_copy_enabled, animate=False)
            folder = self._star_copy_folder
            self._star_path_lbl.configure(
                text=_ellipsize(folder, FONT_TINY, 330, keep="start") if folder
                else "No folder chosen yet")
            self._star_pick_btn.configure(text="Change folder" if folder else "Choose folder")
        except Exception:
            pass

    def _save_setting(self, key, value):
        s = _read_settings()
        s[key] = value
        _write_settings(s)

    def _make_stat_dot(self):
        """Small anti-aliased dot + gap, as tall as one FONT_UI text line, with the
        dot centred on the digit height (not on the line box)."""
        import tkinter.font as _tkfont
        f      = _tkfont.Font(font=FONT_UI)
        asc    = f.metrics("ascent")
        line   = f.metrics("linespace")
        em     = line / 1.33                      # Segoe UI line height ~1.33 em
        cy     = asc - (0.70 * em) / 2.0          # digit centre, measured from top
        d, gap = 3.0, 6
        S      = 8
        w, h   = int(d) + gap, int(line)
        img = _PILImage.new("RGBA", (w * S, h * S), (0, 0, 0, 0))
        dr  = _PILDraw.Draw(img)
        r   = d / 2.0 * S
        cx  = r
        dr.ellipse([cx - r, cy * S - r, cx + r, cy * S + r], fill=TEXT_DIM)
        return _PILTk.PhotoImage(img.resize((w, h), _PILImage.LANCZOS))

    def _set_stat(self, lbl, text):
        """Header stat label: shows the leading dot only when there is text."""
        lbl.configure(text=text, image=self._stat_dot if text else "", compound="left")

    def _on_loop_toggle(self, value):
        self._loop_clip = bool(value)
        self._save_setting("loop_clip", self._loop_clip)

    def _on_trim_gpu_toggle(self, value):
        self._trim_gpu = bool(value)
        self._save_setting("trim_use_gpu", self._trim_gpu)

    def _on_recycle_toggle(self, value):
        self._use_recycle_bin = bool(value)
        self._save_setting("recycle_bin", self._use_recycle_bin)

    # ── Remember window size ──────────────────────────────────────────────
    def _on_remember_size_toggle(self, value):
        self._remember_size = bool(value)
        self._save_setting("remember_window_size", self._remember_size)
        if self._remember_size:
            self._size_ready = True
            self._save_window_size()

    def _queue_size_save(self, e=None):
        if e is not None and e.widget is not self:
            return
        if not (self._remember_size and self._size_ready):
            return
        if self._size_save_job:
            try:
                self.after_cancel(self._size_save_job)
            except Exception:
                pass
        self._size_save_job = self.after(500, self._save_window_size)

    def _save_window_size(self):
        """Store the window's outer size (and whether it was maximised)."""
        self._size_save_job = None
        if not (self._remember_size and self._size_ready) or self._fullscreen:
            return
        try:
            from ctypes import wintypes
            u = ctypes.windll.user32
            hwnd = ctypes.c_void_p(self._hwnd())
            if u.IsIconic(hwnd) or not self.winfo_viewable():
                return
            if self._maximized:
                nr = getattr(self, "_normal_rect", None)
                data = {"w": nr[2], "h": nr[3], "max": True} if nr else dict(self._saved_win or {}, max=True)
            else:
                r = wintypes.RECT()
                u.GetWindowRect(hwnd, ctypes.byref(r))
                data = {"w": r.right - r.left, "h": r.bottom - r.top, "max": False}
            if data.get("w") and data.get("h") and data != self._saved_win:
                self._saved_win = data
                self._save_setting("window", data)
        except Exception:
            pass

    def _restore_saved_size(self, tries=0):
        if not (self._remember_size and self._saved_win):
            self._size_ready = True
            return
        if (not getattr(self, "_tb_styled", False) or not self.winfo_viewable()) and tries < 20:
            self.after(150, lambda: self._restore_saved_size(tries + 1))
            return
        try:
            from ctypes import wintypes
            wx, wy, ww, wh = self._work_area()
            w = max(600, min(int(self._saved_win["w"]), ww))
            h = max(400, min(int(self._saved_win["h"]), wh))
            r = wintypes.RECT()
            ctypes.windll.user32.GetWindowRect(ctypes.c_void_p(self._hwnd()), ctypes.byref(r))
            x = min(max(r.left, wx), wx + ww - w)
            y = min(max(r.top, wy), wy + wh - h)
            self._set_outer_rect(x, y, w, h)
            if self._saved_win.get("max") and not self._maximized:
                self.after(200, self._toggle_maximize)
        except Exception:
            pass
        self.after(800, lambda: setattr(self, "_size_ready", True))

    def _on_tray_toggle(self, value):
        self._close_to_tray = bool(value)
        self._save_setting("close_to_tray", self._close_to_tray)

    def _settings_scroll(self, event):
        sc = getattr(self, "_settings_canvas", None)
        if sc is None:
            return
        lo, hi = sc.yview()
        if hi - lo >= 1.0:
            return                              # everything fits, nothing to scroll
        self._close_theme_popup()               # the popup is anchored to the unscrolled page
        self._close_cloud_games_popup()
        sc.yview_scroll(int(-1 * (event.delta / 120)), "units")

    # ── Accent colour ─────────────────────────────────────────────────────
    def _refresh_accent_settings(self):
        """Sync the accent row (swatch, hex, reset button) with the current theme."""
        try:
            name = _CUR_THEME
            ov = _accent_override(name)
            shown = ov or THEMES[name]["accent"]
            c = self._accent_swatch
            c.delete("all")
            h = int(c.cget("height"))
            _aa_oval(c, 5, (h - 14) // 2, 19, (h - 14) // 2 + 14,
                     fill=ACCENT, outline=_c("#52527e"), width=1)
            self._accent_btn.configure(text=shown.upper())
            self._accent_desc.configure(
                text=f"Custom accent for {name}" if ov else f"Using the {name} theme's own accent")
            if ov:
                if not self._accent_div.winfo_manager():
                    self._accent_div.pack(side="left", fill="y", pady=7)
                    self._accent_reset.pack(side="left")
            else:
                self._accent_div.pack_forget()
                self._accent_reset.pack_forget()
        except Exception:
            pass

    def _reset_accent(self):
        self._set_accent_override(_CUR_THEME, None)

    def _set_accent_override(self, name, color):
        """Save (or clear, with None) the custom accent of one theme and, if
        that theme is the active one, apply it live."""
        ov = {k: v for k, v in _ACCENT_OVERRIDES.items()}
        if color:
            ov[name] = color.lower()
        else:
            ov.pop(name, None)
        self._save_setting("accent_overrides", ov)
        _ACCENT_OVERRIDES.clear()
        _ACCENT_OVERRIDES.update(ov)
        if name == _CUR_THEME:
            old_map, new_map = _set_active_theme(name)
            self._retheme_widgets(old_map, new_map)
            self._apply_dark_titlebar()
            for _b in (self._tb_min, self._tb_max, self._tb_close):
                _b._draw()
        self._refresh_accent_settings()

    def _open_accent_dialog(self):
        name = _CUR_THEME
        default = THEMES[name]["accent"].lower()
        start = _accent_override(name) or default

        dlg = tk.Toplevel(self, bg=CARD_BG)
        dlg.title("Accent color"); dlg.resizable(False, False)
        dlg.grab_set(); dlg.focus_set()
        try:
            hwnd  = ctypes.windll.user32.GetParent(dlg.winfo_id())
            value = ctypes.c_int(1)
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 20, ctypes.byref(value), ctypes.sizeof(value))
            r, g, b = _hex_to_rgb255(CARD_BG)
            color = ctypes.c_int(r | (g << 8) | (b << 16))
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 35, ctypes.byref(color), ctypes.sizeof(color))
        except Exception:
            pass

        SVW, SVH, HW, HH = 280, 170, 280, 22
        pad = tk.Frame(dlg, bg=CARD_BG, padx=24, pady=20)
        pad.pack(fill="both", expand=True)
        tk.Label(pad, text=f"Accent color for {name}", font=FONT_UI, bg=CARD_BG,
                 fg=TEXT_PRIMARY).pack(anchor="w", pady=(0, 12))

        sv = tk.Canvas(pad, width=SVW, height=SVH, bg=CARD_BG, highlightthickness=0,
                       bd=0, cursor="crosshair")
        sv.pack()
        hb = tk.Canvas(pad, width=HW, height=HH, bg=CARD_BG, highlightthickness=0,
                       bd=0, cursor="sb_h_double_arrow")
        hb.pack(pady=(10, 0))
        hb._photo = _PILTk.PhotoImage(_hue_bar_image(HW, 12, CARD_BG))
        hb.create_image(0, (HH - 12) // 2, anchor="nw", image=hb._photo)

        row_ = tk.Frame(pad, bg=CARD_BG)
        row_.pack(fill="x", pady=(14, 0))
        prev = tk.Canvas(row_, width=44, height=30, bg=CARD_BG, highlightthickness=0, bd=0)
        prev.pack(side="left")
        entry_var = tk.StringVar()
        entry = tk.Entry(row_, textvariable=entry_var, font=FONT_UI, width=10,
                         bg=_c("#242444"), fg=TEXT_PRIMARY, insertbackground=TEXT_PRIMARY,
                         relief="flat")
        entry.pack(side="left", padx=(12, 0), ipady=6)
        tk.Label(row_, text=f"Theme default  {default.upper()}", font=FONT_TINY,
                 bg=CARD_BG, fg=TEXT_DIM).pack(side="right")

        r0, g0, b0 = _hex_to_rgb(start)
        h0, s0, v0 = colorsys.rgb_to_hsv(r0, g0, b0)
        st = {"h": h0, "s": s0, "v": v0}
        busy = [False]

        def hex_now():
            return _rgb_to_hex(*colorsys.hsv_to_rgb(st["h"], st["s"], st["v"]))

        def draw_sv_bg():
            sv._photo = _PILTk.PhotoImage(_sv_image(st["h"], SVW, SVH, CARD_BG))
            sv.delete("bg")
            sv.create_image(0, 0, anchor="nw", image=sv._photo, tags="bg")
            sv.tag_lower("bg")

        def draw_sv_mk():
            sv.delete("mk")
            x, y = st["s"] * (SVW - 1), (1 - st["v"]) * (SVH - 1)
            _aa_oval(sv, x - 8, y - 8, x + 8, y + 8, outline="#000000", width=1, tags="mk")
            _aa_oval(sv, x - 7, y - 7, x + 7, y + 7, outline="#ffffff", width=2, tags="mk")

        def draw_hue_mk():
            hb.delete("mk")
            x = st["h"] * (HW - 1)
            x = max(3, min(HW - 3, x))
            _rounded_rect(hb, x - 3, 1, x + 3, HH - 1, 3, fill="#000000", tags="mk")
            _rounded_rect(hb, x - 2, 2, x + 2, HH - 2, 2, fill="#ffffff", tags="mk")

        def sync(from_entry=False):
            c = hex_now()
            prev.delete("all")
            _rounded_rect(prev, 0, 0, 44, 30, 8, fill=_c("#52527e"))
            _rounded_rect(prev, 1, 1, 43, 29, 7, fill=c)
            if not from_entry:
                busy[0] = True
                entry_var.set(c.upper())
                busy[0] = False

        def sv_set(e):
            st["s"] = max(0.0, min(1.0, e.x / (SVW - 1)))
            st["v"] = 1.0 - max(0.0, min(1.0, e.y / (SVH - 1)))
            draw_sv_mk(); sync()

        def hue_set(e):
            st["h"] = max(0.0, min(1.0, e.x / (HW - 1)))
            draw_sv_bg(); draw_sv_mk(); draw_hue_mk(); sync()

        def on_entry(*_):
            if busy[0]:
                return
            v = entry_var.get().strip()
            v = v if v.startswith("#") else "#" + v
            if re.fullmatch(r"#[0-9a-fA-F]{6}", v):
                r_, g_, b_ = _hex_to_rgb(v)
                h_, s_, v_ = colorsys.rgb_to_hsv(r_, g_, b_)
                st["h"] = h_ if (s_ > 0 and v_ > 0) else st["h"]
                st["s"], st["v"] = s_, v_
                draw_sv_bg(); draw_sv_mk(); draw_hue_mk(); sync(from_entry=True)

        for w_, fn in ((sv, sv_set), (hb, hue_set)):
            w_.bind("<ButtonPress-1>", fn)
            w_.bind("<B1-Motion>", fn)
        entry_var.trace_add("write", on_entry)

        draw_sv_bg(); draw_sv_mk(); draw_hue_mk(); sync()

        tk.Frame(dlg, bg=_c("#2A2A4A"), height=1).pack(fill="x", pady=(12, 0))
        btn_row = tk.Frame(dlg, bg=CARD_BG, padx=16, pady=12)
        btn_row.pack(fill="x")

        def _save(event=None):
            v = entry_var.get().strip()
            v = v if v.startswith("#") else "#" + v
            chosen = v.lower() if re.fullmatch(r"#[0-9a-fA-F]{6}", v) else hex_now()
            dlg.destroy()
            self._set_accent_override(name, None if chosen == default else chosen)

        def _use_default():
            dlg.destroy()
            self._set_accent_override(name, None)

        RoundedButton(btn_row, text="Cancel", command=dlg.destroy,
                      fg=TEXT_PRIMARY, bg=_c("#242444"), hover_bg=_c("#2A2A4A"),
                      font=FONT_UI, padx=14, pady=6).pack(side="right", padx=(6, 0))
        RoundedButton(btn_row, text="Save", command=_save,
                      fg=ACCENT_FG, bg=ACCENT, hover_bg=_c("#FF5C8D"),
                      font=FONT_BOLD, padx=14, pady=6).pack(side="right")
        RoundedButton(btn_row, text="Use theme default", command=_use_default,
                      fg=TEXT_MUTED, bg=_c("#242444"), hover_bg=_c("#2A2A4A"),
                      font=FONT_UI, padx=10, pady=6).pack(side="left")

        dlg.bind("<Return>", _save)
        dlg.bind("<Escape>", lambda e: dlg.destroy())
        self.update_idletasks(); dlg.update_idletasks()
        pw, ph = self.winfo_width(), self.winfo_height()
        px, py = self.winfo_rootx(), self.winfo_rooty()
        dw, dh = dlg.winfo_reqwidth(), dlg.winfo_reqheight()
        dlg.geometry(f"+{px + (pw-dw)//2}+{py + (ph-dh)//2}")

    # ── Thumbnail cache ───────────────────────────────────────────────────
    def _current_cache_files(self):
        """Cached thumbnail files belonging to the clips of the folder(s)
        currently loaded (nothing for other folders)."""
        out = []
        for v in list(self.videos):
            try:
                f = os.path.join(CACHE_DIR, cache_key(v))
                if os.path.isfile(f):
                    out.append(f)
            except Exception:
                pass
        return out

    def _refresh_cache_info(self):
        lbl = getattr(self, "_cache_info_lbl", None)
        if lbl is None:
            return
        lbl.configure(text="Calculating…")

        def _work():
            files = self._current_cache_files()
            total = 0
            for f in files:
                try:
                    total += os.path.getsize(f)
                except Exception:
                    pass
            self._cache_count = len(files)
            if not self.videos:
                txt = "No folder loaded"
            elif not files:
                txt = "No cached thumbnails for the current folder"
            else:
                size = (f"{total / 2**30:.2f} GB" if total >= 2**30 else
                        f"{total / 2**20:.1f} MB" if total >= 2**20 else
                        f"{max(total // 1024, 1)} KB")
                txt = f"{len(files)} thumbnail{'s' if len(files) != 1 else ''} • {size} for the current folder"
            self.after(0, lambda: lbl.configure(text=txt))
        threading.Thread(target=_work, daemon=True).start()

    def _clear_thumbnail_cache(self):
        if not self.videos or not getattr(self, "_cache_count", 0):
            return
        msg = (f"Are you sure you want to delete the cached thumbnails for the "
               f"{len(self.videos)} clips in the current folder?\n\n"
               f"They're recreated automatically the next time they're needed. "
               f"Your clips aren't touched.")
        if not self._dark_dialog("Clear thumbnail cache", msg, yes_text="Clear"):
            return

        def _work():
            for f in self._current_cache_files():
                try:
                    os.remove(f)
                except Exception:
                    pass

            def _done():
                self.thumbnails.clear()
                self._vg_photo_refs.clear()
                self._thumbs_dirty = True        # grid is rebuilt when Settings closes
                self._refresh_cache_info()
            self.after(0, _done)
        threading.Thread(target=_work, daemon=True).start()

    def _rebuild_after_cache_clear(self):
        self._render_grid()
        threading.Thread(target=self._load_thumbnails, daemon=True).start()

    def _paint_swatch(self, canvas, name, y0=None):
        h = int(canvas.cget("height"))
        d = 14
        y = (h - d) // 2 if y0 is None else y0
        for i, colr in enumerate(_theme_swatch_colors(name)):
            _aa_oval(canvas, 1 + i * 15, y, 1 + i * 15 + d, y + d,
                     fill=colr, outline=_c("#52527e"), width=1)

    def _draw_theme_swatch(self):
        c = self._theme_swatch
        c.delete("all")
        self._paint_swatch(c, _CUR_THEME)
        self._theme_btn.configure(text=_CUR_THEME)

    def _toggle_theme_popup(self):
        if self._theme_popup_open:
            self._close_theme_popup()
            return
        page = self._settings_page
        pop = RoundedPopup(page)
        self._theme_popup = pop
        for name in THEME_ORDER:
            ThemeRow(pop.inner, name, lambda n=name: self._on_theme_pick(n),
                     current=(name == _CUR_THEME)).pack(fill="x", pady=1)
        field = self._theme_field
        pw = max(field.winfo_width(), 210)
        x = field.winfo_rootx() - page.winfo_rootx() + field.winfo_width() - pw
        y = field.winfo_rooty() - page.winfo_rooty() + field.winfo_height() + 4
        self._place_popup_settled(pop, x, y, pw)
        pop.lift()
        self._theme_popup_open = True

    def _close_theme_popup(self):
        if self._theme_popup is not None:
            try:
                self._theme_popup.destroy()
            except Exception:
                pass
        self._theme_popup = None
        self._theme_popup_open = False

    def _theme_popup_outside(self, event):
        if not self._theme_popup_open:
            return
        w = event.widget
        while w is not None:
            if w is self._theme_popup or w is self._theme_btn:
                return
            try:
                w = w.master
            except Exception:
                break
        self._close_theme_popup()

    def _on_theme_pick(self, name):
        self._close_theme_popup()
        if name != _CUR_THEME:
            self._apply_theme(name)

    def _apply_theme(self, name):
        old_map, new_map = _set_active_theme(name)
        self._retheme_widgets(old_map, new_map)
        self._apply_dark_titlebar()      # re-tint DWM border/caption for the new theme
        for _b in (self._tb_min, self._tb_max, self._tb_close):
            _b._draw()
        s = _read_settings()
        s["theme"] = name
        _write_settings(s)
        self._refresh_accent_settings()

    def _retheme_widgets(self, old_map, new_map):
        """Live theme switch: every colour currently on screen belongs to the
        old theme, so translate old -> new on every widget, canvas item and
        custom-widget attribute, then redraw the anti-aliased/image shapes."""
        remap = {}
        for base, old in old_map.items():
            new = new_map.get(base)
            if new and old.lower() != new.lower():
                remap[old.lower()] = new
        if not remap:
            return

        def cv(v):
            try:
                return remap.get(str(v).lower())
            except Exception:
                return None

        WIDGET_OPTS = ("bg", "fg", "activebackground", "activeforeground",
                       "highlightbackground", "highlightcolor", "disabledforeground",
                       "insertbackground", "selectbackground", "selectforeground",
                       "troughcolor", "disabledbackground", "readonlybackground")
        ATTRS = ("_fg", "_bg", "_hover_bg", "_act_bg", "_current_bg", "_hover_fg",
                 "_press_bg", "_fill", "_border")
        ITEM_OPTS = ("fill", "outline", "activefill", "activeoutline")

        widgets = []

        def walk(w):
            widgets.append(w)
            for ch in w.winfo_children():
                walk(ch)
        walk(self)

        for w in widgets:
            for opt in WIDGET_OPTS:
                try:
                    cur = w.cget(opt)
                except Exception:
                    continue
                nv = cv(cur)
                if nv:
                    try:
                        w.configure(**{opt: nv})
                    except Exception:
                        pass
            for a in ATTRS:
                if hasattr(w, a):
                    nv = cv(getattr(w, a))
                    if nv:
                        setattr(w, a, nv)
            if isinstance(w, tk.Canvas):
                try:
                    items = w.find_all()
                except Exception:
                    items = ()
                for item in items:
                    try:
                        if w.type(item) == "image":
                            continue
                    except Exception:
                        continue
                    for opt in ITEM_OPTS:
                        try:
                            cur = w.itemcget(item, opt)
                        except Exception:
                            continue
                        nv = cv(cur)
                        if nv:
                            try:
                                w.itemconfigure(item, **{opt: nv})
                            except Exception:
                                pass

        # Redraw everything whose look is baked into images
        for w in widgets:
            try:
                if isinstance(w, (RoundedButton, GhostButton, ToggleSwitch)):
                    w._draw()
                elif isinstance(w, Pill):
                    w._fit()
                elif isinstance(w, SlimSlider):
                    w._full_redraw()
                elif isinstance(w, TrackRow):
                    w._draw()
                elif (type(w) is tk.Canvas and callable(getattr(w, "_draw", None))
                        and w not in (self.trim_btn, self.star_btn, self.heart_btn)):
                    w._draw()
            except Exception:
                pass
        try:
            self.star_btn._draw(starred=self.star_btn._starred)
            self.heart_btn._draw(liked=self.heart_btn._liked)
            self._update_trim_btn()
        except Exception:
            pass
        try:
            self._sb.event_generate("<Configure>")
        except Exception:
            pass
        try:
            # Card thumbnails are only re-placed when their size/position
            # changes; forget the placement so placeholders/overlays redraw.
            self._vg_thumb_cvs.clear()
            self._vg_thumb_pos.clear()
            self._vg_refresh()
        except Exception:
            pass
        if self._settings_page is not None and self._settings_built:
            self._draw_theme_swatch()
        self.update_idletasks()

    # ── MPV backend ───────────────────────────────────────────────────────────

    def _start_mpv(self):
        """Create an in-process libmpv instance (via the DLL) and embed it
        directly into our canvas — no separate mpv.exe process, no window."""
        self.update_idletasks()

        # New generation: invalidates anything still referencing a previous
        # mpv instance (mirrors the old subprocess-generation guard).
        self._mpv_generation += 1
        gen = self._mpv_generation

        canvas_hwnd = self.video_canvas.winfo_id()

        def _create():
            """Runs off the Tk main thread — libmpv init can take a beat."""
            try:
                _opts = dict(
                    wid=str(canvas_hwnd),
                    vo="gpu",
                    gpu_api="d3d11",             # fixed renderer, same on every machine
                    icc_profile_auto=False,      # no auto ICC colour correction (washed-out look)
                    target_colorspace_hint=False,
                    keep_open="yes",
                    idle="yes",
                    input_default_bindings=False,
                    input_vo_keyboard=False,
                    osc=False,
                    osd_bar=False,
                    border=False,
                    cursor_autohide="always",
                    # python-mpv's background event thread decodes mpv's event
                    # data in Python and was crashing the whole process (access
                    # violation in python313.dll). Don't start it at all; events
                    # are drained from the Tk thread in _poll_mpv_props instead.
                    start_event_thread=False,
                )
                # Test: the crash log shows exception 0xE24C4A02, which is LuaJIT
                # (mpv's built-in Lua scripts). We don't use any Lua script, so
                # turn them off. Falls back if this libmpv rejects the options.
                player = None
                _tiers = [
                    dict(ytdl=False, load_scripts=False, load_stats_overlay=False,
                         load_console=False, load_auto_profiles=False),
                    dict(ytdl=False, load_scripts=False),
                    dict(),
                ]
                for _extra in _tiers:
                    try:
                        player = _mpv_mod.MPV(**_extra, **_opts)
                        _debug_log(f"libmpv created with extra options: {sorted(_extra)}")
                        break
                    except Exception as e:
                        _debug_log(f"libmpv rejected {sorted(_extra)} ({e}); trying next")
                if player is None:
                    raise RuntimeError("could not create libmpv instance")
            except Exception as e:
                _debug_log(f"Failed to create libmpv instance: {e}")
                return

            if gen != self._mpv_generation:
                try:
                    player.terminate()
                except Exception:
                    pass
                return

            self._post_main(lambda: self._on_mpv_ready(player, gen))

        threading.Thread(target=_create, daemon=True).start()

    def _on_mpv_ready(self, player, gen):
        """Runs on the Tk main thread once the libmpv instance exists."""
        if gen != self._mpv_generation:
            try:
                player.terminate()
            except Exception:
                pass
            return

        self._mpv = player
        self._mpv_proc = player  # kept for back-compat truthiness checks elsewhere
        self._mpv_ipc_ready = True

        try:
            player.volume = self._mpv_volume
        except Exception:
            pass
        try:
            self._apply_speed(player)
        except Exception:
            pass

        # NO property observers: python-mpv decodes their data on its own event
        # thread, and that code crashed Python (access violation in mpv.py's
        # "value") while seeking. The same values are polled from the Tk main
        # thread instead (_poll_mpv_props).
        self._poll_mpv_props(player, gen)

        if self._checked_folders:
            self.after(0, lambda: self._load_folders(self._checked_folders))

    def _poll_mpv_props(self, player, gen):
        """Main-thread replacement for the old mpv property observers."""
        if gen != self._mpv_generation or getattr(self, "_mpv", None) is not player:
            return
        need_tracks = False
        # Drain libmpv's event queue without decoding anything (so it never
        # overflows). Pure pointer return, result intentionally ignored.
        try:
            _h = getattr(player, "_event_handle", None)
            if _h:
                for _ in range(64):
                    _ev = _mpv_mod._mpv_wait_event(_h, 0).contents
                    if _ev.event_id.value == _mpv_mod.MpvEventID.NONE:
                        break
        except Exception:
            pass
        # Only trust position / duration once mpv reports the file we asked
        # for. (Safety net: after 3 s accept anything, so a path-format
        # mismatch can never freeze the progress bar.)
        loaded = True
        expected = getattr(self, "_mpv_loading", None)
        if expected is not None:
            try:
                cur = player.path
                loaded = bool(cur) and (
                    os.path.normcase(os.path.normpath(str(cur)))
                    == os.path.normcase(os.path.normpath(str(expected))))
            except Exception:
                loaded = False
            if not loaded and time.monotonic() - getattr(self, "_mpv_load_t", 0) > 3.0:
                loaded = True
        if loaded:
            try:
                v = player.time_pos
                if v is not None:
                    self._mpv_pos = float(v)
            except Exception:
                pass
            try:
                d = player.duration
                if d is not None:
                    d = float(d)
                    if d > 0 and self._mpv_duration <= 0:
                        need_tracks = True      # a new clip just became known
                    self._mpv_duration = d
            except Exception:
                pass
        try:
            ps = player.pause
            if ps is not None:
                self._mpv_playing = not bool(ps)
        except Exception:
            pass
        if need_tracks:
            try:
                self._on_track_list_changed(self._read_track_list(player))
            except Exception as e:
                _debug_log(f"track_list read error: {e}")
        self.after(40, lambda: self._poll_mpv_props(player, gen))

    def _read_track_list(self, player):
        """Read track-list as a JSON string (mpv converts node properties to
        JSON text) instead of python-mpv's recursive node decoder."""
        import json as _json
        fn = _mpv_mod.backend["mpv_get_property_string"]   # fresh ctypes func object
        fn.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        fn.restype = ctypes.c_void_p
        ptr = fn(ctypes.cast(player.handle, ctypes.c_void_p), b"track-list")
        if not ptr:
            return []
        try:
            raw = ctypes.string_at(ptr).decode("utf-8", "replace")
        finally:
            _mpv_mod._mpv_free(ptr)
        return _json.loads(raw) if raw else []

    def _post_main(self, fn):
        """Thread-safe: run fn() later on the Tk main thread."""
        self._main_q.put(fn)

    def _pump_main_q(self):
        try:
            while True:
                fn = self._main_q.get_nowait()
                try:
                    fn()
                except Exception as e:
                    _debug_log(f"main-queue callback error: {e}")
        except queue.Empty:
            pass
        try:
            self.after(15, self._pump_main_q)
        except Exception:
            pass

    def _mpv_cmd(self, *args):
        """Send a command directly to the in-process libmpv instance."""
        player = getattr(self, "_mpv", None)
        if not player:
            return
        try:
            player.command(*args)
        except Exception as e:
            _debug_log(f"mpv command error {args}: {e}")

    def _on_track_list_changed(self, tracks):
        """Game+Mic mix, applied after a short settle delay and only if the
        same clip is still loading."""
        try:
            job = getattr(self, "_amix_job", None)
            if job:
                try: self.after_cancel(job)
                except Exception: pass
                self._amix_job = None

            audio_tracks = [t for t in tracks if t.get("type") == "audio"]

            def _handler(t):
                return (t.get("metadata") or {}).get("handler_name")

            game = next((t for t in audio_tracks if _handler(t) == "Game"), None)
            mic  = next((t for t in audio_tracks if _handler(t) == "Mic"), None)
            _debug_log("audio tracks editor: %d audio track(s): %s" % (
                len(audio_tracks), [(t.get("id"), t.get("title")) for t in audio_tracks]))
            if game is not None and mic is not None and self._steel_first_only:
                # setting: SteelSeries clips start on their first track only -- no
                # mix, no switching, so no black screen. The audio tracks button
                # still works: tick the Mic there to turn it on for this clip
                # only (that choice is remembered for the clip).
                self._at_steel = {1}        # this clip's default = first track
                self._at_on_tracks(audio_tracks)
                return
            if game is not None and mic is not None:
                # SteelSeries clip: Game + Mic ticked by default, 100 %
                self._at_steel = {i for i, t in enumerate(audio_tracks, 1)
                                  if _handler(t) in ("Game", "Mic")}
                self._at_on_tracks(audio_tracks)
                return
            self._at_steel = None
            if len(audio_tracks) >= 2:
                # several audio tracks: the audio tracks editor decides what plays
                self._at_on_tracks(audio_tracks)
                return
            self._at_set_tracks(audio_tracks, False)    # nothing to pick for this clip
            if getattr(self, "_at_prepared", None):
                # a mix was set up before loading but this clip has no such tracks
                self._at_prepared = None
                try:
                    _pl = getattr(self, "_mpv", None)
                    if _pl:
                        self._mpv_set_lavfi("")
                        _pl.aid = "auto"
                        self._aid_cur = "auto"
                except Exception:
                    pass
            if game is None or mic is None:
                self._release_hold(getattr(self, "_mpv_loading", None))
                return

            filt = f'[aid{game["id"]}][aid{mic["id"]}]amix=inputs=2:duration=longest[ao]'
            gen = self._mpv_generation
            loading = getattr(self, "_mpv_loading", None)

            def _apply():
                self._amix_job = None
                if gen != self._mpv_generation or getattr(self, "_mpv_loading", None) != loading:
                    return
                player_lc = getattr(self, "_mpv", None)
                if not player_lc:
                    return
                try:
                    player_lc["lavfi-complex"] = filt
                except Exception as e:
                    _debug_log(f"lavfi-complex set error: {e}")
                # let the new audio graph settle, then start playback
                self.after(150, lambda: self._release_hold(loading))

            self._amix_job = self.after(350, _apply)
        except Exception as e:
            _debug_log(f"_on_track_list_changed exception: {e}")

    def _on_track_list_changed_old(self, tracks):
        """
        Fired whenever mpv's track-list changes (e.g. right after a new clip
        loads). If the clip has both a 'Game' and a 'Mic' audio track
        (SteelSeries Moments multi-track clips), mix them together so you
        hear both at once. Older/single-track clips just fall back to
        whatever mpv picks as the default audio track.
        """
        try:
            audio_tracks = [t for t in tracks if t.get("type") == "audio"]
            _debug_log(f"track-list audio tracks: {audio_tracks}")

            def _handler(t):
                return (t.get("metadata") or {}).get("handler_name")

            game = next((t for t in audio_tracks if _handler(t) == "Game"), None)
            mic  = next((t for t in audio_tracks if _handler(t) == "Mic"), None)

            if game is not None and mic is not None:
                gid, mid = game["id"], mic["id"]
                filt = f'[aid{gid}][aid{mid}]amix=inputs=2:duration=longest[ao]'
                _debug_log(f"setting lavfi-complex: {filt}")
                player_lc = getattr(self, "_mpv", None)
                if player_lc:
                    try:
                        player_lc["lavfi-complex"] = filt
                    except Exception as e:
                        _debug_log(f"lavfi-complex set error: {e}")
            else:
                # Nothing to do: _mpv_load already cleared lavfi-complex while idle.
                _debug_log("no Game+Mic handler_name pair found, leaving lavfi-complex cleared")
        except Exception as e:
            _debug_log(f"_on_track_list_changed exception: {e}")

    def _mpv_load(self, path):
        """Debounced: if you switch clips quickly, only the LAST one is loaded."""
        self._at_clip_reset()
        self._mpv_loading = path
        self._mpv_load_t = time.monotonic()
        # find out this clip's audio streams now (and its neighbours'), so the
        # audio mix can be set up before it opens
        self._at_prefetch(path, urgent=True)
        try:
            _d = self._vg_display
            _i = _d.index(path)
            for _j in (_i + 1, _i - 1, _i + 2, _i - 2):
                if 0 <= _j < len(_d):
                    self._at_prefetch(_d[_j])
        except Exception:
            pass
        for name in ("_load_job", "_amix_job"):
            job = getattr(self, name, None)
            if job:
                try: self.after_cancel(job)
                except Exception: pass
                setattr(self, name, None)
        # Rapid switching (held arrow key) is debounced; a single click/keypress
        # after a pause loads immediately, with no artificial wait.
        _now = time.monotonic()
        _gap = _now - getattr(self, "_last_load_req_t", 0.0)
        self._last_load_req_t = _now
        self._load_wait0 = _now
        self._load_job = self.after(0 if _gap > 0.35 else 90,
                                    lambda p=path: self._mpv_load_now(p))

    def _mpv_set_lavfi(self, value):
        """Set lavfi-complex only when it really changes: every set makes mpv
        rebuild its filter graph, which is slow on the Tk thread."""
        pl = getattr(self, "_mpv", None)
        if not pl:
            return
        if getattr(self, "_lavfi_cur", None) == value and getattr(self, "_lavfi_player", None) is pl:
            return
        pl["lavfi-complex"] = value
        self._lavfi_cur = value
        self._lavfi_player = pl

    def _mpv_load_now(self, path):
        self._load_job = None
        if getattr(self, "_mpv_loading", None) != path:
            return
        self._mpv_load_t = time.monotonic()
        player_lc = getattr(self, "_mpv", None)
        if player_lc:
            try:
                player_lc.command("stop")
            except Exception as e:
                _debug_log(f"mpv stop before load error: {e}")
            try:
                self._mpv_set_lavfi("")
            except Exception:
                pass
        if player_lc:
            try:
                if getattr(self, "_aid_cur", "auto") != "auto":
                    player_lc.aid = "auto"     # forget a track picked for the previous clip
                    self._aid_cur = "auto"
            except Exception:
                pass
        # Always open with the first audio track only (no audio graph), so the
        # clip starts instantly with a picture; the selected tracks are applied
        # right after playback has started (see _at_on_tracks). (Setting the mix
        # before the file opens gave a black screen.)
        self._at_prepared = None
        self._mpv_cmd("loadfile", path, "replace")
        # Hold playback until the Game+Mic mix is set up, otherwise the video
        # starts first and the mixed audio joins late.
        self._mpv_hold_for_mix = path
        self._mpv_pause()
        self.after(1500, lambda p=path: self._release_hold(p))   # safety net
    def _hide_stale_mpv_windows(self):
        """Only the top-most child window of the video canvas belongs to the
        live player. Hide any older leftover mpv window underneath/over it
        and make sure the live one is shown at full size."""
        try:
            u = ctypes.WinDLL("user32", use_last_error=True)
            u.GetWindow.restype = ctypes.c_void_p
            u.GetWindow.argtypes = [ctypes.c_void_p, ctypes.c_uint]
            parent = ctypes.c_void_p(self.video_canvas.winfo_id())
            kids = []
            h = u.GetWindow(parent, 5)                    # GW_CHILD (top-most)
            while h and len(kids) < 20:
                kids.append(h)
                h = u.GetWindow(ctypes.c_void_p(h), 2)    # GW_HWNDNEXT
            if len(kids) > 1:
                _debug_log(f"stale mpv windows on canvas: {len(kids) - 1}")
            for stale in kids[1:]:
                u.ShowWindow(ctypes.c_void_p(stale), 0)   # SW_HIDE
            if kids:
                w = max(self.video_canvas.winfo_width(), 1)
                hh = max(self.video_canvas.winfo_height(), 1)
                u.SetWindowPos(ctypes.c_void_p(kids[0]), None, 0, 0, w, hh,
                               SWP_NOACTIVATE | SWP_SHOWWINDOW)
        except Exception:
            pass
    
    def _release_hold(self, path):
        if getattr(self, "_mpv_hold_for_mix", None) != path:
            return
        self._mpv_hold_for_mix = None
        if getattr(self, "_mpv_loading", None) == path:
            self._mpv_play()

    def _mpv_load_old(self, path):
        # Remember which file we expect, so the poll loop ignores the OLD
        # clip's position / duration / tracks while the new one is loading.
        self._mpv_loading = path
        self._mpv_load_t = time.monotonic()
        player_lc = getattr(self, "_mpv", None)
        if player_lc:
            # Stop first: tears down the old clip and its audio graph.
            # Only then is it safe to clear the mix filter (nothing is playing).
            try:
                player_lc.command("stop")
            except Exception as e:
                _debug_log(f"mpv stop before load error: {e}")
            try:
                player_lc["lavfi-complex"] = ""
            except Exception:
                pass
        self._mpv_cmd("loadfile", path, "replace")
        # Always start playing regardless of whether the previous clip was paused
        self._mpv_play()

    def _mpv_play(self):
        player = getattr(self, "_mpv", None)
        if player:
            try:
                player.pause = False
            except Exception as e:
                _debug_log(f"_mpv_play error: {e}")

    def _mpv_pause(self):
        player = getattr(self, "_mpv", None)
        if player:
            try:
                player.pause = True
            except Exception as e:
                _debug_log(f"_mpv_pause error: {e}")

    def _mpv_seek(self, pos_seconds):
        player = getattr(self, "_mpv", None)
        if player:
            try:
                player.command("seek", pos_seconds, "absolute")
            except Exception as e:
                _debug_log(f"_mpv_seek error: {e}")

    def _mpv_set_volume(self, vol_pct):
        self._mpv_volume = int(vol_pct)
        player = getattr(self, "_mpv", None)
        if player:
            try:
                player.volume = int(vol_pct)
            except Exception as e:
                _debug_log(f"_mpv_set_volume error: {e}")

    # ── Playback speed ────────────────────────────────────────────────────────

    _SPEEDS = (0.1, 0.25, 0.5, 1.0, 1.25, 1.5, 2.0)
    _SPEED_CELL_W = 38        # width of each speed in the dropdown
    _SPEED_PAD = 6            # space between the border and the speeds

    def _apply_speed(self, player):
        """Set mpv's speed. Below 0.25x mpv's pitch-correcting stretcher
        (scaletempo2) mutes the audio, so pitch correction is switched off
        there: 0.1x then plays, with a lower pitch."""
        try:
            player.audio_pitch_correction = self._play_speed >= 0.25
        except Exception as e:
            _debug_log(f"audio_pitch_correction error: {e}")
        player.speed = self._play_speed

    def _set_speed(self, spd):
        self._play_speed = float(spd)
        player = getattr(self, "_mpv", None)
        if player:
            try:
                self._apply_speed(player)
            except Exception as e:
                _debug_log(f"_set_speed error: {e}")
        try:
            path = self.videos[self.current_idx]
        except Exception:
            path = None
        if path:
            if self._play_speed != 1.0:
                self._clip_speed[path] = self._play_speed
            else:
                self._clip_speed.pop(path, None)
            self._save_setting("clip_speeds", self._clip_speed)
        try:
            self.speed_btn._draw()
        except Exception:
            pass
        self._close_speed_menu()

    def _toggle_speed_menu(self):
        """Dropdown with every speed, centred on top of the speed button."""
        if getattr(self, "_speed_menu_shown", False):
            self._close_speed_menu()
            return
        host = self          # whole window, so the pill can cross the sidebar
        pop = getattr(self, "_speed_popup", None)
        if pop is None:
            pop = RoundedPopup(host, pad=self._SPEED_PAD, radius=10,
                               border=_c("#52527e"))   # rounded rectangle + visible border
            self._speed_popup = pop
            self._speed_rows = {}
            for spd in self._SPEEDS:                    # slowest on the left
                r = SpeedRow(pop.inner, f"{spd:g}x", lambda v=spd: self._set_speed(v),
                             font=FONT_UI, height=28)
                r.configure(width=self._SPEED_CELL_W)
                r.pack(side="left", padx=0, pady=1)
                self._speed_rows[spd] = r
        for spd, r in self._speed_rows.items():         # highlight current, keep all shown
            r.set_active(spd == self._play_speed)

        host.update_idletasks()
        self.speed_btn.update_idletasks()
        pw = len(self._SPEEDS) * self._SPEED_CELL_W + 2 * self._SPEED_PAD
        pop.place(x=-10000, y=0, width=pw)
        pop.update_idletasks()
        ph = 30 + 2 * self._SPEED_PAD
        bx = self.speed_btn.winfo_rootx() - host.winfo_rootx()
        by = self.speed_btn.winfo_rooty() - host.winfo_rooty()
        # always centre the whole pill over the button (it never shifts with the speed)
        x = bx + self.speed_btn.winfo_width() // 2 - pw // 2
        x = max(0, min(x, host.winfo_width() - pw))
        y = max(0, by - ph - 4)
        pop.place(x=x, y=y, width=pw)
        pop.lift()
        self._speed_menu_shown = True
        self._speed_menu_poll()

    def _close_speed_menu(self):
        self._speed_menu_shown = False
        pid = getattr(self, "_speed_poll_id", None)
        if pid:
            try:
                self.after_cancel(pid)
            except Exception:
                pass
            self._speed_poll_id = None
        pop = getattr(self, "_speed_popup", None)
        if pop is not None:
            pop.place_forget()

    def _speed_menu_poll(self):
        """Close the menu on any left-click outside it (also catches clicks on
        the embedded mpv window, which Tk never sees)."""
        if not getattr(self, "_speed_menu_shown", False):
            return
        try:
            px, py = self.winfo_pointerxy()
            def _inside(w):
                x0, y0 = w.winfo_rootx(), w.winfo_rooty()
                return x0 <= px < x0 + w.winfo_width() and y0 <= py < y0 + w.winfo_height()
            if (user32.GetAsyncKeyState(0x01) & 0x8000) and not (
                    _inside(self._speed_popup) or _inside(self.speed_btn)):
                self._close_speed_menu()
                return
        except Exception:
            pass
        self._speed_poll_id = self.after(40, self._speed_menu_poll)

    # ── Audio tracks editor (prototype) ───────────────────────────────────────

    @staticmethod
    def _at_clean(v):
        out = []
        try:
            for x in (v or []):
                n = int(x)
                if 1 <= n <= 6 and n not in out:
                    out.append(n)
        except Exception:
            pass
        return sorted(out) or [1]

    def _at_cur_path(self):
        try:
            return self.videos[self.current_idx]
        except Exception:
            return None

    def _at_remember(self):
        """Save this clip's tracks and volumes (or forget whichever matches the
        defaults again, so later changes to the defaults still apply)."""
        path = self._at_cur_path()
        if not path or self._at_sel is None:
            return
        if set(self._at_sel) != self._at_default_sel(len(self._at_tracks)):
            self._at_clip_sel[path] = sorted(self._at_sel)
        else:
            self._at_clip_sel.pop(path, None)
        if self._at_vol:
            self._at_clip_vol[path] = dict(self._at_vol)
        else:
            self._at_clip_vol.pop(path, None)
        self._save_setting("clip_audio_tracks", self._at_clip_sel)
        self._save_setting("clip_audio_volumes",
                           {p: {str(k): v for k, v in m.items()}
                            for p, m in self._at_clip_vol.items()})
        # Redraw the card so the music icon shows / hides straight away
        try:
            idx = self._vg_display.index(path)
            self._vg_draw_card(idx, hover=(idx == self._vg_hover_idx))
            self._vg_blit_thumb(path)
        except Exception:
            pass

    def _at_eff_vol(self, n):
        """Volume of track n for this clip: its own, else the default volume."""
        if getattr(self, "_at_steel", None):
            return self._at_vol.get(n, 1.0)     # SteelSeries: settings defaults don't apply
        return self._at_vol.get(n, self._at_def_vol.get(n, 1.0))

    def _copy_clip_settings(self, src, dst):
        """Give a new file (e.g. a compressed copy) the same saved audio tracks,
        track volumes and playback speed as the clip it was made from."""
        try:
            if src in self._at_clip_sel:
                self._at_clip_sel[dst] = list(self._at_clip_sel[src])
                self._save_setting("clip_audio_tracks", self._at_clip_sel)
            if src in self._at_clip_vol:
                self._at_clip_vol[dst] = dict(self._at_clip_vol[src])
                self._save_setting("clip_audio_volumes",
                                   {p: {str(k): v for k, v in m.items()}
                                    for p, m in self._at_clip_vol.items()})
            if src in self._clip_speed:
                self._clip_speed[dst] = self._clip_speed[src]
                self._save_setting("clip_speeds", self._clip_speed)
        except Exception as e:
            _debug_log(f"copy clip settings error: {e}")

    def _at_default_sel(self, count):
        """Default tracks that this clip really has; track 1 if none match."""
        if getattr(self, "_at_steel", None):
            return set(self._at_steel)          # SteelSeries: Game + Mic
        sel = {n for n in self._at_defaults if n <= count}
        return sel or {1}

    def _at_changed(self):
        if not self._at_ctl or self._at_sel is None:
            return False
        return (set(self._at_sel) != self._at_default_sel(len(self._at_tracks))
                or bool(self._at_vol))

    def _at_refresh_btn(self):
        try:
            self.audio_btn._draw()
        except Exception:
            pass

    def _at_label(self, n, t):
        title = str(t.get("title") or "").strip()
        hn = str((t.get("metadata") or {}).get("handler_name") or "")
        if hn in ("Game", "Mic"):
            title = hn
        return f"{n}  {title}" if title else f"Track {n}"

    def _at_set_tracks(self, tracks, ctl):
        self._at_tracks = list(tracks)
        self._at_ctl = bool(ctl)
        if not ctl:
            self._at_sel = None
        self._at_refresh_btn()

    def _at_clip_reset(self):
        self._at_tracks, self._at_sel, self._at_ctl = [], None, False
        self._at_vol = {}
        self._at_steel = None
        self._at_prepared = None
        try:
            self._close_audio_menu()
        except Exception:
            pass
        self._at_refresh_btn()

    def _at_on_tracks(self, audio_tracks):
        """A clip with several audio tracks just became known: start from the
        default tracks, apply them, then let playback start."""
        self._at_tracks = list(audio_tracks)
        saved = self._at_clip_sel.get(self._at_cur_path()) if self._at_enabled else None
        sel = {n for n in (saved or ()) if n <= len(audio_tracks)}
        self._at_sel = sel or self._at_default_sel(len(audio_tracks))
        sv = (self._at_clip_vol.get(self._at_cur_path()) if self._at_enabled else None) or {}
        self._at_vol = {n: v for n, v in sv.items() if n <= len(audio_tracks)}
        self._at_ctl = True
        self._at_refresh_btn()
        gen = self._mpv_generation
        loading = getattr(self, "_mpv_loading", None)
        items = [(audio_tracks[k - 1]["id"], self._at_eff_vol(k))
                 for k in sorted(self._at_sel) if 0 < k <= len(audio_tracks)]
        first = audio_tracks[0]["id"]
        # start playing right away on mpv's default (first) track
        self._release_hold(loading)
        if items == [(first, 1.0)] or not items:
            return                      # already what is wanted
        if items and len(items) == 1 and items[0][0] == first and abs(items[0][1] - 1.0) < 0.005:
            return

        def _switch():
            self._amix_job = None
            if gen != self._mpv_generation or getattr(self, "_mpv_loading", None) != loading:
                return
            keep = None
            try:
                now = self._mpv.time_pos
            except Exception:
                now = None
            target = self._clip_start_target(loading)
            if target is not None and target > 0 and (now is None or now < 1.0):
                keep = target           # the jump to the start spot may not have happened yet
            else:
                keep = now
            self._at_apply_keep_pos(pos_override=keep)

        self._amix_job = self.after(60, _switch)

    def _starts_at_beginning(self, path):
        """True when this clip opens at 0:00 rather than the middle of a long clip."""
        return bool(self._start_always or path in self._start_zero)

    def _mark_start_zero(self, path):
        if not path:
            return
        self._start_zero.add(path)
        try:
            self._save_setting("start_from_beginning_clips", sorted(self._start_zero))
        except Exception:
            pass

    def _on_start_always_toggle(self, value):
        self._start_always = bool(value)
        self._save_setting("always_start_beginning", self._start_always)

    def _clip_start_target(self, path):
        """Where a freshly opened clip is meant to start (seconds): the trim
        start for a trimmed clip, the middle for a long untrimmed clip, else the
        beginning. None while the duration isn't known yet."""
        dur = self._mpv_duration
        if not path or dur <= 0:
            return None
        if path in self._trim_enabled:
            s = self._trims.get(path, [0, 10000])
            return (s[0] / 10000.0) * dur
        if dur > 15 and not self._starts_at_beginning(path):
            return dur * 0.5
        return 0.0

    @staticmethod
    def _at_build_graph(items):
        """items: [(audio track id, volume)] -> (aid, graph). aid is set when
        one untouched track can simply be picked; otherwise graph is the
        lavfi-complex string (amix divides the level by the number of inputs,
        so volume=N puts it back)."""
        if len(items) == 1 and abs(items[0][1] - 1.0) < 0.005:
            return items[0][0], ""
        if len(items) == 1:
            i, v = items[0]
            return None, f"[aid{i}]volume={v:.3f}[ao]"
        parts, labels = [], []
        for k, (i, v) in enumerate(items):
            if abs(v - 1.0) < 0.005:
                labels.append(f"[aid{i}]")
            else:
                parts.append(f"[aid{i}]volume={v:.3f}[v{k}]")
                labels.append(f"[v{k}]")
        n = len(items)
        parts.append(f"{''.join(labels)}amix=inputs={n}:duration=longest,volume={n}[ao]")
        return None, ";".join(parts)

    def _at_prefetch(self, path, urgent=False):
        """Find out a clip's audio streams in the background (ffprobe), so the
        audio mix can be set up BEFORE the clip is opened. Done for the clip
        being opened, the ones next to it and the one under the mouse."""
        if (not path or not self._ffmpeg or path in self._at_probe
                or path in self._at_probe_busy):
            return
        self._at_probe_busy.add(path)

        def _work():
            try:
                h = get_audio_handlers(path, self._ffmpeg)
            except Exception:
                h = None
            self._at_probe[path] = h
            self._at_probe_busy.discard(path)
        try:
            if urgent:
                threading.Thread(target=_work, daemon=True).start()
            else:
                self._at_probe_pool.submit(_work)
        except Exception:
            self._at_probe_busy.discard(path)

    def _at_plan(self, path, handlers):
        """The tracks / volumes that _on_track_list_changed + _at_on_tracks will
        pick once mpv reports this clip's tracks, worked out up front from the
        probed handler names. None when the clip has fewer than 2 audio tracks
        or isn't probed yet."""
        if not handlers or len(handlers) < 2:
            return None
        n = len(handlers)
        steel = ({i for i, h in enumerate(handlers, 1) if h in ("Game", "Mic")}
                 if ("Game" in handlers and "Mic" in handlers) else None)
        saved = self._at_clip_sel.get(path) if self._at_enabled else None
        sel = {k for k in (saved or ()) if k <= n}
        if not sel:
            sel = set(steel) if steel else ({k for k in self._at_defaults if k <= n} or {1})
        ov = {k: v for k, v in ((self._at_clip_vol.get(path) if self._at_enabled else None) or {}).items() if k <= n}
        items = []
        for k in sorted(sel):
            if not (0 < k <= n):
                continue
            vol = ov.get(k, 1.0) if steel else ov.get(k, self._at_def_vol.get(k, 1.0))
            items.append((k, vol))                # track id assumed == track number
        return {"n": n, "items": items} if items else None

    def _at_apply(self):
        """Make mpv play exactly the selected tracks at their volumes: one
        untouched track is picked directly; otherwise an audio graph is built."""
        player = getattr(self, "_mpv", None)
        tracks = self._at_tracks
        sel = sorted(self._at_sel or [])
        if not player or not tracks or not sel:
            return
        items = [(tracks[n - 1]["id"], self._at_eff_vol(n))
                 for n in sel if 0 < n <= len(tracks)]
        if not items:
            return
        try:
            aid, graph = self._at_build_graph(items)
            self._mpv_set_lavfi(graph)
            if aid is not None:
                player.aid = aid
                self._aid_cur = aid
        except Exception as e:
            _debug_log(f"audio tracks apply error: {e}")

    def _at_apply_keep_pos(self, pos_override=None):
        """Apply the selection while a clip plays without losing the place:
        adding a track makes mpv open another stream, and playback jumps a few
        seconds ahead. Remember the position, apply, then seek back to it."""
        player = getattr(self, "_mpv", None)
        pos = None
        try:
            pos = player.time_pos if player else None
        except Exception:
            pos = None
        if pos_override is not None:
            pos = pos_override
        self._at_apply()
        if pos is None:
            return
        self._mpv_seek_exact(pos)
        gen = self._mpv_generation
        t0 = time.monotonic()

        def _check():
            # if mpv still ended up away from where it should be, put it back
            if gen != self._mpv_generation:
                return
            try:
                pl = getattr(self, "_mpv", None)
                now = pl.time_pos
                paused = bool(pl.pause)
            except Exception:
                return
            if now is None:
                return
            expect = pos if paused else pos + (time.monotonic() - t0)
            if abs(now - expect) > 0.5:
                self._mpv_seek_exact(expect)

        self.after(350, _check)

    def _mpv_seek_exact(self, pos_seconds):
        player = getattr(self, "_mpv", None)
        if player:
            try:
                player.command("seek", pos_seconds, "absolute+exact")
            except Exception as e:
                _debug_log(f"_mpv_seek_exact error: {e}")

    def _at_toggle_track(self, n):
        if self._at_sel is None:
            return
        sel = set(self._at_sel)
        if n in sel:
            if len(sel) == 1:
                return                  # always keep at least one track playing
            sel.discard(n)
        else:
            sel.add(n)
        self._at_sel = sel
        self._at_apply_keep_pos()
        self._at_refresh_rows()
        self._at_refresh_btn()
        self._at_remember()

    def _at_set_vol(self, n, v, final):
        if abs(v - self._at_def_vol.get(n, 1.0)) < 0.005:
            self._at_vol.pop(n, None)       # same as the default: nothing to keep
        else:
            self._at_vol[n] = v
        self._at_refresh_btn()
        if final:
            if self._at_sel and n in self._at_sel:
                self._at_apply_keep_pos()
            self._at_remember()

    def _at_reset(self):
        if not self._at_ctl:
            return
        self._at_sel = self._at_default_sel(len(self._at_tracks))
        self._at_vol = {}
        self._at_apply_keep_pos()
        self._at_refresh_rows()
        self._at_refresh_btn()
        self._at_remember()

    def _at_refresh_rows(self):
        for n, r in list(self._at_rows.items()):
            try:
                r.set_checked(n in (self._at_sel or ()))
                r.set_vol(self._at_eff_vol(n))
            except Exception:
                pass

    def _at_export_snapshot(self, src):
        """Main-thread snapshot of the audio tracks/volumes to export for a clip
        (the open clip's live state, otherwise what is saved for it)."""
        if src == self._at_cur_path() and self._at_ctl and self._at_sel:
            sel = sorted(self._at_sel)
            vols = {n: self._at_eff_vol(n) for n in range(1, 7)}
            ov = dict(self._at_vol)
        else:
            saved = self._at_clip_sel.get(src)
            sel = sorted(saved) if saved else None        # None = default tracks
            ov = self._at_clip_vol.get(src, {})
            vols = {n: ov.get(n, self._at_def_vol.get(n, 1.0)) for n in range(1, 7)}
        return {"sel": sel, "vol": vols, "ov": ov}

    def _share_audio_args(self, src, snap, aac=False, abr="160k"):
        """ffmpeg output args (maps, filter, audio codec) for sharing: the
        selected tracks are mixed into ONE audio track at their volumes, since
        most players (Discord too) only play the first track. A single track at
        100 % is copied as it is (aac=True re-encodes it instead). SteelSeries
        Game+Mic clips mix all their tracks, like playback does."""
        handlers = get_audio_handlers(src, self._ffmpeg)
        if handlers is None:                       # no ffprobe: keep everything
            return ["-map", "0:v:0?", "-map", "0:a?", "-c:a", "aac", "-b:a", abr] if aac \
                else ["-map", "0:v:0?", "-map", "0:a?", "-c:a", "copy"]
        count = len(handlers)
        if count == 0:
            return ["-map", "0:v:0?"]
        if count >= 2 and "Game" in handlers and "Mic" in handlers:
            gm = [i for i, h in enumerate(handlers, 1) if h in ("Game", "Mic")]
            sel = [n for n in (snap.get("sel") or gm) if n <= count] or gm
            vols = snap.get("ov") or {}
        else:
            sel = [n for n in (snap.get("sel") or sorted(self._at_defaults)) if n <= count]
            sel = sel or sorted(self._at_default_sel(count))
            vols = snap.get("vol") or {}
        args = ["-map", "0:v:0?"]
        if len(sel) == 1 and abs(vols.get(sel[0], 1.0) - 1.0) < 0.005:
            args += ["-map", f"0:a:{sel[0] - 1}"]
            args += ["-c:a", "aac", "-b:a", abr] if aac else ["-c:a", "copy"]
            return args
        parts, labels = [], []
        for k, n in enumerate(sel):
            v = vols.get(n, 1.0)
            if abs(v - 1.0) < 0.005:
                labels.append(f"[0:a:{n - 1}]")
            else:
                parts.append(f"[0:a:{n - 1}]volume={v:.3f}[v{k}]")
                labels.append(f"[v{k}]")
        if len(sel) == 1:
            graph = parts[0].rsplit("[", 1)[0] + "[ao]"
        else:
            n = len(sel)
            parts.append(f"{''.join(labels)}amix=inputs={n}:duration=longest,volume={n}[ao]")
            graph = ";".join(parts)
        args += ["-filter_complex", graph, "-map", "[ao]", "-c:a", "aac", "-b:a", abr]
        return args

    def _toggle_audio_menu(self):
        """Dropdown listing this clip's audio tracks, centred above the button.
        It stays open while you tick several tracks."""
        if self._at_menu_shown:
            self._close_audio_menu()
            return
        pad = 6
        pop = RoundedPopup(self, pad=pad, radius=10, border=_c("#52527e"))
        self._at_popup = pop
        self._at_rows = {}
        if not self._at_ctl:
            # nothing to pick for this clip: say why and list what was found
            tk.Label(pop.inner, text=self._at_status_text(), font=FONT_TINY,
                     bg=CARD_BG, fg=TEXT_MUTED, justify="left", wraplength=210,
                     anchor="w", padx=10, pady=8).pack(fill="x")
            for n, t in enumerate(self._at_tracks, 1):
                _info = PopupRow(pop.inner, self._at_label(n, t), None,
                                 font=FONT_UI, height=28)
                _info.configure(cursor="arrow")       # read-only: no hand cursor,
                _info.unbind("<Enter>")               # and no hover highlight
                _info.unbind("<Leave>")
                _info.pack(fill="x", pady=1)
        else:
            for n, t in enumerate(self._at_tracks, 1):
                r = TrackRow(pop.inner, self._at_label(n, t),
                             lambda k=n: self._at_toggle_track(k),
                             lambda v, final, k=n: self._at_set_vol(k, v, final),
                             vol=self._at_eff_vol(n),
                             checked=(n in self._at_sel), height=30)
                r.pack(fill="x", pady=1)
                self._at_rows[n] = r
            PopupRow(pop.inner, "Use defaults", self._at_reset,
                     font=FONT_UI, height=30).pack(fill="x", pady=(4, 1))

        host = self
        host.update_idletasks()
        self.audio_btn.update_idletasks()
        pw = 340 if self._at_ctl else 240
        pop.place(x=-10000, y=0, width=pw)
        pop.update_idletasks()
        ph = pop.inner.winfo_reqheight() + 2 * pad
        bx = self.audio_btn.winfo_rootx() - host.winfo_rootx()
        by = self.audio_btn.winfo_rooty() - host.winfo_rooty()
        x = bx + self.audio_btn.winfo_width() // 2 - pw // 2
        x = max(0, min(x, host.winfo_width() - pw))
        y = max(0, by - ph - 4)
        pop.place(x=x, y=y, width=pw)
        pop.lift()
        self._at_menu_shown = True
        self._audio_menu_poll()

    def _at_status_text(self):
        n = len(self._at_tracks)
        if n == 0:
            return "No audio tracks found yet (no clip loaded, or it is still opening)"
        if n == 1:
            return "This clip has only one audio track, so there is nothing to choose"
        return "This clip's Game and Mic tracks are mixed automatically"

    def _close_audio_menu(self):
        self._at_menu_shown = False
        pid = self._at_poll_id
        if pid:
            try:
                self.after_cancel(pid)
            except Exception:
                pass
            self._at_poll_id = None
        pop = self._at_popup
        self._at_popup = None
        self._at_rows = {}
        if pop is not None:
            try:
                pop.destroy()
            except Exception:
                pass

    def _audio_menu_poll(self):
        """Close the menu on any left-click outside it (also catches clicks on
        the embedded mpv window, which Tk never sees)."""
        if not self._at_menu_shown:
            return
        try:
            px, py = self.winfo_pointerxy()
            def _inside(w):
                x0, y0 = w.winfo_rootx(), w.winfo_rooty()
                return x0 <= px < x0 + w.winfo_width() and y0 <= py < y0 + w.winfo_height()
            if (user32.GetAsyncKeyState(0x01) & 0x8000) and not (
                    _inside(self._at_popup) or _inside(self.audio_btn)):
                self._close_audio_menu()
                return
        except Exception:
            pass
        self._at_poll_id = self.after(40, self._audio_menu_poll)

    def _on_steel_first_toggle(self, value):
        self._steel_first_only = bool(value)
        self._save_setting("steelseries_first_track_only", self._steel_first_only)

    def _on_audio_btn_toggle(self, value):
        self._at_enabled = bool(value)
        self._save_setting("audio_tracks_btn", self._at_enabled)
        grp = getattr(self, "_at_def_group", None)
        if self._at_enabled:
            self.audio_btn.pack(side="right", padx=(4, 0), after=self.speed_btn)
            if grp is not None:
                grp.pack(fill="x")
        else:
            self._close_audio_menu()
            self.audio_btn.pack_forget()
            if grp is not None:
                grp.pack_forget()

    def _build_at_default_rows(self, parent):
        """Settings page: the default tracks, one row per track (tick, name,
        volume slider), laid out like the track dropdown."""
        box = tk.Frame(parent, bg=CARD_BG, width=340, height=6 * 32)
        box.pack(anchor="w", padx=(10, 0), pady=(0, 12))
        box.pack_propagate(False)
        self._at_def_rows = {}
        for n in range(1, 7):
            r = TrackRow(box, f"Track {n}",
                         lambda k=n: self._on_at_default_tick(k),
                         lambda v, final, k=n: self._on_at_default_vol(k, v, final),
                         vol=self._at_def_vol.get(n, 1.0),
                         checked=(n in self._at_defaults), height=30)
            r.pack(fill="x", pady=1)
            self._at_def_rows[n] = r

    def _on_at_default_tick(self, n):
        cur = set(self._at_defaults)
        if n in cur:
            if len(cur) == 1:
                return                  # at least one default track
            cur.discard(n)
        else:
            cur.add(n)
        self._at_defaults = sorted(cur)
        self._save_setting("default_audio_tracks", self._at_defaults)
        for k, r in self._at_def_rows.items():
            r.set_checked(k in self._at_defaults)

    def _on_at_default_vol(self, n, v, final):
        if abs(v - 1.0) < 0.005:
            self._at_def_vol.pop(n, None)
        else:
            self._at_def_vol[n] = v
        if not final:
            return
        self._save_setting("default_audio_volumes",
                           {str(k): x for k, x in self._at_def_vol.items()})
        self._at_refresh_btn()
        # the open clip follows the defaults for tracks it has no volume of its own for
        if self._at_ctl and self._at_sel and n not in self._at_vol:
            self._at_apply_keep_pos()

    # ── Click watcher ─────────────────────────────────────────────────────────

    def _on_video_resize(self, event):
        """Resize the embedded MPV window to match the canvas."""
        _trace("video_canvas configure %sx%s" % (event.width, event.height))
        if not self._mpv_hwnd:
            return
        w = max(event.width,  1)
        h = max(event.height, 1)
        user32.SetWindowPos(self._mpv_hwnd, 0, 0, 0, w, h,
                            SWP_NOACTIVATE | SWP_SHOWWINDOW)

    def _start_click_watcher(self):
        """Click on the video area toggles play/pause. Polled from Tk's own
        event loop: the old version did this on a background thread and called
        Tk (grab_current, winfo_*) from there on every mouse click anywhere in
        the window, which could crash Python (e.g. when clicking the seek bar)."""
        class POINT(ctypes.Structure):
            _fields_ = [("x", ctypes.c_long), ("y", ctypes.c_long)]
        st = {"prev": False, "last": 0.0}   # last: debounce repeat fires within 200 ms

        def poll():
            try:
                u = ctypes.windll.user32
                down = bool(u.GetAsyncKeyState(0x01) & 0x8000)
                if down and not st["prev"]:
                    # Don't fire if a modal dialog (share, delete, etc.) is open
                    if self.grab_current() is None and not self._settings_open:
                        pid = ctypes.wintypes.DWORD()
                        u.GetWindowThreadProcessId(u.GetForegroundWindow(), ctypes.byref(pid))
                        if pid.value == os.getpid() and self.current_idx is not None:
                            pt = POINT()
                            u.GetCursorPos(ctypes.byref(pt))
                            vc = self.video_canvas
                            vx, vy = vc.winfo_rootx(), vc.winfo_rooty()
                            vw, vh = vc.winfo_width(), vc.winfo_height()
                            now = time.monotonic()
                            # a click on an open dropdown (audio tracks / speed)
                            # that overlaps the video must not pause/play
                            on_popup = False
                            for _pw in (getattr(self, "_at_popup", None),
                                        getattr(self, "_speed_popup", None)):
                                try:
                                    if _pw is not None and _pw.winfo_ismapped():
                                        _x0, _y0 = _pw.winfo_rootx(), _pw.winfo_rooty()
                                        if (_x0 <= pt.x < _x0 + _pw.winfo_width()
                                                and _y0 <= pt.y < _y0 + _pw.winfo_height()):
                                            on_popup = True
                                except Exception:
                                    pass
                            in_video = (not on_popup
                                        and vx <= pt.x <= vx + vw and vy <= pt.y <= vy + vh)
                            if (in_video and self._fullscreen
                                    and now - st.get("click_t", 0.0) < 0.45):
                                # double click in fullscreen: leave fullscreen. The first
                                # click already toggled play/pause, so toggle it back.
                                st["click_t"] = 0.0
                                st["last"] = now
                                self.after(0, self._fs_double_click)
                            elif in_video and now - st["last"] > 0.2:
                                st["last"] = now
                                st["click_t"] = now
                                self.after(0, self._toggle_play)
                st["prev"] = down
            except Exception:
                pass
            self.after(30, poll)

        self.after(30, poll)

    # ── UI build ──────────────────────────────────────────────────────────────

    def _build_ui(self, init_vol=55):
        self._pane = tk.PanedWindow(self, orient="horizontal",
                                    bg=_c("#242444"), sashwidth=5,
                                    sashrelief="flat", opaqueresize=True,
                                    sashcursor="", bd=0)
        self._pane.pack(fill="both", expand=True)
        # While the sidebar sash is being dragged, the clip list is NOT re-laid out
        # (that redraws every card each frame); it updates once on release.
        self._sash_dragging = False
        self._pane.bind("<ButtonPress-1>",   self._on_sash_press,   add="+")
        self._pane.bind("<ButtonRelease-1>", self._on_sash_release, add="+")
        self._pane.bind("<B1-Motion>",       self._on_sash_motion)   # replaces Tk's own sash drag

        # ── Left panel ───────────────────────────────────────────────────────
        self.left = tk.Frame(self._pane, bg=BG)
        self._pane.add(self.left, minsize=400, stretch="always")

        hdr = tk.Frame(self.left, bg=BG, padx=16, pady=10)
        hdr.pack(fill="x")
        self._hdr_frame = hdr
        # Title, then "•  715  •  21.90 GB": one dim colour and one font size for
        # the details, all bottom-aligned so they share the title's baseline.
        # The separator dots are small images placed at the exact vertical
        # centre of the digits (a text "·" sits too low in Segoe UI).
        self._stat_dot = self._make_stat_dot()
        tk.Label(hdr, text="ALL CLIPS", font=FONT_TITLE,
                 bg=BG, fg=TEXT_PRIMARY).pack(side="left", anchor="s")
        self.clip_count = tk.Label(hdr, text="", font=FONT_UI,
                                   bg=BG, fg=TEXT_DIM)
        self.clip_count.pack(side="left", anchor="s", padx=(8, 0), pady=(0, 1))
        self.folder_size_lbl = tk.Label(hdr, text="", font=FONT_UI,
                                        bg=BG, fg=TEXT_DIM)
        self.folder_size_lbl.pack(side="left", anchor="s", padx=(8, 0), pady=(0, 1))
        # Header controls. Packed right-to-left: "Choose folder" goes in first
        # (far right), the action group sits to its left. Settings is the
        # last button of the action group.
        _cf_pill = Pill(hdr)
        _cf_pill.pack(side="right")
        self._folder_btn = GhostButton(
            _cf_pill.inner, "Folder", self._pick_folder, font=FONT_BOLD,
            fg=TEXT_PRIMARY, icon="folder-plus", icon_size=18, padx=10,
            accent_icon=True)
        self._folder_btn.pack(side="left")
        Tooltip(self._folder_btn, "Choose a clips folder")
        _cf_pill.divider()
        self._folder_arrow_btn = GhostButton(
            _cf_pill.inner, "", self._toggle_folder_dropdown,
            icon="chevron-down", icon_size=14, padx=6)
        self._folder_arrow_btn.pack(side="left")
        Tooltip(self._folder_arrow_btn, "Saved folders")
        self._folder_dropdown_open = False
        self._folder_dropdown      = None

        _actions = Pill(hdr)
        _actions.pack(side="right", padx=(0, 8))
        self._hdr_actions = _actions
        _action_items = [
            ("trash",    "Delete selected clips", self._delete_selected_btn),
            ("plus",     "Categorize clips", self._open_categorize_dialog),
            ("folder",   "Show in folder",   self._show_current_file_location),
            ("share",    "Share",            self._open_share_dialog),
            ("compress", "Compress clips",   self._open_compress_dialog),
            ("refresh",  "Refresh",          self._refresh_folder),
            ("gear",     "Settings",         self._open_settings),
        ]
        self.delete_sel_btn = _actions       # (kept: the dialog code un-hovers it)
        for _i, (_ic, _tip, _cmd) in enumerate(_action_items):
            if _i:
                _actions.divider()
            if _ic == "trash":
                # Always visible. Shows the number of selected clips beside the
                # icon once one or more are selected; turns red on hover.
                _b = GhostButton(_actions.inner, "", _cmd, icon=_ic, icon_size=18,
                                 padx=8, font=FONT_BOLD, autosize=True,
                                 text_left=True,
                                 hover_fg=_c("#ff8a94"),
                                 hover_bg=_mix_hex(CARD_BG, "#ff4455", 0.22),
                                 press_bg=_mix_hex(CARD_BG, "#ff4455", 0.10))
                self._delete_btn = _b
            else:
                _b = GhostButton(_actions.inner, "", _cmd, icon=_ic, icon_size=18, padx=8)
            _b.pack(side="left")
            Tooltip(_b, _tip)

        # "Apply trim" -- only shown while trim mode is on for the current clip
        self._apply_trim_pill = Pill(hdr)
        self._apply_trim_btn = GhostButton(
            self._apply_trim_pill.inner, "Apply trim", self._apply_trim_cut,
            font=FONT_BOLD, fg=TEXT_PRIMARY, padx=12, autosize=True)
        self._apply_trim_btn.pack(side="left")
        Tooltip(self._apply_trim_btn, "Cut the clip down to the trimmed part (permanent)")
        self._apply_trim_shown = False

        # Video
        self.video_frame = tk.Frame(self.left, bg=PLAYER_BG)
        self.video_frame.pack(fill="both", expand=True)
        self.video_canvas = tk.Canvas(self.video_frame, bg=PLAYER_BG, highlightthickness=0)
        self.video_canvas.pack(fill="both", expand=True)
        self.video_canvas.bind("<Configure>", self._on_video_resize)
        self.video_canvas.bind("<Configure>", self._queue_sash_sync, add="+")

        self.no_video_label = tk.Label(self.video_canvas, text="Select a clip to play",
                                       font=("Segoe UI", 14), bg=PLAYER_BG, fg=TEXT_DIM)
        self.no_video_label.place(relx=0.5, rely=0.5, anchor="center")

        # Controls
        controls = tk.Frame(self.left, bg=BG)
        controls.pack(fill="x")
        self._controls_frame = controls

        now_row = tk.Frame(controls, bg=BG)
        now_row.pack(fill="x", pady=(8, 2))
        self.now_playing = tk.Label(now_row, text="", font=FONT_BOLD,
                                    bg=BG, fg=TEXT_PRIMARY, anchor="w", padx=18)
        self.now_playing.pack(side="left", fill="x", expand=True)
        self.now_playing_mb = tk.Label(now_row, text="", font=FONT_TINY,
                                       bg=BG, fg=TEXT_MUTED, anchor="e", padx=18)
        self.now_playing_mb.pack(side="right")

        prog_row = tk.Frame(controls, bg=BG, padx=10)
        prog_row.pack(fill="x")
        self.time_label = tk.Label(prog_row, text="0:00", font=FONT_TINY,
                                   bg=BG, fg=TEXT_MUTED, width=4, anchor="w")
        self.time_label.pack(side="left", padx=(8, 0))

        # Pack right-side widgets BEFORE the expanding slider, or they get squeezed out
        # Fullscreen (F): far right of the progress row
        _fs_sz = 34
        self.fullscreen_btn = tk.Canvas(prog_row, width=_fs_sz, height=_fs_sz, bg=BG,
                                        highlightthickness=0, bd=0, cursor="hand2")
        self.fullscreen_btn._hover = False
        def _draw_fs_btn(hovered=None):
            if hovered is not None:
                self.fullscreen_btn._hover = hovered
            hov = self.fullscreen_btn._hover
            self.fullscreen_btn.delete("all")
            if hov:
                _aa_oval(self.fullscreen_btn, 1, 1, _fs_sz - 1, _fs_sz - 1, fill=_c("#242444"))
            col = TEXT_PRIMARY if hov else TEXT_DIM
            icon = "fullscreen-exit" if self._fullscreen else "fullscreen"
            self.fullscreen_btn.create_image(_fs_sz // 2, _fs_sz // 2,
                                             image=ui_icon(icon, 18, col, stroke=1.8),
                                             anchor="center")
        _draw_fs_btn()
        self.fullscreen_btn._draw = _draw_fs_btn
        self.fullscreen_btn.bind("<Enter>",    lambda e: _draw_fs_btn(True))
        self.fullscreen_btn.bind("<Leave>",    lambda e: _draw_fs_btn(False))
        self.fullscreen_btn.bind("<Button-1>", lambda e: self._toggle_fullscreen())
        self.fullscreen_btn.pack(side="right", padx=(4, 0))
        Tooltip(self.fullscreen_btn, "Fullscreen (F)")

        _trim_sz = 34
        self.trim_btn = tk.Canvas(prog_row, width=_trim_sz, height=_trim_sz,
                                  bg=BG, highlightthickness=0, bd=0, cursor="hand2")
        self.trim_btn._trimmed = False
        def _draw_trim_btn(hovered=False, trimmed=False):
            self.trim_btn.delete("all")
            if hovered:
                _aa_oval(self.trim_btn, 1, 1, _trim_sz-1, _trim_sz-1,
                         fill=_c("#242444"))
            col = ACCENT if trimmed else (_c("#bbbbbb") if hovered else TEXT_DIM)
            cx = cy = _trim_sz // 2
            if trimmed:
                img = ui_icon("x", 24, col, stroke=1.4)
            else:
                img = ui_icon("scissors", 17, col, stroke=1.7)
            self.trim_btn.create_image(cx, cy, image=img, anchor="center")
        _draw_trim_btn()
        self.trim_btn._draw = _draw_trim_btn
        self.trim_btn.bind("<Enter>",    lambda e: _draw_trim_btn(hovered=True,  trimmed=self.trim_btn._trimmed))
        self.trim_btn.bind("<Leave>",    lambda e: _draw_trim_btn(hovered=False, trimmed=self.trim_btn._trimmed))
        self.trim_btn.bind("<Button-1>", lambda e: self._toggle_trim())
        self.trim_btn.pack(side="right", padx=(4, 0))

        # Playback speed: click = dropdown of speeds, right-click = back to 1x
        _spd_w, _spd_h = 42, 34
        self.speed_btn = tk.Canvas(prog_row, width=_spd_w, height=_spd_h,
                                   bg=BG, highlightthickness=0, bd=0, cursor="hand2")
        def _draw_speed_btn(hovered=False):
            self.speed_btn.delete("all")
            if hovered:      # same round highlight as the trim button
                _ox = (_spd_w - _spd_h) // 2
                _aa_oval(self.speed_btn, _ox + 1, 1, _ox + _spd_h - 1, _spd_h - 1,
                         fill=_c("#242444"))
            spd = getattr(self, "_play_speed", 1.0)
            if spd != 1.0:
                col = ACCENT
            else:
                col = TEXT_PRIMARY if hovered else TEXT_MUTED
            self.speed_btn.create_text(_spd_w // 2 - 1, _spd_h // 2 - 1, text=f"{spd:g}x",
                                       fill=col, font=("Segoe UI", 10), anchor="center")
        _draw_speed_btn()
        self.speed_btn._draw = _draw_speed_btn
        self.speed_btn.bind("<Enter>",    lambda e: _draw_speed_btn(hovered=True))
        self.speed_btn.bind("<Leave>",    lambda e: _draw_speed_btn(hovered=False))
        self.speed_btn.bind("<Button-1>", lambda e: self._toggle_speed_menu())
        self.speed_btn.bind("<Button-3>", lambda e: self._set_speed(1.0))
        self.speed_btn.pack(side="right", padx=(4, 0))

        # Audio tracks: click = list of this clip's audio tracks, right-click =
        # back to the default tracks. Packed right after the speed button, so it
        # sits on its left. Not packed at all when turned off in settings.
        _at_w, _at_h = 34, 34
        self.audio_btn = tk.Canvas(prog_row, width=_at_w, height=_at_h, bg=BG,
                                   highlightthickness=0, bd=0, cursor="hand2")
        self.audio_btn._hover = False
        def _draw_audio_btn(hovered=None):
            if hovered is not None:
                self.audio_btn._hover = hovered
            hov = self.audio_btn._hover
            self.audio_btn.delete("all")
            if hov and self._at_ctl:
                _aa_oval(self.audio_btn, 1, 1, _at_w - 1, _at_h - 1, fill=_c("#242444"))
            if not self._at_ctl:
                col = TEXT_DIM
            elif self._at_changed():
                col = ACCENT
            else:
                col = TEXT_PRIMARY if hov else TEXT_MUTED
            self.audio_btn.create_text(_at_w // 2, _at_h // 2 - 1, text="\u266b",
                                       fill=col, font=("Segoe UI Symbol", 12),
                                       anchor="center")
        _draw_audio_btn()
        self.audio_btn._draw = _draw_audio_btn
        self.audio_btn.bind("<Enter>",    lambda e: _draw_audio_btn(True))
        self.audio_btn.bind("<Leave>",    lambda e: _draw_audio_btn(False))
        self.audio_btn.bind("<Button-1>", lambda e: self._toggle_audio_menu())
        self.audio_btn.bind("<Button-3>", lambda e: self._at_reset())
        if self._at_enabled:
            self.audio_btn.pack(side="right", padx=(4, 0))

        self.dur_label = tk.Label(prog_row, text="0:00", font=FONT_TINY,
                                  bg=BG, fg=TEXT_MUTED, width=4, anchor="e")
        self.dur_label.pack(side="right")

        self.progress = TrimSlider(prog_row, from_=0, to=10000, value=0,
                                   on_press=self._seek_start,
                                   on_release=self._seek_end,
                                   on_change=self._on_seek_drag,
                                   fill_color=_c("#ffffff"))
        self.progress._app = self
        self.progress.pack(side="left", fill="x", expand=True, padx=8)

        ctrl = tk.Frame(controls, bg=BG, padx=18, pady=8, height=64)
        ctrl.pack(fill="x")
        # Lock this row's height so it can never get squeezed thinner than
        # the 34px star/heart icons need — otherwise, if anything above it
        # (labels, dropdowns) renders taller than expected — which is
        # exactly what correct DPI-aware font rendering does relative to
        # the old DPI-unaware bitmap-scaled build — this, being the last
        # and lowest row, is the one that silently eats the shortfall and
        # its content gets clipped.
        ctrl.pack_propagate(False)

        # Volume
        vol_frame = tk.Frame(ctrl, bg=BG)
        vol_frame.pack(side="right", padx=(0, 7))
        _vol_img = self._btn_imgs.get("vol")
        if _vol_img:
            self.vol_icon = tk.Canvas(vol_frame, width=30, height=30,
                                      bg=BG, highlightthickness=0, bd=0, cursor="hand2")
            self.vol_icon.create_image(15, 15, image=_vol_img, anchor="center", tags="vol_img")
            self.vol_icon.bind("<ButtonRelease-1>", lambda e: self._toggle_mute())
        else:
            self.vol_icon = tk.Label(vol_frame, text="Vol", font=FONT_UI,
                                     bg=BG, fg=TEXT_MUTED)
        self._muted       = init_vol == 0
        self._pre_mute_vol = None
        if init_vol == 0:
            _mute_img = self._btn_imgs.get("mute")
            if _mute_img and isinstance(self.vol_icon, tk.Canvas):
                self.vol_icon.delete("vol_img")
                self.vol_icon.create_image(15, 15, image=_mute_img, anchor="center", tags="vol_img")
        if isinstance(self.vol_icon, tk.Canvas):
            # same round hover highlight as the trim / speed buttons
            def _vol_hover(on):
                self.vol_icon.delete("vol_hover")
                if on:
                    _aa_oval(self.vol_icon, 1, 1, 29, 29, fill=_c("#242444"), tags="vol_hover")
                    self.vol_icon.tag_lower("vol_hover", "vol_img")
            self.vol_icon.bind("<Enter>", lambda e: _vol_hover(True))
            self.vol_icon.bind("<Leave>", lambda e: _vol_hover(False))
        self.vol_icon.pack(side="left", padx=(0, 6))
        self.vol_slider = SlimSlider(vol_frame, from_=0, to=100, value=init_vol,
                                     on_change=self._set_volume, fill_color=_c("#ffffff"))
        self.vol_slider.configure(width=120)
        self.vol_slider.pack(side="left")
        self.vol_pct = tk.Label(vol_frame, text=f"{init_vol}%", font=FONT_TINY,
                                bg=BG, fg=TEXT_MUTED, width=4, anchor="e")
        self.vol_pct.pack(side="left", padx=(4, 0))

        # Transport
        transport = tk.Frame(ctrl, bg=BG)
        def _reposition_transport(e=None):
            # Always dead centre under the playback bar. If the window is narrow
            # enough that the volume control would touch the buttons, the volume
            # slider gets shorter instead of the buttons moving off-centre.
            try:
                pr = self.progress
                if pr.winfo_width() > 1:
                    cx = pr.winfo_rootx() + pr.winfo_width() // 2 - ctrl.winfo_rootx()
                else:
                    cx = ctrl.winfo_width() // 2
            except Exception:
                cx = ctrl.winfo_width() // 2
            try:
                half   = transport.winfo_reqwidth() // 2
                room   = (ctrl.winfo_width() - 18 - 7) - (cx + half + 12)   # width left for the volume control
                others = vol_frame.winfo_reqwidth() - self.vol_slider.winfo_reqwidth()
                want   = max(40, min(120, room - others))
                if want != getattr(self, "_vol_slider_w", 120):
                    self._vol_slider_w = want
                    self.vol_slider.configure(width=want)
            except Exception:
                pass
            transport.place(x=cx, rely=0.5, anchor="center")
        ctrl.bind("<Configure>", _reposition_transport)
        self.progress.bind("<Configure>", _reposition_transport, add="+")
        ctrl.after(200, _reposition_transport)

        def _skip_btn(parent, img_key, fallback, cmd, size=36):
            img = self._btn_imgs.get(img_key)
            c = tk.Canvas(parent, width=size, height=size, bg=BG,
                          highlightthickness=0, bd=0, cursor="hand2")
            def draw(hovered=False, pressed=False):
                c.delete("all")
                if hovered or pressed:
                    col = _c("#2A2A4A") if not pressed else _c("#161628")
                    _aa_oval(c, 1, 1, size-1, size-1, fill=col)
                if img:
                    c.create_image(size//2, size//2, image=img, anchor="center")
                else:
                    c.create_text(size//2, size//2, text=fallback,
                                  fill=TEXT_MUTED, font=("Segoe UI", 11, "bold"))
            draw()
            c.bind("<Enter>",           lambda e: draw(hovered=True))
            c.bind("<Leave>",           lambda e: draw())
            c.bind("<ButtonPress-1>",   lambda e: draw(pressed=True))
            c.bind("<ButtonRelease-1>", lambda e: (draw(), cmd()))
            c._draw = draw
            return c

        def _play_pause_btn(parent, size=46):
            c = tk.Canvas(parent, width=size, height=size, bg=BG,
                          highlightthickness=0, bd=0, cursor="hand2")
            c._playing = False
            def draw(hovered=False, pressed=False):
                c.delete("all")
                col = _c("#333333") if pressed else (_c("#2A2A4A") if hovered else _c("#242444"))
                _aa_oval(c, 2, 2, size-2, size-2, fill=col, outline=_c("#45456e"), width=1)
                cx, cy = size//2, size//2
                img = self._btn_imgs.get("pause" if c._playing else "play")
                if img:
                    c.create_image(cx, cy, image=img, anchor="center")
                elif c._playing:
                    bw, bh = 5, 16
                    c.create_rectangle(cx-9, cy-bh//2, cx-9+bw, cy+bh//2, fill="white", outline="")
                    c.create_rectangle(cx+4, cy-bh//2, cx+4+bw, cy+bh//2, fill="white", outline="")
                else:
                    c.create_polygon(cx-6, cy-10, cx-6, cy+10, cx+11, cy, fill="white", outline="")
            draw()
            c.bind("<Enter>",           lambda e: draw(hovered=True))
            c.bind("<Leave>",           lambda e: draw())
            c.bind("<ButtonPress-1>",   lambda e: draw(pressed=True))
            c.bind("<ButtonRelease-1>", lambda e: (draw(), self._toggle_play()))
            c._draw = draw
            return c

        _skip_btn(transport, "prev", "◀◀", self._frame_back, size=30).pack(side="left", padx=(0, 6))
        self.play_btn = _play_pause_btn(transport, size=44)
        self.play_btn.pack(side="left")
        _skip_btn(transport, "next", "▶▶", self._frame_forward, size=30).pack(side="left", padx=(6, 0))

        # Star + heart
        _star_size = 34
        self.star_btn = tk.Canvas(ctrl, width=_star_size, height=_star_size,
                                  bg=BG, highlightthickness=0, bd=0, cursor="hand2")
        self.star_btn._starred = False
        def _draw_star(hovered=False, starred=False):
            self.star_btn.delete("all")
            if hovered:
                _aa_oval(self.star_btn, 1, 1, _star_size-1, _star_size-1, fill=_c("#242444"))
            col = _c("#e8c84a") if starred else (_c("#bbbbbb") if hovered else TEXT_DIM)
            self.star_btn.create_image(_star_size//2, _star_size//2,
                                       image=ui_icon("star", 20, col, filled=starred),
                                       anchor="center")
        _draw_star()
        self.star_btn._draw = _draw_star
        self.star_btn.bind("<Enter>",    lambda e: _draw_star(hovered=True,  starred=self.star_btn._starred))
        self.star_btn.bind("<Leave>",    lambda e: _draw_star(hovered=False, starred=self.star_btn._starred))
        self.star_btn.bind("<Button-1>", lambda e: self._toggle_star())
        self.star_btn.bind("<Button-3>", lambda e: self._choose_star_folder(force=True))
        self.star_btn.pack(side="left")

        _heart_size = 34
        self.heart_btn = tk.Canvas(ctrl, width=_heart_size, height=_heart_size,
                                   bg=BG, highlightthickness=0, bd=0, cursor="hand2")
        self.heart_btn._liked = False
        def _draw_heart(hovered=False, liked=False):
            self.heart_btn.delete("all")
            if hovered:
                _aa_oval(self.heart_btn, 1, 1, _heart_size-1, _heart_size-1, fill=_c("#242444"))
            col = _c("#e05555") if liked else (_c("#bbbbbb") if hovered else TEXT_DIM)
            self.heart_btn.create_image(_heart_size//2, _heart_size//2 + 1,
                                        image=ui_icon("heart", 20, col, filled=liked),
                                        anchor="center")
        _draw_heart()
        self.heart_btn._draw = _draw_heart
        self.heart_btn.bind("<Enter>",    lambda e: _draw_heart(hovered=True,  liked=self.heart_btn._liked))
        self.heart_btn.bind("<Leave>",    lambda e: _draw_heart(hovered=False, liked=self.heart_btn._liked))
        self.heart_btn.bind("<Button-1>", lambda e: self._toggle_like())
        self.heart_btn.pack(side="left", padx=(4, 0))


        # ── Right panel (clip grid) ───────────────────────────────────────────
        right = tk.Frame(self._pane, bg=SIDEBAR_BG)
        self._right_panel = right
        self._pane.add(right, minsize=220, width=300, stretch="never")

        right.bind("<Configure>", self._clamp_sidebar_width)
        tk.Frame(right, bg=_c("#242444"), width=1).pack(side="left", fill="y")
        inner = tk.Frame(right, bg=SIDEBAR_BG)
        inner.pack(fill="both", expand=True)

        rh = tk.Frame(inner, bg=SIDEBAR_BG, padx=12, pady=10)
        rh.pack(fill="x")
        self._rh_frame = rh

        self._sort_var  = tk.StringVar(value="Newest")
        self._sort_open = False
        rh.columnconfigure(0, weight=1)

        _fpill = Pill(rh, stretch=True)
        _fpill.grid(row=0, column=0, sticky="ew")
        _fin = _fpill.inner
        for _col in (0, 2, 4):
            _fin.columnconfigure(_col, weight=1, uniform="flt")

        self._game_open = False
        self._game_btn = GhostButton(_fin, text="All", command=self._toggle_game_dropdown,
                                     fg=TEXT_PRIMARY, font=FONT_TINY, arrow=True)
        self._game_btn.grid(row=0, column=0, sticky="ew")
        _fpill.divider(row=0, column=1)

        self._filter_open = False
        self._filter_btn = GhostButton(_fin, text="All clips", command=self._toggle_filter_dropdown,
                                       fg=TEXT_PRIMARY, font=FONT_TINY, arrow=True)
        self._filter_btn.grid(row=0, column=2, sticky="ew")
        _fpill.divider(row=0, column=3)

        self._sort_btn = GhostButton(_fin, text=self._sort_var.get(), command=self._toggle_sort_dropdown,
                                     fg=TEXT_PRIMARY, font=FONT_TINY, arrow=True)
        self._sort_btn.grid(row=0, column=4, sticky="ew")

        # Calendar button (right of the dropdowns): pick a day to show only that day's clips.
        # Right-click it to clear the date.
        _dpill = Pill(rh)
        _dpill.grid(row=0, column=1, sticky="n", padx=(6, 0))
        self._date_btn = GhostButton(_dpill.inner, text="", command=self._toggle_date_popup,
                                     fg=TEXT_PRIMARY, font=FONT_TINY, icon="calendar",
                                     icon_size=16, padx=8, autosize=True)
        self._date_btn.pack()
        self._date_btn.bind("<Button-3>", lambda e: self._set_date_range(None))

        def _bind_hover_switch():
            def _hover_sort(e):
                if (self._filter_open or self._game_open) and not self._sort_open:
                    self._toggle_sort_dropdown()
            def _hover_filter(e):
                if (self._sort_open or self._game_open) and not self._filter_open:
                    self._toggle_filter_dropdown()
            def _hover_game(e):
                if (self._sort_open or self._filter_open) and not self._game_open:
                    self._toggle_game_dropdown()
            self._sort_btn.bind("<Enter>",   _hover_sort,   add="+")
            self._filter_btn.bind("<Enter>", _hover_filter, add="+")
            self._game_btn.bind("<Enter>",   _hover_game,   add="+")
        self.after(100, _bind_hover_switch)

        self._sort_popup = RoundedPopup(inner)
        self._sort_rows = {}
        for opt in ("Newest", "Oldest", "Largest", "Smallest"):
            r = PopupRow(self._sort_popup.inner, opt, lambda v=opt: self._select_sort(v),
                         font=FONT_TINY, height=28, active=(opt == self._sort_var.get()))
            r.pack(fill="x", pady=1)
            self._sort_rows[opt] = r

        self._date_popup = RoundedPopup(inner)

        self._filter_popup = RoundedPopup(inner)
        self._filter_rows = {}
        for opt in ("All clips", "Starred", "Liked", "Trimmed"):
            r = PopupRow(self._filter_popup.inner, opt, lambda v=opt: self._select_filter(v),
                         font=FONT_TINY, height=28, active=(opt == self._filter_var))
            r.pack(fill="x", pady=1)
            self._filter_rows[opt] = r

        self._game_popup = RoundedPopup(inner)
        self._game_popup_sb = tk.Canvas(self._game_popup.inner, bg=CARD_BG, highlightthickness=0, bd=0, width=12, height=1)
        self._game_popup_sb.pack(side="right", fill="y")
        self._game_popup_canvas = tk.Canvas(self._game_popup.inner, bg=CARD_BG, highlightthickness=0, bd=0, width=180)
        self._game_popup_canvas.configure(yscrollcommand=self._game_sb_set)
        self._game_popup_inner = tk.Frame(self._game_popup_canvas, bg=CARD_BG)
        self._game_popup_win = self._game_popup_canvas.create_window((0, 0), window=self._game_popup_inner, anchor="nw")
        self._game_popup_canvas.pack(side="left", fill="both", expand=True)
        self._game_popup_inner.bind("<Configure>", self._game_popup_on_configure)
        self._game_popup_canvas.bind("<MouseWheel>",
            lambda e: self._game_popup_canvas.yview_scroll(int(-1*(e.delta/120)), "units"))
        self._game_popup_inner.bind("<MouseWheel>",
            lambda e: self._game_popup_canvas.yview_scroll(int(-1*(e.delta/120)), "units"))
        self._game_sb_drag_offset = 0.0
        def _sb_press(e):
            self._game_sb_drag_offset = (e.y, getattr(self, '_game_sb_lo', 0.0))
        def _sb_drag(e):
            h, _y0, _L, room, rng = self._game_sb_geom()
            if room <= 0: return
            start_y, start_lo = self._game_sb_drag_offset
            target = start_lo + (e.y - start_y) / room * rng
            self._game_popup_canvas.yview_moveto(max(0.0, min(rng, target)))
        self._game_popup_sb.bind("<Button-1>", _sb_press)
        self._game_popup_sb.bind("<B1-Motion>", _sb_drag)
        self._game_popup_sb.bind("<Configure>", lambda e: self._game_sb_draw())
        self._game_sb_lo = 0.0
        self._game_sb_hi = 1.0

        grid_outer = tk.Frame(inner, bg=SIDEBAR_BG)
        grid_outer.pack(fill="both", expand=True)

        self._vc = tk.Canvas(grid_outer, bg=SIDEBAR_BG, highlightthickness=0, bd=0)
        self._sb = tk.Canvas(grid_outer, width=12, bg=SIDEBAR_BG, highlightthickness=0, bd=0)
        self._sb_thumb_y0 = 0.0
        self._sb_thumb_y1 = 1.0
        self._sb_drag_y   = None

        def _sb_set(lo, hi):
            self._sb_thumb_y0 = float(lo)
            self._sb_thumb_y1 = float(hi)
            _sb_redraw()

        def _sb_geom():
            """Handle position inside the track. The handle has a minimum length, so
            its travel is scaled to the space that is left; that way it ends exactly
            at the bottom of the track when you are at the very bottom of the list."""
            h    = self._sb.winfo_height()
            span = max(0.0, min(1.0, self._sb_thumb_y1 - self._sb_thumb_y0))
            L    = min(max(span * h - 4, 28), max(h - 4, 1))
            room = max(h - 4 - L, 0)
            rng  = 1.0 - span
            y0   = 2 + ((self._sb_thumb_y0 / rng) * room if rng > 1e-6 else 0)
            return h, y0, L, room, rng

        def _sb_redraw(e=None):
            self._sb.delete("all")
            h, _y0, _L, _room, _rng = _sb_geom()
            if h < 2: return
            if _rng <= 1e-6: return                     # everything fits: no handle
            y0 = int(round(_y0))
            y1 = int(round(_y0 + _L))
            r = 4; x0, x1 = 2, 10
            _aa_oval(self._sb, x0, y0, x1, y0+r*2, fill=_c("#3a3a5e"))
            _aa_oval(self._sb, x0, y1-r*2, x1, y1, fill=_c("#3a3a5e"))
            self._sb.create_rectangle(x0, y0+r, x1, y1-r, fill=_c("#3a3a5e"), outline="")

        def _sb_press(e):  self._sb_drag_y = e.y
        def _sb_drag(e):
            if self._sb_drag_y is None: return
            h, _y0, _L, room, rng = _sb_geom()
            delta = ((e.y - self._sb_drag_y) / room * rng) if room > 0 else 0.0
            self._sb_drag_y = e.y
            self._vc.yview_moveto(self._sb_thumb_y0 + delta)
        def _sb_release(e): self._sb_drag_y = None

        self._sb.bind("<Configure>",       _sb_redraw)
        self._sb.bind("<ButtonPress-1>",   _sb_press)
        self._sb.bind("<B1-Motion>",       _sb_drag)
        self._sb.bind("<ButtonRelease-1>", _sb_release)
        self._vc.configure(yscrollcommand=_sb_set)
        self._sb.pack(side="right", fill="y")
        self._vc.pack(fill="both", expand=True)

        # Virtual grid state
        self._vg_display    = []
        self._vg_total      = 0
        self._vg_cols       = 2
        self._vg_card_w     = THUMB_W
        self._vg_card_h     = THUMB_H
        self._vg_pad        = 4
        self._vg_row_h      = THUMB_H + 22 + 8
        self._vg_hover_idx  = None
        self._vg_thumb_wins  = {}
        self._vg_thumb_cvs   = {}
        self._vg_photo_refs  = {}
        self._vg_thumb_sizes = {}
        self._vg_thumb_pos   = {}

        self._vc.bind("<Configure>",       self._vg_on_resize)
        self._vc.bind("<ButtonPress-1>",   self._vg_click)
        self._vc.bind("<ButtonPress-3>",   self._vg_rb_press)
        self._vc.bind("<ButtonRelease-3>", self._vg_rb_release)
        self._vc.bind("<Motion>",          self._vg_motion)
        self._vc.bind("<Leave>",           self._vg_leave)
        self._vc.bind_all("<MouseWheel>",  self._on_scroll)
        self._vc.bind("<Configure>",       self._on_panel_resize, add="+")
        self.bind_all("<ButtonRelease-3>", lambda e: self._rb_drag_reset(), add="+")
        # Plain cover over the whole clip list: shown while the sidebar divider is
        # being dragged, so no clips are visible (or drawn) until it is released.
        self._list_cover = tk.Frame(grid_outer, bg=SIDEBAR_BG, bd=0, highlightthickness=0)
        self._list_cover_on = False

    # ── Controls ──────────────────────────────────────────────────────────────

    def _set_play_icon(self, playing: bool):
        try:
            self.play_btn._playing = playing
            self.play_btn._draw()
        except Exception:
            pass

    def _toggle_play(self):
        if not self._mpv_ipc_ready:
            return
        # Read the actual pause state directly from libmpv rather than the
        # cached _mpv_playing flag — the property observer updates that flag
        # asynchronously, so reading it here can race and double-fire.
        player = getattr(self, "_mpv", None)
        if player is None:
            return
        try:
            currently_paused = bool(player.pause)
        except Exception:
            currently_paused = not self._mpv_playing
        if not currently_paused:
            # Currently playing → pause
            self._at_trim_end = False
            self._mpv_pause()
        else:
            # Currently paused → play
            if self._at_trim_end and self._trim_mode and self._mpv_duration > 0:
                t_start_s = (self.progress._trim_start / 10000.0) * self._mpv_duration
                self._mpv_seek(t_start_s)
            self._at_trim_end = False
            self._mpv_play()

    def _frame_back(self):
        if not self._mpv_ipc_ready:
            return
        if self._mpv_playing:
            self._mpv_pause()
        self._mpv_cmd("frame-back-step")

    def _frame_forward(self):
        if not self._mpv_ipc_ready:
            return
        if self._mpv_playing:
            self._mpv_pause()
        self._mpv_cmd("frame-step")

    # ── Folder auto-refresh ───────────────────────────────────────────────
    _WATCH_EXTS = {".mp4", ".mkv", ".avi", ".mov", ".wmv", ".m4v", ".flv", ".webm"}

    def _folder_watch_tick(self):
        """Every few seconds: look for clips added to / removed from the folders."""
        try:
            if (self._auto_refresh and self._loaded_once and not getattr(self, "_watch_busy", False)
                    and self._checked_folders and self.winfo_viewable()
                    and not getattr(self, "_hidden_to_tray", False)
                    and not self._sash_drag_active()):
                paths = [p for p in self._checked_folders if os.path.isdir(p)]
                if paths:
                    self._watch_busy = True
                    threading.Thread(target=self._watch_scan, args=(paths,), daemon=True).start()
        except Exception:
            self._watch_busy = False
        self.after(3000, self._folder_watch_tick)

    def _watch_scan(self, paths):
        sig = {}
        exts = self._WATCH_EXTS
        def scan(base, depth):
            try:
                with os.scandir(base) as it:
                    for entry in it:
                        try:
                            if entry.is_file(follow_symlinks=False):
                                if os.path.splitext(entry.name)[1].lower() in exts:
                                    sig[entry.path] = entry.stat(follow_symlinks=False).st_size
                            elif depth == 0 and entry.is_dir(follow_symlinks=False):
                                scan(entry.path, 1)
                        except Exception:
                            pass
            except Exception:
                pass
        for base in paths:
            scan(base, 0)
        try:
            self.after(0, lambda: self._watch_apply(sig))
        except Exception:
            self._watch_busy = False

    def _watch_apply(self, sig):
        self._watch_busy = False
        try:
            prev = getattr(self, "_watch_prev", {})
            self._watch_prev = sig
            cur, known = set(sig), set(self.videos)
            if cur == known:
                self._watch_last_trigger = None
                return
            # A clip that is still being written keeps growing: wait until its size is stable
            if any(prev.get(p) != sig[p] or sig[p] == 0 for p in (cur - known)):
                return
            if frozenset(cur) == getattr(self, "_watch_last_trigger", None):
                return                      # already refreshed for exactly this state
            self._watch_last_trigger = frozenset(cur)
            self._load_folders(self._checked_folders, auto=True)
        except Exception:
            pass

    def _on_startup_toggle(self, value):
        ok = _set_startup(bool(value))
        if not ok:      # couldn't write the registry: put the switch back where it was
            try:
                self._startup_toggle.set(_startup_enabled())
            except Exception:
                pass

    def _start_hidden(self):
        """Launched with --minimized (Start with Windows): go straight to the tray."""
        try:
            self._kill_mpv_fast()
        except Exception:
            pass
        self.withdraw()
        self._hidden_to_tray = True
        self._show_tray_icon()

    def _change_see_more(self, direction):
        steps = (25, 50, 100, 200, 500, 1000)
        i = min(range(len(steps)), key=lambda k: abs(steps[k] - self._see_more_step))
        i = max(0, min(len(steps) - 1, i + direction))
        self._see_more_step = steps[i]
        self._save_setting("see_more_step", self._see_more_step)
        self._see_more_lbl.configure(text=str(self._see_more_step))
        # Keep the sidebar button's label in step with the new amount
        try:
            remaining = self._vg_total - len(self._vg_display)
            if remaining > 0:
                self._vc.itemconfig("see_more_text",
                                    text=f"Load more  ({min(self._see_more_step, remaining)} clips)")
        except Exception:
            pass

    def _on_auto_refresh_toggle(self, value):
        self._auto_refresh = bool(value)
        self._save_setting("auto_refresh", self._auto_refresh)
        self._watch_last_trigger = None

    def _refresh_folder(self):
        if self._checked_folders:
            self._load_folders(self._checked_folders)

    def _show_current_file_location(self):
        if self.current_idx is None or not self.videos:
            return
        path = self.videos[self.current_idx]
        try:
            subprocess.Popen(["explorer", "/select,", os.path.normpath(path)])
        except Exception:
            try:
                subprocess.Popen(["explorer", os.path.normpath(self.folder_path)])
            except Exception:
                pass

    def _set_volume(self, val):
        v = int(val)
        self._mpv_set_volume(v)
        self.vol_pct.configure(text=f"{v}%")
        # If slider moved manually away from 0 while muted, clear mute state
        if v > 0 and self._muted:
            self._muted = False
        # If slider moved manually to 0 while not muted, treat as muted
        if v == 0 and not self._muted:
            self._pre_mute_vol = getattr(self, "_pre_mute_vol", None)
            self._muted = True
        if isinstance(self.vol_icon, tk.Label):
            self.vol_icon.configure(text="Vol")
        elif isinstance(self.vol_icon, tk.Canvas):
            img_key = "mute" if v == 0 else "vol"
            img = self._btn_imgs.get(img_key)
            if img:
                self.vol_icon.delete("vol_img")
                self.vol_icon.create_image(15, 15, image=img, anchor="center", tags="vol_img")

    def _toggle_mute(self):
        if self._muted:
            # Unmute — restore previous volume
            restore = self._pre_mute_vol if self._pre_mute_vol else 50
            self._muted = False
            self.vol_slider.set(restore)
            self._set_volume(restore)
        else:
            # Mute — save current volume and go to 0
            self._pre_mute_vol = int(self.vol_slider.get())
            self._muted = True
            self.vol_slider.set(0)
            self._set_volume(0)

    def _update_trim_btn(self):
        """Sync scissors button to current clip trim state."""
        try:
            trimmed = (self.current_idx is not None and
                       self.videos[self.current_idx] in self._trim_enabled)
            self.trim_btn._trimmed = trimmed
            self.trim_btn._draw(trimmed=trimmed)
        except Exception:
            pass

    def _toggle_trim(self):
        if self.current_idx is None: return
        name = self.videos[self.current_idx]
        if name in self._trim_enabled:
            self._trim_enabled.discard(name)
            self._trims.pop(name, None)
            self.trim_btn._trimmed = False
        else:
            self._trim_enabled.add(name)
            self.trim_btn._trimmed = True
        self.trim_btn._draw(trimmed=self.trim_btn._trimmed)
        self._save_prefs()
        self._update_trim_mode_for_current()
        try:
            idx = self._vg_display.index(name)
            self._vg_draw_card(idx, hover=(idx == self._vg_hover_idx))
            self._vg_blit_thumb(name)
        except ValueError: pass

    def _apply_trim(self):
        """Called when user releases a trim handle — save trim position."""
        if self.current_idx is None:
            return
        self._at_trim_end = False
        self._trim_past_ok = False
        start = self.progress._trim_start
        end   = self.progress._trim_end
        name  = self.videos[self.current_idx]
        if start == 0 and end == 10000:
            self._trims.pop(name, None)
        else:
            self._trims[name] = [start, end]
        self._save_prefs()
        # Redraw the card so scissor and trimmed duration update instantly
        try:
            idx = self._vg_display.index(name)
            self._vg_draw_card(idx, hover=(idx == self._vg_hover_idx))
            self._vg_blit_thumb(name)
        except ValueError:
            pass
        # Snap playback to the new trim start so the clip always begins
        # at the start line right after it's trimmed.
        if self._mpv_duration > 0:
            t_start_s = (start / 10000.0) * self._mpv_duration
            self._mpv_seek(t_start_s)

    def _apply_trim_cut(self):
        """Bake the trim into the file: cut away everything outside the trim
        handles, replace the clip in place (same path + dates), clear its trim."""
        if self.current_idx is None or not self.videos:
            return
        if getattr(self, "_trim_cut_busy", False):
            return
        path = self.videos[self.current_idx]
        rng = self._trims.get(path)
        if path not in self._trim_enabled or not rng or (rng[0] <= 0 and rng[1] >= 10000):
            self._flash_star_status("Nothing to trim - move a trim handle first", _c("#c8c864"))
            return
        if not self._ffmpeg:
            self._flash_star_status("ffmpeg not found", _c("#e05555"))
            return
        res = self._dark_dialog(
                "Apply trim", "Cut this clip down to the trimmed part?",
                yes_text="Apply trim", details=[os.path.basename(path)],
                note="Apply trim deletes the parts outside the trim from the file "
                     "and can't be undone. Save as copy keeps the original clip "
                     "and makes a new trimmed clip next to it.",
                extra2_text="Save as copy", extra2_value="copy", extra2_style="neutral")
        if not res:
            return
        as_copy = (res == "copy")

        self._trim_cut_busy = True
        ff = self._ffmpeg
        self._flash_star_status("Saving trimmed copy…" if as_copy else "Cutting clip…",
                                _c("#c8c864"))

        def _work():
            tmp = None
            try:
                dur = get_video_duration(path, ff)
                t0 = (rng[0] / 10000.0) * dur
                t1 = (rng[1] / 10000.0) * dur
                if dur <= 0 or t1 <= t0 + 0.05:
                    raise RuntimeError("bad trim range")
                ext = os.path.splitext(path)[1].lower() or ".mp4"
                fmt = _TRIM_FORMATS.get(ext, "matroska")
                # .trimtmp so the folder watcher doesn't treat it as a new clip
                tmp = path + ".trimtmp"
                dest_path = path
                if as_copy:
                    _stem, _ext = os.path.splitext(path)
                    dest_path = f"{_stem} (trimmed){_ext}"
                    _k = 1
                    while os.path.exists(dest_path):
                        _k += 1
                        dest_path = f"{_stem} (trimmed {_k}){_ext}"
                args = [ff, "-y", "-ss", f"{t0:.3f}", "-i", path, "-t", f"{t1 - t0:.3f}",
                        "-map", "0:v:0", "-map", "0:a?", "-map_metadata", "0"]
                cpu_v = ["-c:v", "libx264", "-preset", "veryfast", "-crf", "18"]
                gpu_v = ["-c:v", "h264_nvenc", "-preset", "medium", "-rc", "vbr",
                         "-cq", "19", "-b:v", "0"]
                tail = []
                # Never let the cut come out bigger than the source: cap the
                # video bitrate at the original's (the quality setting still
                # picks the bitrate when the clip is easy to compress).
                src_kbps = get_video_bitrate_kbps(path, ff)
                if src_kbps > 0:
                    cap = max(500, src_kbps)
                    tail += ["-maxrate", f"{cap}k", "-bufsize", f"{cap * 2}k"]
                tail += ["-c:a", "copy"]
                if fmt in ("mp4", "mov"):
                    tail += ["-movflags", "+faststart"]
                tail += ["-f", fmt, tmp]

                def _run(v_args):
                    proc = subprocess.run(args + v_args + tail, stdout=subprocess.DEVNULL,
                                          stderr=subprocess.DEVNULL, creationflags=0x08000000)
                    return (proc.returncode == 0 and os.path.exists(tmp)
                            and os.path.getsize(tmp) > 0)

                ok = False
                # GPU encoder first (much faster); if it isn't available or can't
                # handle this clip, quietly fall back to the CPU encode below.
                if self._trim_gpu and getattr(self, "_nvenc_ok", True):
                    ok = _run(gpu_v)
                    if ok:
                        self._nvenc_ok = True
                    elif os.path.exists(tmp):
                        try: os.remove(tmp)
                        except Exception: pass
                if not ok:
                    ok = _run(cpu_v)
                if not ok:
                    raise RuntimeError("ffmpeg failed")

                _copy_file_times(path, tmp)

                if as_copy:
                    # Original stays as it is (mpv keeps playing it): just
                    # move the finished file to its new name.
                    os.replace(tmp, dest_path)
                    tmp = None
                    self._post_main(lambda: self._finish_trim_copy(dest_path, True, ""))
                    return

                # Old thumbnail cache name depends on size/date -- work it out now
                try:
                    old_thumb = os.path.join(CACHE_DIR, cache_key(path))
                except Exception:
                    old_thumb = None

                # mpv holds the file open; release it before swapping
                self._post_main(lambda: self._mpv_cmd("stop"))
                time.sleep(0.6)
                last_err = None
                for _ in range(10):
                    try:
                        os.replace(tmp, path)
                        last_err = None
                        break
                    except Exception as e:
                        last_err = e
                        time.sleep(0.3)
                if last_err:
                    raise last_err
                tmp = None
                if old_thumb:
                    try: os.remove(old_thumb)
                    except Exception: pass
                self._post_main(lambda: self._finish_trim_cut(path, True, ""))
            except Exception as e:
                _debug_log(f"apply trim failed: {e}")
                if tmp and os.path.exists(tmp):
                    try: os.remove(tmp)
                    except Exception: pass
                if as_copy:
                    self._post_main(lambda m=str(e): self._finish_trim_copy(None, False, m))
                else:
                    self._post_main(lambda m=str(e): self._finish_trim_cut(path, False, m))

        threading.Thread(target=_work, daemon=True).start()

    def _finish_trim_copy(self, new_path, ok, msg):
        self._trim_cut_busy = False
        if ok:
            self._mark_start_zero(new_path)
            self._flash_star_status("Saved copy: " + os.path.basename(new_path), _c("#7ec97e"))
            self._load_folders(self._checked_folders, auto=True)
        else:
            self._flash_star_status(f"Copy failed: {msg}"[:60], _c("#e05555"))

    def _finish_trim_cut(self, path, ok, msg):
        self._trim_cut_busy = False
        if ok:
            # The trim is now baked in: the whole clip is the trim
            self._mark_start_zero(path)
            self._trims.pop(path, None)
            self._trim_enabled.discard(path)
            self._durations.pop(path, None)
            self._file_stat_cache.pop(path, None)
            self._vg_photo_refs.pop(path, None)
            self._save_prefs()
            self._flash_star_status("Trim applied", _c("#7ec97e"))
        else:
            self._flash_star_status(f"Trim failed: {msg}"[:60], _c("#e05555"))
        # Reload the (possibly new) file in the player and refresh the list
        if self.current_idx is not None and self.current_idx < len(self.videos) \
                and self.videos[self.current_idx] == path:
            self._update_trim_mode_for_current()
            self._mpv_load(path)
            try:
                mb = os.path.getsize(path) / (1024 * 1024)
                self.now_playing_mb.configure(text=f"{mb:.1f} MB")
            except Exception:
                pass
        self._load_folders(self._checked_folders, auto=True)

    def _seek_start(self):
        self._seeking = True
        self._seek_last_val = -1
        self._at_trim_end = False
        # Pause while the playhead is being dragged; on release it resumes
        # (unless it was dropped on a trim line).
        self._seek_was_playing = bool(self._mpv_playing)
        if self._seek_was_playing:
            self._mpv_pause()
            self._set_play_icon(False)

    def _on_seek_drag(self, val):
        if self._seeking and self._mpv_duration > 0:
            ival = int(val)
            if abs(ival - self._seek_last_val) >= 2:
                self._seek_last_val = ival
                secs = (ival / 10000.0) * self._mpv_duration
                self._mpv_seek(secs)
                self.time_label.configure(text=self._fmt(secs))

    def _seek_end(self, val):
        self._seeking = False
        self._at_trim_end = False
        snapped = False
        if self._mpv_duration > 0:
            secs = (val / 10000.0) * self._mpv_duration
            # Placed manually on (or past) the trim end -> allow playing from there.
            # (Normal playback reaching the end line still stops.)
            t_end_s = (self.progress._trim_end / 10000.0) * self._mpv_duration
            self._trim_past_ok = bool(self._trim_mode and secs >= t_end_s - 0.05)
            self._trim_seek_t  = time.monotonic()
            self._mpv_seek(secs)
            # Dropped exactly on a trim line (snapped): stay paused there;
            # pressing play then starts from that spot.
            if self._trim_mode and (abs(val - self.progress._trim_start) < 0.5
                                    or abs(val - self.progress._trim_end) < 0.5):
                snapped = True
                self._mpv_pause()
                self._set_play_icon(False)
        was_playing = getattr(self, "_seek_was_playing", False)
        self._seek_was_playing = False
        if was_playing and not snapped:
            self._mpv_play()
            self._set_play_icon(True)

    def _fmt(self, secs):
        if secs < 0: secs = 0
        s = int(secs)
        return f"{s//60}:{s%60:02d}"

    def _start_progress_loop(self):
        self._last_icon_state = None
        self._last_time_str   = None
        self._last_dur_str    = None

        def _tick():
            try:
                playing = self._mpv_playing
                pos     = self._mpv_pos
                dur     = self._mpv_duration

                # Update play icon
                if playing != self._last_icon_state:
                    self._last_icon_state = playing
                    self._set_play_icon(playing)

                if not self._seeking and dur > 0:
                    t_end_frac   = self.progress._trim_end   / 10000.0
                    t_end_s      = t_end_frac   * dur
                    if (self._trim_mode and not self._trim_past_ok and pos > t_end_s
                            and (self._at_trim_end or playing)):
                        pos = t_end_s                # never draw past the trim line
                    frac         = pos / dur

                    # Always keep progress bar synced to MPV position
                    self.progress.set(frac * 10000, redraw=True)

                    # Once the position is back before the trim end (e.g. the
                    # user skipped back), normal enforcement resumes.
                    if (self._trim_past_ok and pos < t_end_s - 0.15
                            and time.monotonic() - self._trim_seek_t > 0.7):
                        self._trim_past_ok = False

                    # Trim enforcement: auto-pause when playback *reaches* the
                    # trim end. Skipping past it by hand is allowed.
                    if (playing and self._trim_mode and not self._at_trim_end
                            and not self._trim_past_ok
                            and time.monotonic() - self._loop_seek_t > 0.3):
                        if pos >= t_end_s - 0.03:
                            if self._loop_clip:
                                # Loop: jump back to the trim start and keep playing
                                t_start_s = (self.progress._trim_start / 10000.0) * dur
                                self._loop_seek_t = time.monotonic()
                                self._mpv_seek(t_start_s)
                                self._mpv_pos = t_start_s
                            else:
                                self._at_trim_end = True
                                self._mpv_seek(t_end_s)      # land exactly on the trim line
                                self._mpv_pos = t_end_s
                                self._mpv_pause()
                                self._set_play_icon(False)

                    # Loop an untrimmed clip: when playback reaches the end
                    # (mpv holds the last frame), go back to 0 and play on.
                    if (self._loop_clip and not self._trim_mode
                            and pos >= dur - 0.15
                            and (playing or self._loop_prev_playing)
                            and time.monotonic() - self._loop_seek_t > 0.5):
                        self._loop_seek_t = time.monotonic()
                        self._mpv_seek(0)
                        self._mpv_pos = 0.0
                        self._mpv_play()

                self._loop_prev_playing = playing

                # Time label
                t_str = self._fmt(pos)
                if t_str != self._last_time_str:
                    self._last_time_str = t_str
                    self.time_label.configure(text=t_str)

                # Duration label
                d_str = self._fmt(dur)
                if d_str != self._last_dur_str:
                    self._last_dur_str = d_str
                    self.dur_label.configure(text=d_str)
            except Exception:
                pass
            self.after(15, _tick)

        self.after(15, _tick)

    # ── Playback ──────────────────────────────────────────────────────────────

    def _play_index(self, index):
        if not self.videos:
            return
        # Do nothing if this clip is already playing
        if index == self.current_idx and self._mpv_playing:
            return
        self.current_idx = index
        path = self.videos[index]
        name = os.path.basename(path)

        self._mpv_duration = 0.0
        self._mpv_pos      = 0.0
        self._at_trim_end = False
        self._trim_past_ok = False

        # speed belongs to the clip: its own saved speed, otherwise 1x
        self._play_speed = self._clip_speed.get(path, 1.0)
        try:
            self._apply_speed(self._mpv)
        except Exception:
            pass
        try:
            self.speed_btn._draw()
        except Exception:
            pass

        # Show/hide trim handles based on whether this clip has trim enabled
        self._update_trim_mode_for_current()
        self._mpv_load(path)

        # Decide where to land once duration is known. This check is done
        # live, right when the seek actually fires (not back when this
        # function was first called) — if you trim the clip while it's
        # still loading, this will correctly seek to the trim start
        # instead of clobbering it with a stale "seek to midpoint" seek.
        def _seek_after_load(p=path):
            for _ in range(60):
                time.sleep(0.05)
                # Bail out if the user switched to a different clip while
                # this was waiting on duration — don't seek the wrong clip.
                if self.current_idx is None or self.videos[self.current_idx] != p:
                    return
                dur = self._mpv_duration
                if dur > 0:
                    if p in self._trim_enabled:
                        s = self._trims.get(p, [0, 10000])
                        t_start_s = (s[0] / 10000.0) * dur
                        if t_start_s > 0:
                            self._post_main(lambda t=t_start_s: self._mpv_seek(t))
                    elif dur > 15 and not self._starts_at_beginning(p):
                        self._post_main(lambda t=dur * 0.5: self._mpv_seek(t))
                    return
        threading.Thread(target=_seek_after_load, daemon=True).start()

        # Ensure volume is applied
        self._mpv_set_volume(int(self.vol_slider.get()))

        self.no_video_label.place_forget()
        self.now_playing.configure(text=self._format_now_playing(name, path))
        try:
            mb = os.path.getsize(path) / (1024 * 1024)
            self.now_playing_mb.configure(text=f"{mb:.1f} MB")
        except Exception:
            self.now_playing_mb.configure(text="")

        self._highlight_active_card()
        self._update_star_btn()
        self._update_heart_btn()
        self._update_trim_btn()

        # Update play icon after short delay (MPV needs a moment to start)
        self.after(200, lambda: self._set_play_icon(True))

    def _nav_clip(self, direction):
        if not self.videos:
            return
        if self._filter_var == "Starred":
            nav_list = [v for v in self.videos if v in self._starred_clips]
        elif self._filter_var == "Liked":
            nav_list = [v for v in self.videos if v in self._liked_clips]
        elif self._filter_var == "Trimmed":
            nav_list = [v for v in self.videos if v in self._trim_enabled]
        else:
            nav_list = list(self.videos)
        nav_list = self._apply_date_filter(self._game_filter(nav_list))
        if not nav_list:
            return
        if self.current_idx is None:
            self._play_index(self.videos.index(nav_list[0]))
            return
        current_name = self.videos[self.current_idx]
        if current_name not in nav_list:
            self._play_index(self.videos.index(nav_list[0]))
            return
        nav_idx = nav_list.index(current_name)
        nav_idx += -1 if direction == "newer" else 1
        if 0 <= nav_idx < len(nav_list):
            self._play_index(self.videos.index(nav_list[nav_idx]))

    def _format_now_playing(self, name, path):
        key = self._category_key_for_video(path)
        game_label = self._game_norm_map.get(key) if key else None
        if not game_label:
            game_label = game_name_from_filename(name) or os.path.splitext(os.path.basename(name))[0]
        dt_fn = date_from_filename(name)
        if dt_fn:
            ts = dt_fn.strftime("%d.%m.%Y %H:%M")
        else:
            try:
                ts = datetime.datetime.fromtimestamp(os.path.getmtime(path)).strftime("%d.%m.%Y %H:%M")
            except Exception:
                ts = ""
        return f"{game_label}  {ts}" if ts else game_label

    def _delete_targets(self):
        """Clips the trash button acts on: the right-click selection if there
        is one, otherwise just the clip that is currently open."""
        if self._selected_clips:
            return sorted(self._selected_clips)
        if self.current_idx is not None and 0 <= self.current_idx < len(self.videos):
            return [self.videos[self.current_idx]]
        return []

    def _delete_selected(self):
        names = self._delete_targets()
        if not names:
            return
        use_bin = self._use_recycle_bin
        n = len(names)
        noun = "clip" if n == 1 else "clips"
        shown = [os.path.basename(x) for x in names[:8]]
        more = max(0, n - 8)
        _total = 0
        for _x in names:
            try:
                _total += os.path.getsize(_x)
            except OSError:
                pass
        size_txt = (f"{_total / 2**30:.2f} GB" if _total >= 2**30
                    else f"{_total / 2**20:.1f} MB")
        if use_bin:
            n_perm = sum(1 for x in names if not _can_recycle(x))
            head = f"Move {n} {noun} ({size_txt}) to the Recycle Bin?"
            if n_perm:
                note = (f"{n_perm} of them {'is' if n_perm == 1 else 'are'} on a drive without a "
                        f"Recycle Bin and will be deleted permanently.")
            else:
                note = "You can restore them from the Recycle Bin."
        else:
            head = f"Permanently delete {n} {noun} ({size_txt})?"
            note = "This can't be undone."
        cloud_map = {x: self._cloud_copy_of(x) for x in names}
        n_cloud = sum(1 for v in cloud_map.values() if v)
        if n_cloud:
            if n_cloud == n:
                extra_note = ("It also has" if n == 1 else "They also have") + " a copy in your cloud folder."
            else:
                extra_note = f"{n_cloud} of them also {'has' if n_cloud == 1 else 'have'} a copy in your cloud folder."
            note += "\n" + extra_note
        res = self._dark_dialog("Delete clips", head, details=shown, more=more, note=note,
                                extra_text="Delete + from cloud" if n_cloud else None,
                                extra2_text="Only from cloud" if n_cloud else None,
                                extra2_value="cloud_only")
        if not res:
            return
        if res == "cloud_only":
            self._delete_cloud_copies_only(names, cloud_map)
            return
        with_cloud = (res == "cloud")
        cloud_changed = False
        errors = []
        # mpv keeps the open clip locked: let go of it before deleting
        try:
            cur = (self.videos[self.current_idx]
                   if self.current_idx is not None and self.current_idx < len(self.videos)
                   else None)
            if cur in names:
                self._mpv_cmd("stop")
                time.sleep(0.4)
        except Exception:
            pass
        for path in names:
            # The cache name is derived from the file's size/date, so work it
            # out before the file disappears.
            try:
                thumb = os.path.join(CACHE_DIR, cache_key(path))
            except Exception:
                thumb = None
            try:
                if use_bin and _can_recycle(path):
                    if not _recycle_file(path):
                        raise OSError("could not move it to the Recycle Bin")
                else:
                    os.remove(path)
            except Exception as exc:
                errors.append(f"{os.path.basename(path)}: {exc}")
                continue
            if thumb:
                try:
                    os.remove(thumb)
                except Exception:
                    pass
            if with_cloud and cloud_map.get(path):          # the local delete worked: remove the cloud copy too
                try:
                    os.remove(cloud_map[path])
                    self._cloud_copied.pop(path, None)
                    cloud_changed = True
                except Exception as exc:
                    errors.append(f"{os.path.basename(path)} (cloud copy): {exc}")
        if cloud_changed:
            self._cloud_save_copied()
        self._selected_clips.clear()
        self._update_delete_size_lbl()
        if errors:
            self._dark_dialog("Some deletions failed", "Some clips couldn't be deleted.",
                              kind="error", details=errors[:8], more=max(0, len(errors) - 8))
        self._load_folders(self._checked_folders)

    def _delete_cloud_copies_only(self, names, cloud_map):
        """Remove only the cloud copies; the local clips stay untouched. The
        record is kept (marked as removed on purpose) so sync doesn't just
        upload the clip again."""
        errors, done = [], 0
        for path in names:
            target = cloud_map.get(path)
            if not target:
                continue
            try:
                os.remove(target)
                rec = self._cloud_copied.get(path)
                if rec and len(rec) >= 3:
                    self._cloud_copied[path] = [rec[0], rec[1], rec[2], True]
                done += 1
            except Exception as exc:
                errors.append(f"{os.path.basename(path)} (cloud copy): {exc}")
        if done:
            self._cloud_save_copied()
        self._selected_clips.clear()
        self._update_delete_size_lbl()
        if errors:
            self._dark_dialog("Some deletions failed", "Some cloud copies couldn't be deleted.",
                              kind="error", details=errors[:8], more=max(0, len(errors) - 8))
        else:
            self._flash_star_status(
                "Removed %d cloud cop%s" % (done, "y" if done == 1 else "ies"), _c("#7ec97e"))
        self._load_folders(self._checked_folders)

    # ── Folder loading ────────────────────────────────────────────────────────

    def _pick_folder(self):
        path = filedialog.askdirectory(title="Select your video folder")
        if path:
            self._visible_count = 50
            if path not in self._saved_folders:
                self._saved_folders.append(path)
            if path not in self._checked_folders:
                self._checked_folders = [path]
            else:
                self._checked_folders = [path]
            self._active_folder = path
            self._save_prefs()
            self._load_folders(self._checked_folders)

    def _toggle_folder_dropdown(self):
        if self._folder_dropdown_open:
            self._close_folder_dropdown()
        else:
            self._open_folder_dropdown()

    def _open_folder_dropdown(self):
        self._close_folder_dropdown()
        popup = RoundedPopup(self)
        self._folder_dropdown = popup
        box = popup.inner

        tk.Label(box, text="Saved folders", font=FONT_TINY, bg=CARD_BG,
                 fg=TEXT_DIM, anchor="w").pack(fill="x", padx=12, pady=(6, 3))

        def _toggle(p, row):
            if p in self._checked_folders:
                if len(self._checked_folders) > 1:
                    self._checked_folders.remove(p)
                    # After unchecking, make the new first folder active
                    self._active_folder = self._checked_folders[0]
                # Don't allow unchecking the last folder
            else:
                self._checked_folders.append(p)
                self._active_folder = p
                # Move newly checked folder to front
                self._checked_folders = [p] + [f for f in self._checked_folders if f != p]
            row.set_checked(p in self._checked_folders)
            self._save_prefs()
            self._load_folders(self._checked_folders)

        if not self._saved_folders:
            tk.Label(box, text="No saved folders yet", font=FONT_UI, bg=CARD_BG,
                     fg=TEXT_MUTED, anchor="w").pack(fill="x", padx=12, pady=(2, 8))
        for fp in self._saved_folders:
            display = os.path.basename(fp) or fp
            row = PopupRow(box, display, None, font=FONT_UI, height=44,
                           check=(fp in self._checked_folders), subtitle=fp)
            row._command = lambda p=fp, r=row: _toggle(p, r)
            row.pack(fill="x", pady=1)

        tk.Frame(box, bg=DIVIDER, height=1).pack(fill="x", padx=8, pady=(5, 4))

        def _add_new():
            self._close_folder_dropdown()
            path = filedialog.askdirectory(title="Select your video folder")
            if path:
                if path not in self._saved_folders:
                    self._saved_folders.append(path)
                if path not in self._checked_folders:
                    self._checked_folders.append(path)
                self._active_folder = path
                self._save_prefs()
                self._load_folders(self._checked_folders)
        PopupRow(box, "Add new folder", _add_new, font=FONT_UI, height=34,
                 icon="folder-plus").pack(fill="x", pady=(0, 1))

        self._folder_arrow_btn.update_idletasks()
        ax = self._folder_arrow_btn.winfo_rootx() - self.winfo_rootx()
        ay = (self._folder_arrow_btn.winfo_rooty() - self.winfo_rooty()
              + self._folder_arrow_btn.winfo_height() + 4)
        popup.place(x=ax + self._folder_arrow_btn.winfo_width() + 2, y=ay,
                    anchor="ne", width=300)
        popup.lift()
        self._folder_dropdown_open = True
        self.bind("<Button-1>", self._close_folder_dropdown_if_outside, add="+")

    def _close_folder_dropdown(self, event=None):
        if self._folder_dropdown:
            self._folder_dropdown.destroy()
            self._folder_dropdown = None
        self._folder_dropdown_open = False

    def _close_folder_dropdown_if_outside(self, event):
        if not self._folder_dropdown:
            return
        w = event.widget
        while w is not None:
            if w is self._folder_dropdown or w is self._folder_arrow_btn:
                return
            try: w = w.master
            except Exception: break
        self._close_folder_dropdown()

    def _load_folders(self, paths, auto=False):
        """Load clips from one or more folders. videos stored as absolute paths.

        The directory scan (and any new-file thumbnail generation) can be
        slow, especially on network drives, so the scan itself runs in a
        background thread and hands off to _finish_load_folders on the main
        thread once done. Files that weren't in the previous video list are
        treated as "new" and get their thumbnails loaded first, so newly
        added clips show up fast while everything else fills in afterward.
        """
        paths = [p for p in paths if os.path.isdir(p)]
        if not paths:
            return
        # Keep folder_path pointing at the active folder (not just index 0)
        active = self._active_folder if self._active_folder in paths else paths[0]
        self.folder_path = active
        # Reorder so active is first — ensures consistency everywhere
        paths = [active] + [p for p in paths if p != active]

        previous_known = set(self.videos)
        self._scan_thread_id += 1
        my_scan_id = self._scan_thread_id

        def _do_scan():
            exts = {".mp4", ".mkv", ".avi", ".mov", ".wmv", ".m4v", ".flv", ".webm"}
            files = []  # list of (abs_path, mtime)

            def _scan_dir(base):
                try:
                    with os.scandir(base) as it:
                        for entry in it:
                            try:
                                if (entry.is_file(follow_symlinks=False) and
                                        os.path.splitext(entry.name)[1].lower() in exts):
                                    dt_fn = date_from_filename(entry.name)
                                    mtime = dt_fn.timestamp() if dt_fn else entry.stat(follow_symlinks=False).st_mtime
                                    files.append((entry.path, mtime))
                                elif entry.is_dir(follow_symlinks=False):
                                    sub_files = []
                                    try:
                                        with os.scandir(entry.path) as sub_it:
                                            for sub_entry in sub_it:
                                                try:
                                                    if (sub_entry.is_file(follow_symlinks=False) and
                                                            os.path.splitext(sub_entry.name)[1].lower() in exts):
                                                        sub_files.append(sub_entry)
                                                except Exception:
                                                    pass
                                    except Exception:
                                        pass
                                    for sub_entry in sub_files:
                                        dt_fn = date_from_filename(sub_entry.name)
                                        mtime = dt_fn.timestamp() if dt_fn else sub_entry.stat(follow_symlinks=False).st_mtime
                                        files.append((sub_entry.path, mtime))
                            except Exception:
                                pass
                except Exception:
                    pass

            for p in paths:
                _scan_dir(p)

            files.sort(key=lambda x: x[1], reverse=True)
            if my_scan_id != self._scan_thread_id:
                return  # a newer refresh started while we were scanning
            self.after(0, lambda: self._finish_load_folders(files, previous_known, my_scan_id, auto))

        threading.Thread(target=_do_scan, daemon=True).start()

    def _finish_load_folders(self, files, previous_known, scan_id, auto=False):
        """Runs on the main thread once the background scan finishes.
        auto=True is the silent background refresh: it keeps the playing clip,
        the selection and the scroll position."""
        if scan_id != self._scan_thread_id:
            return  # superseded by a newer refresh

        self._loaded_once = True
        prev_cur = None
        if auto and self.current_idx is not None and 0 <= self.current_idx < len(self.videos):
            prev_cur = self.videos[self.current_idx]
        try:
            prev_scroll_px = self._vc.canvasy(0) if auto else 0
        except Exception:
            prev_scroll_px = 0
        self.videos = [f[0] for f in files]  # full absolute paths
        new_paths = [p for p in self.videos if p not in previous_known]

        # Pre-cache file stats (keyed by full path). Reuse anything already
        # cached (no disk I/O needed) and only stat/size genuinely new files —
        # this is what makes repeat refreshes fast.
        current_set = set(self.videos)
        for stale in [k for k in self._file_stat_cache if k not in current_set]:
            del self._file_stat_cache[stale]

        for fpath, _ in files:
            cached_entry = self._file_stat_cache.get(fpath)
            if cached_entry is not None:
                date_str, mb_str, _prev_ago, clip_dt = cached_entry
            else:
                fname = os.path.basename(fpath)
                clip_dt = None
                try:
                    dt_fn = date_from_filename(fname)
                    clip_dt = dt_fn if dt_fn else datetime.datetime.fromtimestamp(os.path.getmtime(fpath))
                    date_str = clip_dt.strftime("%d-%m-%Y  %H:%M")
                except Exception:
                    date_str = ""
                try:
                    mb_str = f"{os.path.getsize(fpath)/(1024*1024):.1f} MB"
                except Exception:
                    mb_str = ""
            try:
                if clip_dt is not None:
                    delta = datetime.datetime.now() - clip_dt
                    secs = delta.total_seconds()
                    if secs < 60:
                        ago_str = "Just now"
                    elif secs < 3600:
                        mins = int(secs // 60)
                        ago_str = f"{mins} minute{'s' if mins != 1 else ''} ago"
                    elif delta.days <= 0:
                        hrs = int(secs // 3600)
                        ago_str = f"{hrs} hour{'s' if hrs != 1 else ''} ago"
                    elif delta.days == 1:
                        ago_str = "1 day ago"
                    else:
                        ago_str = f"{delta.days} days ago"
                else:
                    ago_str = ""
            except Exception:
                ago_str = ""
            self._file_stat_cache[fpath] = (date_str, mb_str, ago_str, clip_dt)

        self._recompute_categories()
        # Validate restored game filter — reset to "All" if that category no longer exists.
        # Also handle prefs saved by an older version of the app, which stored the
        # display text instead of the stable key.
        if self._game_var != "All" and self._game_var not in self._game_norm_map:
            legacy_match = next((k for k, v in self._game_norm_map.items()
                                 if v.lower() == str(self._game_var).lower()), None)
            self._game_var = legacy_match or "All"
        self._game_multi &= set(self._game_norm_map)
        if self._game_multi:
            self._game_var = "All"
        self._rebuild_game_popup()
        self._update_game_btn_text()

        # Drop cached thumbnail images for clips that no longer exist, but
        # keep everything else so already-loaded thumbnails stay on screen
        # instantly instead of flashing to placeholders on every refresh.
        for stale in [k for k in self.thumbnails if k not in current_set]:
            self.thumbnails.pop(stale, None)
        if auto:
            self._selected_clips &= current_set
        else:
            self.current_idx = None
            self._selected_clips.clear()
            self.now_playing.configure(text="")
        self._set_stat(self.clip_count, f"{len(self.videos)}" if self.videos else "0")
        if not auto:
            self._set_stat(self.folder_size_lbl, "")
        self._update_delete_size_lbl()
        self._on_sort_change(self._sort_var.get(), reset_visible=False)
        if auto:
            # the sort re-orders the list: find the playing clip again, restore the scroll
            self.current_idx = self.videos.index(prev_cur) if prev_cur in self.videos else None
            try:
                self._vg_refresh()
                _sr = str(self._vc.cget("scrollregion")).split()
                total = float(_sr[3]) if len(_sr) >= 4 else 0
                if prev_scroll_px > 0 and total > 0:
                    self._vc.yview_moveto(min(prev_scroll_px / total, 1.0))
            except Exception:
                pass
        threading.Thread(target=self._load_thumbnails, kwargs={"priority": new_paths}, daemon=True).start()
        self._save_prefs()

        def _calc_size():
            try:
                total = sum(os.path.getsize(fp) for fp, _ in files)
                gb = total / (1024**3)
                s  = f"{gb:.2f} GB" if gb >= 1 else f"{total/(1024**2):.1f} MB"
                self.after(0, lambda: self._set_stat(self.folder_size_lbl, s if files else ""))
            except Exception:
                pass
        threading.Thread(target=_calc_size, daemon=True).start()

    def _get_filtered_visible(self):
        _display = self.videos
        if self._filter_var == "Starred":
            _display = [v for v in _display if v in self._starred_clips]
        elif self._filter_var == "Liked":
            _display = [v for v in _display if v in self._liked_clips]
        elif self._filter_var == "Trimmed":
            _display = [v for v in _display if v in self._trim_enabled]
        _display = self._apply_date_filter(self._game_filter(_display))
        return _display[:self._visible_count]

    def _load_thumbnails(self, priority=None):
        """Loads durations/thumbnails for currently visible clips.

        If `priority` is given (paths that are new since the last scan),
        those are fetched first with full concurrency so newly added clips
        appear almost immediately. The remaining, already-seen clips (whose
        thumbnails are normally just a fast cache-hit anyway) are then
        loaded afterward at a gentler pace so they don't compete for
        CPU/disk/network with whatever the user is actively watching.
        When `priority` is None (sort/filter/show-more, not a refresh),
        everything is fetched at full speed as before.
        """
        self._thumb_thread_id += 1
        my_id = self._thumb_thread_id
        visible = self._get_filtered_visible()
        two_phase = priority is not None
        priority_set = set(priority or [])
        priority_paths = [v for v in visible if v in priority_set] if two_phase else visible
        rest_paths     = [v for v in visible if v not in priority_set] if two_phase else []

        def _fetch_one(vpath):
            if my_id != self._thumb_thread_id:
                return None, None, None
            dur_key = vpath
            dur_str = None
            if dur_key not in self._durations:
                secs = get_video_duration(vpath, self._ffmpeg)
                if secs > 0:
                    s = int(secs)
                    dur_str = (f"{s//3600}:{(s%3600)//60:02d}:{s%60:02d}"
                               if s >= 3600 else f"{s//60}:{s%60:02d}")
            thumb = get_thumb(vpath, self._ffmpeg)
            return vpath, dur_str, thumb

        def _run_batch(paths, workers, delay=0.0):
            if not paths:
                return
            with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
                futures = {pool.submit(_fetch_one, n): n for n in paths}
                for fut in concurrent.futures.as_completed(futures):
                    if my_id != self._thumb_thread_id:
                        return
                    try:
                        vpath, dur_str, thumb = fut.result()
                    except Exception:
                        continue
                    if vpath is None:
                        continue
                    if dur_str and vpath not in self._durations:
                        self._durations[vpath] = dur_str
                        self.after(0, self._save_prefs)
                        self.after(0, lambda n=vpath: self._vg_blit_thumb(n))
                    if thumb:
                        self.after(0, lambda n=vpath, p=thumb: self._apply_thumb(n, p))
                    if delay:
                        time.sleep(delay)

        # Phase 1: new clips, fetched as fast as possible.
        _run_batch(priority_paths, workers=4)
        if my_id != self._thumb_thread_id:
            return
        # Phase 2: everything else. Only throttle it if phase 1 actually had
        # something to prioritize — otherwise there's nothing to protect and
        # it should just run at full speed like before.
        if priority_paths:
            _run_batch(rest_paths, workers=2, delay=0.03)
        else:
            _run_batch(rest_paths, workers=4)

    def _thumb_path(self, vpath):
        p = os.path.join(CACHE_DIR, cache_key(vpath))
        return p if os.path.exists(p) else None

    def _on_panel_resize(self, event):
        if not self._ui_ready:
            return
        self._panel_width = event.width
        if self._sash_drag_active():
            return                       # re-laid out once on release
        if event.width == getattr(self, "_vg_canvas_w", None):
            return                       # only the height changed: nothing to re-layout
        if self._resize_job:
            self.after_cancel(self._resize_job)
        self._resize_job = self.after(150, self._vg_on_resize)

    def _load_all(self):
        total = getattr(self, "_vg_total", 0)
        if total > 1500:
            from tkinter import messagebox
            if not messagebox.askyesno(
                    "Load all clips",
                    f"Load all {total} clips at once?\n\n"
                    "The app may freeze for a while while the list and "
                    "thumbnails load."):
                return
        self._ignore_next_click = True
        self._visible_count = max(self._visible_count, total)
        self._render_grid()
        threading.Thread(target=self._load_thumbnails, daemon=True).start()

    def _show_more(self):
        self._visible_count += self._see_more_step
        self._ignore_next_click = True
        self._render_grid()
        threading.Thread(target=self._load_thumbnails, daemon=True).start()

    # ── Dropdown logic ────────────────────────────────────────────────────────

    def _place_popup_settled(self, popup, x, y, width):
        """Place a dropdown off-screen first so its size settles (the popup
        resizes itself over several <Configure> passes), then move it into
        view. Without this the first open visibly warps for a moment."""
        popup.place(x=-10000, y=0, width=width)
        popup.update_idletasks()
        popup.place(x=x, y=y, width=width)

    def _close_all_dropdowns(self):
        self._sort_popup.place_forget();   self._sort_open   = False
        self._filter_popup.place_forget(); self._filter_open = False
        if getattr(self, "_date_popup", None) is not None:
            self._date_popup.place_forget(); self._date_open = False
        if self._game_open:
            self._game_scroll_pos = self._game_popup_canvas.yview()[0]
        self._game_popup.place_forget();   self._game_open   = False

    def _toggle_sort_dropdown(self):
        if self._sort_open:
            self._close_all_dropdowns(); return
        self._close_all_dropdowns()
        self._sort_btn.update_idletasks()
        inner = self._sort_popup.master
        inner.update_idletasks()
        btn_x = self._sort_btn.winfo_rootx() - inner.winfo_rootx()
        btn_y = self._sort_btn.winfo_rooty() - inner.winfo_rooty() + self._sort_btn.winfo_height()
        self._place_popup_settled(self._sort_popup, btn_x, btn_y + 3,
                                  max(self._sort_btn.winfo_width(), 130))
        self._sort_popup.lift()
        self._sort_open = True
        self.bind("<Button-1>", self._close_sort_if_outside, add="+")

    def _close_sort_if_outside(self, event):
        w = event.widget
        while w is not None:
            if w in (self._sort_popup, self._sort_btn, self._filter_popup,
                     self._filter_btn, self._game_popup, self._game_btn,
                     self._date_popup, self._date_btn):
                return
            try: w = w.master
            except Exception: break
        self._sort_popup.place_forget();   self._sort_open   = False
        self._filter_popup.place_forget(); self._filter_open = False
        self._game_popup.place_forget();   self._game_open   = False
        self._date_popup.place_forget();   self._date_open   = False

    def _select_sort(self, value):
        self._sort_var.set(value)
        self._sort_btn.configure(text=value)
        for _k, _r in self._sort_rows.items():
            _r.set_active(_k == value)
        self._sort_popup.place_forget()
        self._sort_open = False
        self._on_sort_change(value)

    def _on_sort_change(self, value, reset_visible=True):
        if not self.videos: return
        if reset_visible: self._visible_count = 50
        def safe_mtime(f): return mtime_from_filename(os.path.basename(f), os.path.dirname(f))
        def safe_size(f):
            try: return os.path.getsize(f)
            except Exception: return 0
        if value == "Newest":   self.videos.sort(key=safe_mtime, reverse=True)
        elif value == "Oldest": self.videos.sort(key=safe_mtime, reverse=False)
        elif value == "Largest":  self.videos.sort(key=safe_size, reverse=True)
        elif value == "Smallest": self.videos.sort(key=safe_size, reverse=False)
        self._render_grid()
        threading.Thread(target=self._load_thumbnails, daemon=True).start()

    def _toggle_filter_dropdown(self):
        if self._filter_open:
            self._close_all_dropdowns(); return
        self._close_all_dropdowns()
        self._filter_btn.update_idletasks()
        inner = self._filter_popup.master
        inner.update_idletasks()
        btn_x = self._filter_btn.winfo_rootx() - inner.winfo_rootx()
        btn_y = self._filter_btn.winfo_rooty() - inner.winfo_rooty() + self._filter_btn.winfo_height()
        self._place_popup_settled(self._filter_popup, btn_x, btn_y + 3,
                                  max(self._filter_btn.winfo_width(), 130))
        self._filter_popup.lift()
        self._filter_open = True
        self.bind("<Button-1>", self._close_sort_if_outside, add="+")

    def _select_filter(self, value):
        self._filter_var = value
        self._filter_btn.configure(text=value)
        for _k, _r in self._filter_rows.items():
            _r.set_active(_k == value)
        self._filter_popup.place_forget()
        self._filter_open = False
        self._visible_count = 50
        self._render_grid()
        threading.Thread(target=self._load_thumbnails, daemon=True).start()

    # ── Date (calendar) filter ────────────────────────────────────────────────

    def _clip_date(self, v):
        """The calendar day a clip belongs to (filename date, else file mtime)."""
        ent = self._file_stat_cache.get(v)
        dt = ent[3] if ent else None
        if dt is None:
            try:
                ts = mtime_from_filename(os.path.basename(v), os.path.dirname(v))
                dt = datetime.datetime.fromtimestamp(ts) if ts else None
            except Exception:
                dt = None
        return dt.date() if dt else None

    def _apply_date_filter(self, clip_list):
        a, b = self._date_from, self._date_to
        if a is None:
            return clip_list
        out = []
        for v in clip_list:
            d = self._clip_date(v)
            if d is not None and a <= d <= b:
                out.append(v)
        return out

    def _clip_dates(self):
        """{date: number of clips} for every clip that is loaded."""
        out = {}
        for v in self.videos:
            d = self._clip_date(v)
            if d:
                out[d] = out.get(d, 0) + 1
        return out

    def _date_btn_text(self):
        # Icon only: never show the selected date next to the calendar icon.
        return ""
        a, b = self._date_from, self._date_to
        if a is None:
            return ""
        yr = datetime.date.today().year
        sfx = "" if b.year == yr else f".{b.year % 100:02d}"
        if a == b:
            return a.strftime("%d.%m.") + sfx
        if (a.year, a.month) == (b.year, b.month):
            return f"{a.day}\u2013{b.day}.{b.month}." + sfx
        return f"{a.strftime('%d.%m.')}\u2013{b.strftime('%d.%m.')}" + sfx

    def _set_date_range(self, a, b=None, pending=False, keep_open=False, jump=False):
        """a=None clears. Otherwise filter to a..b (b defaults to a)."""
        if a is None:
            b, pending = None, False
        elif b is None:
            b = a
        self._date_from, self._date_to, self._date_pending = a, b, pending
        btn = self._date_btn
        btn._accent_icon = a is not None
        btn.configure(text=self._date_btn_text())
        btn._draw()
        if keep_open and self._date_open:
            if jump and a:
                self._cal_ym = (a.year, a.month)
            self._build_calendar()
        else:
            self._close_all_dropdowns()
        self._visible_count = 50
        self._render_grid()
        threading.Thread(target=self._load_thumbnails, daemon=True).start()

    def _pick_date(self, d):
        """1st click = start day (popup stays open), 2nd click = end day (any order)."""
        if self._date_from is not None and self._date_pending:
            lo, hi = sorted((self._date_from, d))
            self._set_date_range(lo, hi, pending=False)            # closes the popup
        else:
            self._set_date_range(d, d, pending=True, keep_open=True)

    def _step_date(self, delta):
        """Shift the whole selected range (or today) by `delta` days."""
        a = self._date_from or datetime.date.today()
        b = self._date_to or a
        try:
            td = datetime.timedelta(days=delta)
            self._set_date_range(a + td, b + td, keep_open=True, jump=True)
        except OverflowError:
            pass

    def _cal_shift_month(self, delta):
        y, m = self._cal_ym
        m += delta
        while m < 1:
            m += 12; y -= 1
        while m > 12:
            m -= 12; y += 1
        self._cal_ym = (y, m)
        self._cal_year_open = False
        self._build_calendar()

    def _toggle_year_picker(self):
        """Open/close the year list under the calendar header."""
        self._cal_year_open = not getattr(self, "_cal_year_open", False)
        self._build_calendar()

    def _cal_pick_year(self, year):
        """Jump the calendar to `year` (same month) and close the year list."""
        _, m = self._cal_ym
        self._cal_ym = (year, m)
        self._cal_year_open = False
        self._build_calendar()

    def _toggle_date_popup(self):
        if self._date_open:
            self._close_all_dropdowns(); return
        self._close_all_dropdowns()
        base = self._date_from or datetime.date.today()
        self._cal_ym = (base.year, base.month)
        self._cal_year_open = False
        self._build_calendar()
        inner = self._date_popup.master
        inner.update_idletasks()
        self._date_popup.inner.update_idletasks()
        pw = self._date_popup.inner.winfo_reqwidth() + 10
        btn_x = self._date_btn.winfo_rootx() - inner.winfo_rootx()
        btn_y = self._date_btn.winfo_rooty() - inner.winfo_rooty() + self._date_btn.winfo_height()
        x = max(0, btn_x + self._date_btn.winfo_width() - pw)   # right-align under the button
        self._place_popup_settled(self._date_popup, x, btn_y + 3, pw)
        self._date_popup.lift()
        self._date_open = True
        self.bind("<Button-1>", self._close_sort_if_outside, add="+")

    def _build_calendar(self):
        import calendar as _calmod
        host = self._date_popup.inner
        for w in host.winfo_children():
            w.destroy()
        y, m = self._cal_ym
        a, b = self._date_from, self._date_to
        today = datetime.date.today()
        have = self._clip_dates()
        dim = _c("#6b6b88")
        range_bg = _c("#4a1830")      # tint for days between the two ends
        range_hov = _c("#5e2040")

        def lbl(parent, text, cmd=None, fg=TEXT_MUTED, bg=CARD_BG, font=FONT_TINY,
                hover=CARD_HOVER, **kw):
            l = tk.Label(parent, text=text, fg=fg, bg=bg, font=font,
                         cursor="hand2" if cmd else "", **kw)
            if cmd:
                l.bind("<ButtonRelease-1>", lambda e: cmd())
                l.bind("<Enter>", lambda e: l.configure(bg=hover))
                l.bind("<Leave>", lambda e: l.configure(bg=bg))
            return l

        # header: < Month Year >
        head = tk.Frame(host, bg=CARD_BG)
        head.pack(fill="x", pady=(2, 4))
        head.columnconfigure(1, weight=1)
        lbl(head, "\u2039", lambda: self._cal_shift_month(-1), font=FONT_BOLD, width=3).grid(row=0, column=0)
        yr_open = getattr(self, "_cal_year_open", False)
        mid = tk.Frame(head, bg=CARD_BG)
        mid.grid(row=0, column=1)
        tk.Label(mid, text=datetime.date(y, m, 1).strftime("%B"), fg=TEXT_PRIMARY,
                 bg=CARD_BG, font=FONT_BOLD).pack(side="left")
        arrow = "\u25b4" if yr_open else "\u25be"
        lbl(mid, f"{y} {arrow}", self._toggle_year_picker, fg=TEXT_PRIMARY,
            font=FONT_BOLD, padx=4).pack(side="left", padx=(4, 0))
        lbl(head, "\u203a", lambda: self._cal_shift_month(1), font=FONT_BOLD, width=3).grid(row=0, column=2)

        # year picker (prototype): years that have clips + this year + the shown year, newest first
        if yr_open:
            years = sorted({d.year for d in have} | {today.year, y}, reverse=True)
            picker = tk.Frame(host, bg=CARD_BG)
            picker.pack(padx=2, pady=(0, 4))
            for i, yy in enumerate(years):
                sel = (yy == y)
                lbl(picker, str(yy), lambda yy=yy: self._cal_pick_year(yy),
                    fg=ACCENT_FG if sel else TEXT_PRIMARY,
                    bg=ACCENT if sel else CARD_BG,
                    hover=ACCENT if sel else CARD_HOVER,
                    font=FONT_BOLD if sel else FONT_TINY, width=7, pady=3
                    ).grid(row=i // 3, column=i % 3, padx=1, pady=1)

        # month grid (Monday first)
        grid = tk.Frame(host, bg=CARD_BG)
        if not yr_open:
            grid.pack(padx=2)
        for c in range(7):
            grid.columnconfigure(c, uniform="cal", weight=1)
        for c, wd in enumerate(("Mo", "Tu", "We", "Th", "Fr", "Sa", "Su")):
            tk.Label(grid, text=wd, fg=dim, bg=CARD_BG, font=FONT_TINY, width=3).grid(row=0, column=c)
        for r, week in enumerate(_calmod.Calendar(0).monthdatescalendar(y, m), start=1):
            for c, d in enumerate(week):
                if d.month != m:
                    tk.Label(grid, text="", bg=CARD_BG, width=3).grid(row=r, column=c)
                    continue
                has = d in have
                if a and (d == a or d == b):
                    fg, bg, hov = ACCENT_FG, ACCENT, ACCENT
                elif a and a < d < b:
                    fg, bg, hov = TEXT_PRIMARY, range_bg, range_hov
                else:
                    fg = ACCENT if d == today else (TEXT_PRIMARY if has else dim)
                    bg, hov = CARD_BG, CARD_HOVER
                lbl(grid, str(d.day), lambda d=d: self._pick_date(d), fg=fg, bg=bg, hover=hov,
                    font=FONT_BOLD if has else FONT_TINY, width=3, pady=3
                    ).grid(row=r, column=c, padx=1, pady=1)

        # hint line
        if self._date_pending:
            hint = "Now pick the end date"
        elif a is None:
            hint = "Pick a start date, then an end date"
        elif a == b:
            hint = "Click another day to make a range"
        else:
            hint = f"{(b - a).days + 1} days selected"
        if not yr_open:
            tk.Label(host, text=hint, fg=dim, bg=CARD_BG, font=FONT_TINY).pack(pady=(3, 0))

        # footer: shift selection a day back/forward, jump to today, clear
        foot = tk.Frame(host, bg=CARD_BG)
        foot.pack(fill="x", pady=(4, 2))
        for c in range(4):
            foot.columnconfigure(c, weight=1, uniform="ft")
        for c, (txt, cmd) in enumerate((("\u2039 Day", lambda: self._step_date(-1)),
                                        ("Today", lambda: self._set_date_range(today, today, keep_open=True, jump=True)),
                                        ("Day \u203a", lambda: self._step_date(1)),
                                        ("Clear", lambda: self._set_date_range(None)))):
            lbl(foot, txt, cmd, pady=3).grid(row=0, column=c, sticky="ew", padx=1)

        # mouse wheel over the popup = previous/next month
        def _wheel(e):
            self._cal_shift_month(-1 if e.delta > 0 else 1)
            return "break"
        def _bind_wheel(w):
            w.bind("<MouseWheel>", _wheel)
            for ch in w.winfo_children():
                _bind_wheel(ch)
        _bind_wheel(host)

    def _toggle_game_dropdown(self):
        if self._game_open:
            self._close_all_dropdowns(); return
        self._close_all_dropdowns()
        self._game_btn.update_idletasks()
        inner = self._game_popup.master
        inner.update_idletasks()
        btn_x = self._game_btn.winfo_rootx() - inner.winfo_rootx()
        btn_y = self._game_btn.winfo_rooty() - inner.winfo_rooty() + self._game_btn.winfo_height()
        popup_w = max(self._game_btn.winfo_width(), 200)
        # popup padding (2 x 5) + scrollbar strip (10)
        self._game_popup_canvas.configure(width=popup_w - 20)
        self._place_popup_settled(self._game_popup, btn_x, btn_y + 3, popup_w)
        self._game_popup.lift()
        self._game_open = True
        self._game_popup_inner.update_idletasks()
        self._game_popup_on_configure()
        self._game_popup_canvas.yview_moveto(getattr(self, "_game_scroll_pos", 0))
        self.bind("<Button-1>", self._close_sort_if_outside, add="+")

    def _update_game_btn_text(self):
        """Button label: the single category, 'A, B' / 'N games' for a multi-select."""
        if self._game_multi:
            names = sorted(self._game_norm_map.get(k, k) for k in self._game_multi)
            txt = names[0] if len(names) == 1 else (
                ", ".join(names) if len(names) == 2 and len(", ".join(names)) <= 18
                else f"{len(names)} games")
        else:
            txt = self._game_norm_map.get(self._game_var, self._game_var)
        self._game_btn.configure(text=txt)

    def _game_refresh_rows(self):
        """Update the popup rows in place (keeps the popup open and its scroll)."""
        rows = getattr(self, "_game_rows", {})
        multi = self._game_multi
        for k, r in rows.items():
            try:
                r.set_checked((k in multi) if multi else None)
                r.set_active((not multi) and k == self._game_var)
            except Exception:
                pass

    def _toggle_game_multi(self, key):
        """Right click on a category: add/remove it from the multi-selection.
        The dropdown stays open so more categories can be picked."""
        if key == "All":
            self._game_multi = set()
            self._game_var = "All"
        else:
            multi = set(self._game_multi)
            if not multi and self._game_var not in ("All", "", None) and self._game_var in self._game_norm_map:
                multi.add(self._game_var)          # keep a single left-click choice
            if key in multi:
                multi.discard(key)
            else:
                multi.add(key)
            self._game_multi = multi
            self._game_var = "All"
        self._update_game_btn_text()
        self._game_refresh_rows()
        self._save_prefs()
        self._visible_count = 50
        self._render_grid()
        threading.Thread(target=self._load_thumbnails, daemon=True).start()

    def _select_game(self, key):
        self._game_var = key
        self._game_multi = set()
        self._update_game_btn_text()
        self._game_popup.place_forget()
        self._game_open = False
        self._game_scroll_pos = 0
        self._rebuild_game_popup()
        self._visible_count = 50
        self._render_grid()
        threading.Thread(target=self._load_thumbnails, daemon=True).start()

    def _category_key_for_video(self, vpath):
        """Stable category key for a clip. User-defined keyword groups are
        checked first (normalised filename prefix match), which lets one
        category absorb several different filename styles for the same
        game (e.g. "cs2", "counter-strike-2", "Cs2" all match a "cs2"
        keyword). Falls back to the original auto-detected game name if no
        keyword group matches. Returns None if nothing matches at all."""
        fname_norm = _normalize_game(os.path.splitext(os.path.basename(vpath))[0])
        for key, grp in self._keyword_groups.items():
            for kw in grp.get("keywords", ()):
                if kw and fname_norm.startswith(kw):
                    return key
        label = game_name_from_filename(os.path.basename(vpath))
        if not label or label == "Moments":
            return None
        return _normalize_game(label)

    def _recompute_categories(self):
        """(Re)build the category key -> label maps from self.videos.
        Keyword groups take priority over auto-detection. IMPORTANT: this
        never deletes anything from self._keyword_groups or
        self._category_renames — a category simply won't appear in the
        dropdown if nothing currently matches it (e.g. a transient/partial
        folder scan, or a drive that isn't mounted yet), but the underlying
        keywords stay saved and will reappear the moment something matches
        again. Actual removal only happens when the user explicitly deletes
        a keyword (see the categorize dialog's × button)."""
        self._game_auto_map = {}   # norm_key -> label
        _has_moments = False
        for vpath in self.videos:
            key = self._category_key_for_video(vpath)
            if key is None:
                _has_moments = True
                continue
            if key in self._keyword_groups:
                label = self._keyword_groups[key]["label"]
            else:
                label = game_name_from_filename(os.path.basename(vpath))
                existing = self._game_auto_map.get(key)
                if existing is not None and len(existing) >= len(label):
                    label = existing
            self._game_auto_map[key] = label

        # Display label = user rename if one exists (auto-detected
        # categories only — keyword-group labels are already the source
        # of truth), else the auto-detected / keyword-group label. Only
        # categories with at least one match right now show up here; the
        # rest stay untouched in self._keyword_groups / self._category_renames.
        self._game_norm_map = {
            key: self._category_renames.get(key, auto_label)
            for key, auto_label in self._game_auto_map.items()
        }
        # Clips without a game name ("Moments") get their own entry in the
        # game filter, under the same key the cloud-sync game list uses.
        if _has_moments:
            self._game_norm_map[self._CLOUD_NONE_KEY] = "Moments"
        self._dynamic_games = list(self._game_norm_map.keys())

    def _filter_key_for_video(self, vpath):
        """Category key used by the game filter: the clip's game key, or the
        shared "Moments" key for clips without a game name."""
        return self._category_key_for_video(vpath) or self._CLOUD_NONE_KEY

    def _game_filter(self, clip_list):
        if self._game_multi:
            keys = self._game_multi
            return [v for v in clip_list if self._filter_key_for_video(v) in keys]
        gv = self._game_var
        if not gv or gv == "All": return clip_list
        target_key = gv  # already a stable norm_key, not display text
        return [v for v in clip_list if self._filter_key_for_video(v) == target_key]

    def _game_sb_set(self, lo, hi):
        self._game_sb_lo = float(lo)
        self._game_sb_hi = float(hi)
        self._game_sb_draw()

    def _game_sb_geom(self):
        """Same handle geometry as the sidebar scrollbar (minimum handle length,
        travel scaled to the space that is left)."""
        h    = self._game_popup_sb.winfo_height()
        span = max(0.0, min(1.0, getattr(self, '_game_sb_hi', 1.0) - getattr(self, '_game_sb_lo', 0.0)))
        L    = min(max(span * h - 4, 28), max(h - 4, 1))
        room = max(h - 4 - L, 0)
        rng  = 1.0 - span
        y0   = 2 + ((getattr(self, '_game_sb_lo', 0.0) / rng) * room if rng > 1e-6 else 0)
        return h, y0, L, room, rng

    def _game_sb_draw(self):
        sb = self._game_popup_sb
        sb.delete("all")
        h = sb.winfo_height(); w = sb.winfo_width()
        if h < 2 or w < 2: return
        sb.create_rectangle(0, 0, w, h, fill=CARD_BG, outline="")
        h, _y0, _L, _room, _rng = self._game_sb_geom()
        if _rng <= 1e-6: return                      # everything fits: no handle
        y0 = int(round(_y0)); y1 = int(round(_y0 + _L))
        r = 4; x0, x1 = 2, 10
        col = _c("#3a3a5e")
        _aa_oval(sb, x0, y0, x1, y0 + r * 2, fill=col)
        _aa_oval(sb, x0, y1 - r * 2, x1, y1, fill=col)
        sb.create_rectangle(x0, y0 + r, x1, y1 - r, fill=col, outline="")

    def _game_popup_on_configure(self, e=None):
        self._game_popup_canvas.configure(scrollregion=self._game_popup_canvas.bbox("all"))
        self._game_popup_canvas.itemconfig(self._game_popup_win,
            width=self._game_popup_canvas.winfo_width() or 180)
        content_h = self._game_popup_inner.winfo_reqheight()
        self._game_popup_canvas.configure(height=min(content_h, 350))
        self._game_popup_sb.after(1, self._game_sb_draw)

    def _rebuild_game_popup(self):
        for w in self._game_popup_inner.winfo_children():
            w.destroy()
        _tally = {}
        for _v in self.videos:
            _k = self._filter_key_for_video(_v)
            _tally[_k] = _tally.get(_k, 0) + 1
        def count_for(key):
            if key == "All": return len(self.videos)
            return _tally.get(key, 0)

        self._game_rows = {}
        _multi = self._game_multi

        def _popup_btn(key, text, command, renamable=True):
            # left click = pick only this one | right click = add/remove it from
            # a multi-selection (popup stays open) | middle click = rename
            r = PopupRow(
                self._game_popup_inner, text, command, font=FONT_TINY, height=28,
                active=((not _multi) and key == self._game_var),
                check=((key in _multi) if _multi else None),
                right_text=str(count_for(key)),
                on_wheel=lambda e: self._game_popup_canvas.yview_scroll(
                    int(-1 * (e.delta / 120)), "units"),
                on_right=lambda e, k=key: self._toggle_game_multi(k),
                on_middle=((lambda e, k=key: self._open_rename_category_dialog(k))
                           if renamable else None))
            r.pack(fill="x", pady=1)
            self._game_rows[key] = r

        # Alphabetical by the (possibly renamed) display name, not by clip count.
        all_games = sorted(self._dynamic_games,
                           key=lambda k: self._game_norm_map.get(k, k).lower())
        _popup_btn("All", "All", lambda: self._select_game("All"), renamable=False)
        for key in all_games:
            display = self._game_norm_map.get(key, key)
            _popup_btn(key, display, lambda k=key: self._select_game(k),
                       renamable=(key != self._CLOUD_NONE_KEY))
        self._game_popup_inner.update_idletasks()
        self._game_popup_on_configure()

    def _open_rename_category_dialog(self, key):
        """Right-click a category in the popup to rename its display label.
        This only changes what's shown in the UI — it never touches clip
        filenames on disk, and clips still match the category the same way
        they always did (via the underlying detected game key)."""
        if key == "All" or key == self._CLOUD_NONE_KEY or key not in self._game_norm_map:
            return
        current      = self._game_norm_map.get(key, key)
        auto_default = self._game_auto_map.get(key, current)

        dlg = tk.Toplevel(self, bg=CARD_BG)
        dlg.title("Rename Category"); dlg.resizable(False, False)
        dlg.grab_set(); dlg.focus_set()
        try:
            hwnd  = ctypes.windll.user32.GetParent(dlg.winfo_id())
            value = ctypes.c_int(1)
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 20, ctypes.byref(value), ctypes.sizeof(value))
            r, g, b = _hex_to_rgb255(CARD_BG)
            color = ctypes.c_int(r | (g << 8) | (b << 16))
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 35, ctypes.byref(color), ctypes.sizeof(color))
        except Exception:
            pass

        pad = tk.Frame(dlg, bg=CARD_BG, padx=24, pady=20)
        pad.pack(fill="both", expand=True)
        tk.Label(pad, text="Category name", font=FONT_UI, bg=CARD_BG,
                 fg=TEXT_PRIMARY).pack(anchor="w", pady=(0, 8))

        entry_var = tk.StringVar(value=current)
        entry = tk.Entry(pad, textvariable=entry_var, font=FONT_UI,
                         bg=_c("#242444"), fg=TEXT_PRIMARY, insertbackground=TEXT_PRIMARY,
                         relief="flat", width=28)
        entry.pack(fill="x", ipady=6)
        entry.select_range(0, "end")
        entry.focus_set()

        tk.Label(pad, text=f'Only renames the label — clip files aren\'t touched. '
                            f'Auto-detected name: "{auto_default}"',
                 font=FONT_TINY, bg=CARD_BG, fg=TEXT_DIM,
                 wraplength=280, justify="left").pack(anchor="w", pady=(8, 0))

        tk.Frame(dlg, bg=_c("#2A2A4A"), height=1).pack(fill="x", pady=(12, 0))
        btn_row = tk.Frame(dlg, bg=CARD_BG, padx=16, pady=12)
        btn_row.pack(fill="x")

        def _save(event=None):
            new_name = entry_var.get().strip()
            if not new_name:
                dlg.destroy(); return
            if key in self._keyword_groups:
                self._keyword_groups[key]["label"] = new_name
            elif new_name == auto_default:
                self._category_renames.pop(key, None)
            else:
                self._category_renames[key] = new_name
            self._recompute_categories()
            self._update_game_btn_text()
            self._save_prefs()
            self._rebuild_game_popup()
            dlg.destroy()

        def _reset_default():
            entry_var.set(auto_default)

        RoundedButton(btn_row, text="Cancel", command=dlg.destroy,
                     fg=TEXT_PRIMARY, bg=_c("#242444"), hover_bg=_c("#2A2A4A"),
                     font=FONT_UI, padx=14, pady=6).pack(side="right", padx=(6, 0))
        RoundedButton(btn_row, text="Save", command=_save,
                     fg=ACCENT_FG, bg=ACCENT, hover_bg=_c("#FF5C8D"),
                     font=FONT_BOLD, padx=14, pady=6).pack(side="right")
        RoundedButton(btn_row, text="Reset to default", command=_reset_default,
                     fg=TEXT_MUTED, bg=_c("#242444"), hover_bg=_c("#2A2A4A"),
                     font=FONT_UI, padx=10, pady=6).pack(side="left")

        dlg.bind("<Return>", _save)
        dlg.bind("<Escape>", lambda e: dlg.destroy())

        self.update_idletasks(); dlg.update_idletasks()
        pw, ph = self.winfo_width(), self.winfo_height()
        px, py = self.winfo_rootx(), self.winfo_rooty()
        dw, dh = dlg.winfo_reqwidth(), dlg.winfo_reqheight()
        dlg.geometry(f"+{px + (pw-dw)//2}+{py + (ph-dh)//2}")

    def _open_categorize_dialog(self):
        """Assign a keyword to a category (existing or brand new), and manage
        every keyword/category you've already created. Every clip in the
        library whose filename starts with a keyword joins that keyword's
        category — not just the one currently playing. A keyword only ever
        lives in one category, but it CAN be moved to a different one at
        any time by re-adding it here. Everything is written to the same
        prefs file the app already saves on every change, so nothing here
        needs a separate "save" step — it's persisted automatically."""
        dlg = tk.Toplevel(self, bg=CARD_BG)
        dlg.title("Categories"); dlg.resizable(False, False)
        dlg.grab_set(); dlg.focus_set()
        try:
            hwnd  = ctypes.windll.user32.GetParent(dlg.winfo_id())
            value = ctypes.c_int(1)
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 20, ctypes.byref(value), ctypes.sizeof(value))
            r, g, b = _hex_to_rgb255(CARD_BG)
            color = ctypes.c_int(r | (g << 8) | (b << 16))
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 35, ctypes.byref(color), ctypes.sizeof(color))
        except Exception:
            pass

        pad = tk.Frame(dlg, bg=CARD_BG, padx=24, pady=20)
        pad.pack(fill="both", expand=True)

        # Prefill the keyword from whatever's currently playing, if anything.
        default_kw = ""
        if self.current_idx is not None and self.videos and self.current_idx < len(self.videos):
            cur_name = os.path.basename(self.videos[self.current_idx])
            default_kw = game_name_from_filename(cur_name) or ""
            if default_kw == "Moments":
                default_kw = ""

        tk.Label(pad, text="Keyword", font=FONT_UI, bg=CARD_BG,
                 fg=TEXT_PRIMARY).pack(anchor="w", pady=(0, 6))
        tk.Label(pad, text="Any clip whose filename starts with this joins the category below. "
                            "Reusing a keyword that's already assigned moves it to the new category.",
                 font=FONT_TINY, bg=CARD_BG, fg=TEXT_DIM,
                 wraplength=280, justify="left").pack(anchor="w", pady=(0, 8))
        kw_var = tk.StringVar(value=default_kw)
        kw_entry = tk.Entry(pad, textvariable=kw_var, font=FONT_UI,
                            bg=_c("#242444"), fg=TEXT_PRIMARY, insertbackground=TEXT_PRIMARY,
                            relief="flat", width=28)
        kw_entry.pack(fill="x", ipady=6)
        kw_entry.select_range(0, "end")
        kw_entry.focus_set()

        tk.Label(pad, text="Category", font=FONT_UI, bg=CARD_BG,
                 fg=TEXT_PRIMARY).pack(anchor="w", pady=(14, 6))
        tk.Label(pad, text="Type an existing category to add to it, or a new name to create one.",
                 font=FONT_TINY, bg=CARD_BG, fg=TEXT_DIM,
                 wraplength=280, justify="left").pack(anchor="w", pady=(0, 8))
        cat_var = tk.StringVar(value=default_kw)
        cat_entry = tk.Entry(pad, textvariable=cat_var, font=FONT_UI,
                             bg=_c("#242444"), fg=TEXT_PRIMARY, insertbackground=TEXT_PRIMARY,
                             relief="flat", width=28)
        cat_entry.pack(fill="x", ipady=6)

        err_lbl = tk.Label(pad, text="", font=FONT_TINY, bg=CARD_BG, fg=_c("#e0685f"),
                           wraplength=280, justify="left")
        err_lbl.pack(anchor="w", pady=(8, 0))

        # ── Persistent list of everything already set up ──────────────────
        tk.Label(pad, text="Your categories", font=FONT_UI, bg=CARD_BG,
                 fg=TEXT_PRIMARY).pack(anchor="w", pady=(16, 2))
        tk.Label(pad, text="Click a keyword to reuse it above (edit the category, then Save "
                            "to move it). Click × to remove it.",
                 font=FONT_TINY, bg=CARD_BG, fg=TEXT_DIM,
                 wraplength=280, justify="left").pack(anchor="w", pady=(0, 6))

        list_wrap = tk.Frame(pad, bg=CARD_BG)
        list_wrap.pack(fill="both", expand=True)
        list_canvas = tk.Canvas(list_wrap, bg=CARD_BG, highlightthickness=0,
                                width=280, height=160)
        list_scroll = tk.Scrollbar(list_wrap, orient="vertical", command=list_canvas.yview)
        list_inner = tk.Frame(list_canvas, bg=CARD_BG)
        list_inner.bind("<Configure>",
                        lambda e: list_canvas.configure(scrollregion=list_canvas.bbox("all")))
        list_canvas.create_window((0, 0), window=list_inner, anchor="nw", width=280)
        list_canvas.configure(yscrollcommand=list_scroll.set)
        list_canvas.pack(side="left", fill="both", expand=True)
        list_scroll.pack(side="right", fill="y")
        list_canvas.bind("<MouseWheel>",
                         lambda e: list_canvas.yview_scroll(int(-1 * (e.delta / 120)), "units"))

        def _use_kw(k, c):
            kw_var.set(k); cat_var.set(c)
            err_lbl.configure(text="")
            kw_entry.focus_set()
            kw_entry.select_range(0, "end")

        def _del_kw(gk, k):
            grp = self._keyword_groups.get(gk)
            if grp and k in grp["keywords"]:
                grp["keywords"].remove(k)
                # Now genuinely empty and explicitly emptied by the user —
                # this is the one place a category should actually be
                # deleted (as opposed to _recompute_categories, which never
                # deletes anything on its own).
                if not grp["keywords"]:
                    del self._keyword_groups[gk]
            self._recompute_categories()
            self._rebuild_game_popup()
            self._save_prefs()
            self._visible_count = 50
            self._render_grid()
            threading.Thread(target=self._load_thumbnails, daemon=True).start()
            _refresh_existing_list()

        def _refresh_existing_list():
            for w in list_inner.winfo_children():
                w.destroy()
            if not self._keyword_groups:
                tk.Label(list_inner, text="No categories yet — add one above.",
                         font=FONT_TINY, bg=CARD_BG, fg=TEXT_DIM).pack(anchor="w", pady=4)
            for gkey, grp in sorted(self._keyword_groups.items(),
                                     key=lambda kv: kv[1]["label"].lower()):
                if not grp.get("keywords"):
                    continue
                row = tk.Frame(list_inner, bg=CARD_BG)
                row.pack(fill="x", pady=(4, 2))
                tk.Label(row, text=grp["label"], font=FONT_UI, bg=CARD_BG,
                         fg=TEXT_PRIMARY).pack(anchor="w")
                chip_row = tk.Frame(row, bg=CARD_BG)
                chip_row.pack(fill="x", pady=(2, 0))
                for kw in grp["keywords"]:
                    chip = tk.Frame(chip_row, bg=_c("#242444"))
                    chip.pack(side="left", padx=(0, 6), pady=2)
                    kw_lbl = tk.Label(chip, text=kw, font=FONT_TINY, bg=_c("#242444"),
                                      fg=TEXT_MUTED, padx=8, pady=3, cursor="hand2")
                    kw_lbl.pack(side="left")
                    kw_lbl.bind("<Button-1>", lambda e, k=kw, c=grp["label"]: _use_kw(k, c))
                    x_btn = tk.Label(chip, text="×", font=FONT_TINY, bg=_c("#242444"),
                                     fg=_c("#e0685f"), padx=6, cursor="hand2")
                    x_btn.pack(side="left")
                    x_btn.bind("<Button-1>", lambda e, gk=gkey, k=kw: _del_kw(gk, k))
            list_inner.update_idletasks()
            list_canvas.configure(scrollregion=list_canvas.bbox("all"))

        _refresh_existing_list()

        tk.Frame(dlg, bg=_c("#2A2A4A"), height=1).pack(fill="x", pady=(12, 0))
        btn_row = tk.Frame(dlg, bg=CARD_BG, padx=16, pady=12)
        btn_row.pack(fill="x")

        def _save(event=None):
            kw_raw  = kw_var.get().strip()
            cat_raw = cat_var.get().strip()
            if not kw_raw or not cat_raw:
                err_lbl.configure(text="Both fields are required.")
                return
            kw_norm = _normalize_game(kw_raw)
            if not kw_norm:
                err_lbl.configure(text="Keyword must contain letters or numbers.")
                return

            # Keywords can be reassigned — detach from wherever they
            # currently live before attaching to the target category.
            for grp in self._keyword_groups.values():
                if kw_norm in grp["keywords"]:
                    grp["keywords"].remove(kw_norm)

            # Match the typed category against an existing one by display
            # name, whether it's already a keyword group or still purely
            # auto-detected.
            cat_key = None
            for gkey, grp in self._keyword_groups.items():
                if grp["label"].lower() == cat_raw.lower():
                    cat_key = gkey; break
            if cat_key is None:
                for gkey, label in self._game_norm_map.items():
                    if gkey == self._CLOUD_NONE_KEY:
                        continue
                    if label.lower() == cat_raw.lower():
                        cat_key = gkey; break

            target_key = cat_key or _normalize_game(cat_raw)
            if target_key in self._keyword_groups:
                if kw_norm not in self._keyword_groups[target_key]["keywords"]:
                    self._keyword_groups[target_key]["keywords"].append(kw_norm)
            else:
                # New keyword group — reuses the auto-detected key if this
                # is promoting an existing auto category, so clips already
                # matching it keep matching under the same key.
                self._keyword_groups[target_key] = {"label": cat_raw, "keywords": [kw_norm]}
                self._category_renames.pop(target_key, None)

            self._recompute_categories()
            self._game_multi &= set(self._game_norm_map)
            if self._game_var != "All" and self._game_var not in self._game_norm_map:
                self._game_var = "All"
            self._update_game_btn_text()
            self._rebuild_game_popup()
            self._save_prefs()
            self._visible_count = 50
            self._render_grid()
            threading.Thread(target=self._load_thumbnails, daemon=True).start()

            err_lbl.configure(text="")
            kw_var.set(""); cat_var.set("")
            kw_entry.focus_set()
            _refresh_existing_list()

        RoundedButton(btn_row, text="Done", command=dlg.destroy,
                     fg=TEXT_PRIMARY, bg=_c("#242444"), hover_bg=_c("#2A2A4A"),
                     font=FONT_UI, padx=14, pady=6).pack(side="right", padx=(6, 0))
        RoundedButton(btn_row, text="Save", command=_save,
                     fg=ACCENT_FG, bg=ACCENT, hover_bg=_c("#FF5C8D"),
                     font=FONT_BOLD, padx=14, pady=6).pack(side="right")

        dlg.bind("<Return>", _save)
        dlg.bind("<Escape>", lambda e: dlg.destroy())

        self.update_idletasks(); dlg.update_idletasks()
        pw, ph = self.winfo_width(), self.winfo_height()
        px, py = self.winfo_rootx(), self.winfo_rooty()
        dw, dh = dlg.winfo_reqwidth(), dlg.winfo_reqheight()
        dlg.geometry(f"+{px + (pw-dw)//2}+{py + (ph-dh)//2}")

    def _open_compress_dialog(self):
        """Compress a clip — or every clip currently selected — to a target
        file size, each saved as a new file right next to its original
        (originals are left alone — delete them yourself if you want). Each
        new file's date matches its original exactly, both in the
        filename's timestamp and the actual file dates, so it sorts and
        displays identically. If clips are selected, this compresses all of
        them, one after another, to whichever size you pick."""
        if self._selected_clips:
            targets = [v for v in self.videos if v in self._selected_clips]
        elif self.current_idx is not None and self.videos:
            targets = [self.videos[self.current_idx]]
        else:
            return
        n = len(targets)

        if self._mpv_playing:
            self._mpv_pause()
            self._set_play_icon(False)

        dlg = tk.Toplevel(self, bg=CARD_BG)
        dlg.title("Compress"); dlg.resizable(False, False)
        dlg.grab_set(); dlg.focus_set()
        try:
            hwnd  = ctypes.windll.user32.GetParent(dlg.winfo_id())
            value = ctypes.c_int(1)
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 20, ctypes.byref(value), ctypes.sizeof(value))
            r, g, b = _hex_to_rgb255(CARD_BG)
            color = ctypes.c_int(r | (g << 8) | (b << 16))
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 35, ctypes.byref(color), ctypes.sizeof(color))
        except Exception:
            pass

        SIZE_OPTIONS = [("10 MB", 10), ("20 MB", 20), ("50 MB", 50)]
        encoding      = [False]
        cancelled     = [False]
        _encode_proc  = [None]

        def _on_close():
            cancelled[0] = True
            if _encode_proc[0] is not None:
                try: _encode_proc[0].kill()
                except Exception: pass
            dlg.destroy()

        dlg.protocol("WM_DELETE_WINDOW", _on_close)

        pad = tk.Frame(dlg, bg=CARD_BG, padx=28, pady=22)
        pad.pack(fill="both", expand=True)

        title_txt = "COMPRESS CLIP" if n == 1 else f"COMPRESS {n} CLIPS"
        tk.Label(pad, text=title_txt, font=FONT_TITLE,
                 bg=CARD_BG, fg=TEXT_PRIMARY).pack(anchor="center", pady=(0, 14))

        tk.Label(pad, text="Choose a Target Size", font=FONT_UI,
                 bg=CARD_BG, fg=TEXT_PRIMARY).pack(anchor="center")
        if n > 1:
            tk.Label(pad, text="Applies to every selected clip below.",
                     font=FONT_TINY, bg=CARD_BG, fg=TEXT_DIM).pack(anchor="center")
        size_row = tk.Frame(pad, bg=CARD_BG)
        size_row.pack(anchor="center", pady=(6, 14))

        size_btns = []
        def _select_size(idx):
            for i, b in enumerate(size_btns):
                b.configure(bg=ACCENT if i == idx else _c("#242444"),
                            fg=ACCENT_FG if i == idx else TEXT_PRIMARY)
            _start_encode(SIZE_OPTIONS[idx][1])

        for i, (label, _) in enumerate(SIZE_OPTIONS):
            b = tk.Button(size_row, text=label, font=FONT_UI,
                          bg=_c("#242444"), fg=TEXT_PRIMARY,
                          activebackground=ACCENT, relief="flat",
                          bd=0, padx=14, pady=6, cursor="hand2",
                          command=lambda i=i: _select_size(i))
            b.pack(side="left", padx=(0, 6))
            size_btns.append(b)

        # ── Preview: single thumbnail, or a scrollable list when batching ──────
        TW = 280
        if n == 1:
            src0 = targets[0]
            TH = int(TW * 9 / 16)
            card = tk.Frame(pad, bg=_c("#1A1A2E"), width=TW, height=TH + 48)
            card.pack_propagate(False)
            card.pack(pady=(0, 10))

            thumb_canvas = tk.Canvas(card, width=TW, height=TH,
                                     bg=_c("#0F0F1A"), highlightthickness=0)
            thumb_canvas.pack()
            thumb_canvas.create_text(TW//2, TH//2, text="", fill=TEXT_DIM,
                                     font=("Segoe UI", 18), tags="status")
            thumb_canvas._photo = None
            thumb_path = get_thumb(src0, self._ffmpeg)
            if thumb_path and os.path.exists(thumb_path):
                try:
                    from PIL import Image, ImageTk
                    img = Image.open(thumb_path).convert("RGB")
                    img = img.resize((TW, TH), Image.BILINEAR)
                    thumb_canvas._photo = ImageTk.PhotoImage(img)
                    thumb_canvas.delete("status")
                    thumb_canvas.create_image(0, 0, anchor="nw", image=thumb_canvas._photo)
                except Exception:
                    thumb_canvas.delete("status")
                    thumb_canvas.create_text(TW//2, TH//2, text="No preview",
                                             fill=TEXT_DIM, font=("Segoe UI", 11))
            status_bar = tk.Frame(card, bg=_c("#1A1A2E"))
            status_bar.pack(fill="x", expand=True)
        else:
            LIST_H = 130
            card = tk.Frame(pad, bg=_c("#1A1A2E"), width=TW, height=LIST_H + 48)
            card.pack_propagate(False)
            card.pack(pady=(0, 10))

            status_bar = tk.Frame(card, bg=_c("#1A1A2E"))
            status_bar.pack(side="bottom", fill="x")

            list_wrap = tk.Frame(card, bg=_c("#0F0F1A"))
            list_wrap.pack(fill="both", expand=True)
            list_canvas = tk.Canvas(list_wrap, bg=_c("#0F0F1A"), highlightthickness=0, width=TW)
            list_scroll = tk.Scrollbar(list_wrap, orient="vertical", command=list_canvas.yview)
            list_inner = tk.Frame(list_canvas, bg=_c("#0F0F1A"))
            list_inner.bind("<Configure>",
                            lambda e: list_canvas.configure(scrollregion=list_canvas.bbox("all")))
            list_canvas.create_window((0, 0), window=list_inner, anchor="nw")
            list_canvas.configure(yscrollcommand=list_scroll.set)
            list_canvas.pack(side="left", fill="both", expand=True)
            list_scroll.pack(side="right", fill="y")
            list_canvas.bind("<MouseWheel>",
                             lambda e: list_canvas.yview_scroll(int(-1*(e.delta/120)), "units"))

            clip_row_lbls = {}
            for t in targets:
                row_lbl = tk.Label(list_inner, text=os.path.basename(t), font=FONT_TINY,
                                   bg=_c("#0F0F1A"), fg=TEXT_MUTED, anchor="w", padx=10, pady=3)
                row_lbl.pack(fill="x")
                clip_row_lbls[t] = row_lbl

        prog_canvas = tk.Canvas(status_bar, bg=_c("#13131f"),
                                highlightthickness=0, height=3, width=TW)
        prog_canvas.pack(fill="x")
        status_lbl = tk.Label(status_bar, text="", font=FONT_TINY,
                              bg=_c("#1A1A2E"), fg=TEXT_MUTED, pady=6)
        status_lbl.pack()

        def _set_status(text, color, progress=None):
            status_lbl.configure(text=text, fg=color)
            if progress is not None:
                prog_canvas.delete("all")
                w = prog_canvas.winfo_width() or TW
                prog_canvas.create_rectangle(0, 0, int(w * min(1.0, progress)), 3,
                                             fill=ACCENT, outline="")

        def _mark_clip_done(t, ok):
            lbl = clip_row_lbls.get(t) if n > 1 else None
            if lbl is not None:
                lbl.configure(text=("✓  " if ok else "✗  ") + os.path.basename(t),
                              fg=(_c("#ccc364") if ok else _c("#e05555")))

        def _compress_one(src, nominal_mb, progress_cb):
            """Compress a single clip; returns the new file's path. Runs on
            the worker thread — progress_cb schedules its own UI updates."""
            ff  = self._ffmpeg
            dur = get_video_duration(src, ff)
            if dur <= 0:
                raise RuntimeError("Could not read clip duration")

            base, ext = os.path.splitext(src)
            ext = ext or ".mp4"
            dst = f"{base}_{nominal_mb}MB{ext}"
            counter = 1
            while os.path.exists(dst):
                dst = f"{base}_{nominal_mb}MB_{counter}{ext}"
                counter += 1

            # Every audio track is kept (copied unchanged), so the audio takes
            # what the source tracks take; budget the video with what is left.
            _rates = get_audio_bitrates(src, ff)
            if _rates is None:
                audio_bits = 160_000
            else:
                audio_bits = sum(r if r > 0 else 160_000 for r in _rates)
            # Small safety margin below the nominal cap — container
            # overhead means a bitrate calc'd for exactly nominal_mb can
            # land a little over.
            # Same cushion Share uses for its 10 MB option: a flat 0.5 MB
            # below the nominal cap, not a percentage — container overhead
            # eats into the budget by roughly the same fixed amount
            # regardless of target size.
            target_mb  = nominal_mb - 0.5
            video_bits = max(200_000,
                int((target_mb * 8 * 1024 * 1024 / dur) - audio_bits))
            bufsize    = video_bits * 2

            # Downscale only if the bit budget is genuinely too tight for
            # the source resolution — keeps quality up at small target
            # sizes instead of just blockiness.
            src_w, src_h, src_fps = get_video_info(src, ff)
            scale_filter = None
            if src_w and src_h:
                fps = src_fps or 30.0
                BPP_FLOOR = 0.07 * 0.4
                def _ref_bps(w, h):
                    return w * h * fps * BPP_FLOOR
                if video_bits < _ref_bps(src_w, src_h):
                    new_h = src_h
                    for h in (1080, 720, 540, 480, 360, 240):
                        if h >= src_h:
                            continue
                        new_h = h
                        new_w = src_w * h / src_h
                        if video_bits >= _ref_bps(new_w, h):
                            break
                    if new_h < src_h:
                        scale_filter = f"scale=-2:{new_h}"

            time_re = re.compile(r"time=(\d+):(\d+):(\d+\.?\d*)")
            def _pump(proc):
                buf = ""
                for chunk in iter(lambda: proc.stderr.read(512), b""):
                    buf += chunk.decode(errors="ignore")
                    m = time_re.search(buf)
                    if m:
                        cur = (int(m.group(1))*3600 + int(m.group(2))*60
                               + float(m.group(3)))
                        progress_cb(min(1.0, max(0.0, cur / dur)))

            vf = ["-vf", scale_filter] if scale_filter else []
            # one video + ALL audio tracks, audio copied as it is
            aud_copy = ["-map", "0:v:0?", "-map", "0:a?", "-c:a", "copy"]

            def _encode(codec_args):
                proc = subprocess.Popen(codec_args, stderr=subprocess.PIPE,
                                        creationflags=0x08000000)
                _encode_proc[0] = proc
                _pump(proc)
                proc.wait()
                _encode_proc[0] = None
                return proc.returncode

            rc = 1
            if not cancelled[0] and getattr(self, "_nvenc_ok", True):
                nvenc_args = [ff, "-y", "-i", src,
                              "-c:v", "h264_nvenc", "-preset", "fast", "-rc", "vbr",
                              *vf,
                              "-b:v", str(video_bits),
                              "-maxrate", str(video_bits),
                              "-bufsize", str(bufsize),
                              *aud_copy,
                              "-movflags", "+faststart", dst]
                rc = _encode(nvenc_args)
                self._nvenc_ok = (rc == 0)

            if rc != 0 and not cancelled[0]:
                if os.path.exists(dst):
                    try: os.remove(dst)
                    except Exception: pass
                cpu_args = [ff, "-y", "-i", src,
                            "-c:v", "libx264", "-preset", "veryfast",
                            *vf,
                            "-b:v", str(video_bits),
                            "-maxrate", str(video_bits),
                            "-bufsize", str(bufsize),
                            *aud_copy,
                            "-movflags", "+faststart", dst]
                rc = _encode(cpu_args)
                if rc != 0 and not cancelled[0]:
                    # last resort: the tracks couldn't be copied as they are,
                    # so re-encode every one at the same 160 kbps OBS records
                    if os.path.exists(dst):
                        try: os.remove(dst)
                        except Exception: pass
                    cpu_args = [a for a in cpu_args]
                    k = cpu_args.index("copy")
                    cpu_args[k:k + 1] = ["aac", "-b:a", "160k"]
                    rc = _encode(cpu_args)

            if rc != 0 or cancelled[0]:
                if os.path.exists(dst):
                    try: os.remove(dst)
                    except Exception: pass
                raise RuntimeError("ffmpeg failed" if not cancelled[0] else "cancelled")

            # Match the original's file dates exactly (the filename already
            # carries the same timestamp text, which is what the app's own
            # date display reads first).
            try:
                shutil.copystat(src, dst)
            except Exception:
                pass
            # the compressed clip keeps the original's track choice, volumes, speed
            self._post_main(lambda a=src, b=dst: self._copy_clip_settings(a, b))
            return dst

        def _start_encode(nominal_mb):
            if encoding[0]:
                return
            encoding[0] = True
            for b in size_btns:
                b.configure(state="disabled")

            def _run():
                done, failed = 0, []
                for idx, src in enumerate(targets):
                    if cancelled[0]:
                        break
                    base_frac = idx / n
                    span = 1.0 / n
                    short = os.path.basename(src)
                    def _progress_cb(local, bf=base_frac, sp=span, sn=short, i=idx):
                        f = bf + sp * local
                        txt = f"Encoding {i+1}/{n} — {sn}" if n > 1 else "Encoding…"
                        dlg.after(0, lambda: _set_status(txt, TEXT_MUTED, progress=f))
                    dlg.after(0, lambda bf=base_frac, sn=short, i=idx: _set_status(
                        (f"Encoding {i+1}/{n} — {sn}" if n > 1 else "Preparing…"),
                        TEXT_MUTED, progress=bf))
                    try:
                        _compress_one(src, nominal_mb, _progress_cb)
                        done += 1
                        dlg.after(0, lambda t=src: _mark_clip_done(t, True))
                    except Exception:
                        failed.append(short)
                        dlg.after(0, lambda t=src: _mark_clip_done(t, False))

                encoding[0] = False
                dlg.after(0, lambda: [b.configure(state="normal") for b in size_btns])
                if cancelled[0]:
                    return
                if failed:
                    dlg.after(0, lambda: _set_status(
                        f"Done — {done}/{n} compressed, {len(failed)} failed",
                        _c("#e0a05f"), progress=1.0))
                else:
                    suffix = "saved next to the original" if n == 1 else "saved next to the originals"
                    dlg.after(0, lambda: _set_status(
                        f"Done  •  {done}/{n} compressed  •  {suffix}",
                        _c("#FFFFFF"), progress=1.0))

                def _finish():
                    self._refresh_folder()
                    if self._selected_clips:
                        self._selected_clips.clear()
                        self._update_delete_size_lbl()
                dlg.after(0, _finish)

            threading.Thread(target=_run, daemon=True).start()

        tk.Frame(pad, bg=_c("#2A2A4A"), height=1).pack(fill="x", pady=(12, 0))
        btn_row = tk.Frame(pad, bg=CARD_BG, pady=10)
        btn_row.pack(fill="x")
        RoundedButton(btn_row, text="Close", command=_on_close,
                      fg=TEXT_PRIMARY, bg=_c("#242444"), hover_bg=_c("#2A2A4A"),
                      font=FONT_UI, padx=14, pady=6).pack(side="right")

        self.update_idletasks(); dlg.update_idletasks()
        pw, ph = self.winfo_width(), self.winfo_height()
        px, py = self.winfo_rootx(), self.winfo_rooty()
        dw, dh = dlg.winfo_reqwidth(), dlg.winfo_reqheight()
        dlg.geometry(f"+{px + (pw-dw)//2}+{py + (ph-dh)//2}")

    # ── Virtual grid ──────────────────────────────────────────────────────────

    def _vg_layout(self):
        w = self._vc.winfo_width()
        if w < 10: w = self._panel_width or 290
        pad = self._vg_pad
        cols = 1                       # always a single "tower" of clips
        cw   = max(60, w - 16)
        ch = int((cw - CARD_BORDER * 2) * 9 / 16)
        self._vg_cols     = cols
        self._vg_card_w   = cw
        self._vg_card_h   = ch
        self._vg_row_h    = ch + 22 + pad * 2
        self._vg_canvas_w = w

    def _vg_idx_at(self, canvas_x, canvas_y):
        pad    = self._vg_pad
        cols   = self._vg_cols
        cw     = self._vg_card_w
        rh     = self._vg_row_h
        total_w = getattr(self, '_vg_canvas_w', None) or self._vc.winfo_width()
        cell_w  = total_w // cols
        col = canvas_x // cell_w
        row = canvas_y // rh
        idx = row * cols + col
        if col < 0 or col >= cols: return None
        if idx < 0 or idx >= len(self._vg_display): return None
        cx0 = col * cell_w + (cell_w - cw) // 2
        cy0 = row * rh + pad
        if canvas_x < cx0 or canvas_x > cx0 + cw: return None
        if canvas_y < cy0 or canvas_y > cy0 + rh - pad: return None
        return idx

    def _vg_card_rect(self, idx):
        cols    = self._vg_cols
        cw      = self._vg_card_w
        rh      = self._vg_row_h
        pad     = self._vg_pad
        total_w = getattr(self, '_vg_canvas_w', None) or self._vc.winfo_width()
        cell_w  = total_w // cols
        col = idx % cols
        row = idx // cols
        cx0 = col * cell_w + (cell_w - cw) // 2
        cy0 = row * rh + pad
        return cx0, cy0, cx0 + cw, cy0 + rh - pad

    def _vg_draw_card(self, idx, hover=False):
        if idx < 0 or idx >= len(self._vg_display): return
        name  = self._vg_display[idx]
        x0, y0, x1, y1 = self._vg_card_rect(idx)
        cw    = self._vg_card_w
        ch    = self._vg_card_h
        is_active   = (self.current_idx is not None and
                       len(self.videos) > self.current_idx and
                       self.videos[self.current_idx] == name)
        is_selected = name in self._selected_clips
        tag_base = f"card_{idx}"
        self._vc.delete(tag_base)
        bg  = CARD_HOVER if hover else CARD_BG
        bdr = _c("#e0e0e0") if is_selected else (ACCENT if is_active else _c("#242444"))
        r   = 7
        p   = CARD_BORDER
        _rounded_rect(self._vc, x0, y0, x1, y1, r, fill=bdr, tags=tag_base)
        _rounded_rect(self._vc, x0+p, y0+p, x1-p, y1-p, max(r-p, 2), fill=bg, tags=tag_base)
        self._vc.create_rectangle(x0+p, y0+p, x1-p, y0+p+ch, fill=_c("#0F0F1A"), outline="", tags=tag_base)
        _rounded_rect(self._vc, x0+p, y0+p, x1-p, y0+p+ch, max(r-p, 2), fill=_c("#0F0F1A"), tags=tag_base)
        self._vc.create_rectangle(x0+p, y0+p+ch-r, x1-p, y0+p+ch, fill=_c("#0F0F1A"), outline="", tags=tag_base)
        thumb_w = cw - p * 2
        self._vg_place_thumb(idx, name, x0+p, y0+p, thumb_w, ch)
        self._vc.tag_raise(f"thumb_{idx}")
        # Heart drawn last so it sits above the thumbnail image
        heart_tag = f"heart_{name}"
        self._vc.delete(heart_tag)
        liked = name in self._liked_clips
        self._vc.create_image(x0+p + thumb_w - 6 + 4, y0+p + 6 - 4,
                              image=ui_icon_shadow("heart", 16,
                                                   _c("#e8454a") if liked else _c("#dddddd"),
                                                   filled=liked, stroke=2.1),
                              anchor="ne", tags=(tag_base, heart_tag))
        ly = y0 + p + ch
        self._vc.create_rectangle(x0+p, ly, x1-p, y1-p, fill=bg, outline="", tags=tag_base)
        date_str, mb_str, ago_str, _clip_dt = self._file_stat_cache.get(name, ("", "", "", None))
        if ago_str:
            ago_id = self._vc.create_text(x0+6, ly+11, text=ago_str, font=FONT_TINY,
                                           fill=TEXT_DIM, anchor="w", tags=tag_base)
            ago_bbox = self._vc.bbox(ago_id)
            date_x = ago_bbox[2] + 8 if ago_bbox else x0 + 6
        else:
            date_x = x0 + 6
        self._vc.create_text(date_x, ly+11, text=date_str, font=FONT_TINY, fill=TEXT_MUTED, anchor="w", tags=tag_base)
        self._vc.create_text(x1-6, ly+11, text=mb_str, font=FONT_TINY, fill=TEXT_DIM, anchor="e", tags=tag_base)

    def _vg_place_thumb(self, idx, name, x, y, w, h):
        tag = f"thumb_{idx}"
        size_changed = self._vg_thumb_sizes.get(name) != (w, h)
        pos_changed  = self._vg_thumb_pos.get(name)  != (x, y)
        if name not in self._vg_thumb_cvs or size_changed or pos_changed:
            self._vc.delete(tag)
            self._vg_thumb_cvs[name] = tag
            self._vg_thumb_pos[name] = (x, y)
            if name in self.thumbnails and not size_changed:
                photo = self.thumbnails[name]
                self._vc.create_image(x, y, anchor="nw", image=photo, tags=tag)
                self._vg_draw_dur_overlay(name, x, y, w, h, tag)
                self._vc.tag_raise(tag)
            else:
                self._vc.create_rectangle(x, y, x+w, y+h, fill=_c("#0F0F1A"), outline="", tags=tag)
                self._vc.create_image(x+w//2, y+h//2, image=ui_icon("play", 22, TEXT_DIM),
                              anchor="center", tags=tag)
                if name in self.thumbnails or size_changed:
                    self._vg_blit_thumb(name, w, h)
            self._vg_thumb_sizes[name] = (w, h)
        else:
            if name not in self._vg_photo_refs:
                self._vg_blit_thumb(name, w, h)

    def _vg_draw_dur_overlay(self, name, x, y, w, h, tag):
        dur_key = name  # name is now a full path
        full_dur = self._durations.get(dur_key, "")

        # If clip is trimmed, show trimmed duration instead of full duration
        dur = full_dur
        if name in self._trim_enabled and name in self._trims and full_dur:
            try:
                trim = self._trims[name]  # [start_0_10000, end_0_10000]
                # Parse full duration string to total seconds
                parts = full_dur.split(":")
                if len(parts) == 2:
                    total_s = int(parts[0]) * 60 + int(parts[1])
                else:
                    total_s = int(parts[0]) * 3600 + int(parts[1]) * 60 + int(parts[2])
                trimmed_s = int(total_s * (trim[1] - trim[0]) / 10000)
                if trimmed_s >= 3600:
                    dur = f"{trimmed_s//3600}:{(trimmed_s%3600)//60:02d}:{trimmed_s%60:02d}"
                else:
                    dur = f"{trimmed_s//60}:{trimmed_s%60:02d}"
            except Exception:
                dur = full_dur


        # Trim icon — top-left, only when clip has an active trim.
        # Same vector scissors as the trim button in the player controls.
        is_trimmed = name in self._trim_enabled
        badge_x = x + 5 - 4
        if is_trimmed:
            try:
                self._vc.create_image(badge_x, y + 5 - 4, anchor="nw",
                                      image=ui_icon_shadow("scissors", 17, _c("#dddddd"), stroke=1.7),
                                      tags=tag)
                badge_x += 21          # the music icon goes right of the scissors
            except Exception:
                pass

        # Music icon — only when the clip has its own audio tracks / volumes
        # (not just the defaults). Next to the scissors when trimmed, otherwise
        # it takes the scissors' place. Same shadow as the scissors.
        if (name in getattr(self, "_at_clip_sel", {})
                or name in getattr(self, "_at_clip_vol", {})):
            try:
                self._vc.create_image(badge_x, y + 5 - 4, anchor="nw",
                                      image=ui_icon_shadow("music", 17, _c("#dddddd"), stroke=1.7),
                                      tags=tag)
            except Exception:
                pass

    def _vg_blit_thumb(self, name, w=None, h=None):
        p = CARD_BORDER
        if w is None: w = self._vg_card_w - p * 2
        if h is None: h = self._vg_card_h
        thumb_path = self._thumb_path(name)
        if thumb_path is None: return
        try:
            from PIL import Image, ImageTk
            raw = Image.open(thumb_path).convert("RGB")
            src_w, src_h = raw.size
            # Scale so width fills slot exactly, then center-crop height
            scale = w / src_w
            new_h = int(src_h * scale)
            raw = raw.resize((w, new_h), Image.LANCZOS)
            if new_h > h:
                top = (new_h - h) // 2
                raw = raw.crop((0, top, w, top + h))
            elif new_h < h:
                img = Image.new("RGB", (w, h), (0, 0, 0))
                img.paste(raw, (0, (h - new_h) // 2))
                raw = img
            photo = ImageTk.PhotoImage(raw)
            self.thumbnails[name]     = photo
            self._vg_photo_refs[name] = photo
            self._vg_thumb_sizes[name] = (w, h)
        except Exception:
            return
        try:
            idx = self._vg_display.index(name)
        except ValueError:
            return
        pos = self._vg_thumb_pos.get(name)
        if pos is None:
            x0, y0, x1, y1 = self._vg_card_rect(idx)
            pos = (x0 + CARD_BORDER, y0 + CARD_BORDER)
        x, y = pos
        tag = f"thumb_{idx}"
        self._vc.delete(tag)
        self._vc.create_image(x, y, anchor="nw", image=photo, tags=tag)
        self._vg_draw_dur_overlay(name, x, y, w, h, tag)
        self._vg_thumb_pos[name] = (x, y)
        self._vc.tag_raise(tag)
        # Always raise heart above thumb so it's never buried
        self._vc.tag_raise(f"heart_{name}")

    def _vg_refresh(self):
        n    = len(self._vg_display)
        cols = self._vg_cols
        rh   = self._vg_row_h
        rows = (n + cols - 1) // cols
        w    = getattr(self, '_vg_canvas_w', None) or self._vc.winfo_width() or 290
        extra_h = 76 if self._vg_total > n else 0
        total_h = rows * rh + self._vg_pad + extra_h
        self._vc.configure(scrollregion=(0, 0, w, max(total_h, 1)))
        self._vc.delete("see_more")
        self._vc.delete("load_all")
        for i in range(n):
            self._vg_draw_card(i, hover=(i == self._vg_hover_idx))
        if self._vg_total > n:
            remaining = self._vg_total - n
            btn_y = rows * rh + self._vg_pad + 4
            self._vc.create_rectangle(8, btn_y, w-8, btn_y+30,
                fill=SIDEBAR_BG, outline=TEXT_DIM, width=1, tags=("see_more", "see_more_rect"))
            self._vc.create_text(w//2, btn_y+15,
                text=f"Load more  ({min(self._see_more_step, remaining)} clips)",
                font=FONT_TINY, fill=TEXT_MUTED, anchor="center", tags=("see_more", "see_more_text"))
            self._vc.tag_bind("see_more", "<Button-1>", lambda e: (self._show_more(), "break"))
            self._vc.tag_bind("see_more", "<Enter>",
                lambda e: self._vc.itemconfig("see_more_rect", fill=CARD_HOVER))
            self._vc.tag_bind("see_more", "<Leave>",
                lambda e: self._vc.itemconfig("see_more_rect", fill=SIDEBAR_BG))
            la_y = btn_y + 38
            self._vc.create_rectangle(8, la_y, w-8, la_y+30,
                fill=SIDEBAR_BG, outline=TEXT_DIM, width=1, tags=("load_all", "load_all_rect"))
            self._vc.create_text(w//2, la_y+15,
                text=f"Load all  ({remaining} clips)",
                font=FONT_TINY, fill=TEXT_MUTED, anchor="center", tags=("load_all", "load_all_text"))
            self._vc.tag_bind("load_all", "<Button-1>", lambda e: (self._load_all(), "break"))
            self._vc.tag_bind("load_all", "<Enter>",
                lambda e: self._vc.itemconfig("load_all_rect", fill=CARD_HOVER))
            self._vc.tag_bind("load_all", "<Leave>",
                lambda e: self._vc.itemconfig("load_all_rect", fill=SIDEBAR_BG))
        display_set = set(self._vg_display)
        for name in list(self._vg_thumb_cvs.keys()):
            if name not in display_set:
                self._vg_thumb_cvs.pop(name, None)

    def _on_sash_press(self, e):
        try:
            self._sash_dragging = "sash" in str(self._pane.identify(e.x, e.y))
            if self._sash_dragging:
                # distance from the pointer to the sash, so the sash doesn't jump
                self._sash_off = self._pane.sash_coord(0)[0] - e.x
        except Exception:
            self._sash_dragging = False

    # While the divider MOVES, the rows of buttons (header, sidebar filters, player
    # controls) are taken out of the layout and replaced by blank spacers of the same
    # height - so they are invisible and are not re-laid-out / redrawn every frame.
    # When the divider stops for a moment (timer) or is released, they come back.
    _THAW_MS = 120

    def _freeze_buttons(self):
        if getattr(self, "_frozen", None):
            return
        frozen = []
        for name in ("_hdr_frame", "_rh_frame", "_controls_frame"):
            f = getattr(self, name, None)
            try:
                if f is None or not f.winfo_ismapped():
                    continue
                info = f.pack_info()
                info.pop("in", None)
                sp = tk.Frame(f.master, bg=f.cget("bg"), bd=0, highlightthickness=0,
                              width=1, height=max(f.winfo_height(), 1))
                sp_info = dict(info, ipady=0, ipadx=0, expand=0)
                sp.pack(before=f, **sp_info)
                f.pack_forget()
                frozen.append((f, sp, info))
            except Exception:
                pass
        self._frozen = frozen

    def _thaw_buttons(self):
        job = getattr(self, "_thaw_job", None)
        if job:
            try:
                self.after_cancel(job)
            except Exception:
                pass
            self._thaw_job = None
        frozen = getattr(self, "_frozen", None)
        if not frozen:
            return
        self._frozen = None
        for f, sp, info in frozen:
            try:
                f.pack(before=sp, **info)
                sp.destroy()
            except Exception:
                pass

    def _schedule_thaw(self):
        job = getattr(self, "_thaw_job", None)
        if job:
            try:
                self.after_cancel(job)
            except Exception:
                pass
        self._thaw_job = self.after(self._THAW_MS, self._thaw_buttons)

    def _set_list_cover(self, on):
        """Hide / show the clip list behind a plain cover (no item juggling)."""
        try:
            if on and not self._list_cover_on:
                self._list_cover.place(x=0, y=0, relwidth=1, relheight=1)
                self._list_cover.lift()
                self._list_cover_on = True
            elif not on and self._list_cover_on:
                self._list_cover.place_forget()
                self._list_cover_on = False
        except Exception:
            pass

    def _on_sash_motion(self, e):
        """Own sash drag: the divider is clamped HARD to the allowed range while
        dragging (it simply stops there - no overshoot and snap-back), and the
        clip list is hidden until the divider is released."""
        if not getattr(self, "_sash_dragging", False):
            return None                     # not on the sash: let Tk handle it
        try:
            self._set_list_cover(True)
            pane_w = self._pane.winfo_width()
            sash_w = int(self._pane.cget("sashwidth"))
            left_min, right_min = 400, 220                         # the pane's minsize values
            lo = max(left_min, pane_w - self.SIDEBAR_MAX_W - sash_w)   # sidebar max width
            hi = pane_w - right_min - sash_w                           # sidebar min width
            lo = min(lo, hi)
            x = int(e.x + getattr(self, "_sash_off", 0))
            x = max(lo, min(x, hi))
            self._freeze_buttons()           # buttons vanish while the divider moves
            if x != self._pane.sash_coord(0)[0]:
                self._pane.sash_place(0, x, 0)
            self._schedule_thaw()            # ...and return once it has been still for a moment
        except Exception:
            pass
        return "break"

    def _on_sash_release(self, e):
        if not getattr(self, "_sash_dragging", False):
            return
        self._sash_dragging = False
        self._thaw_buttons()                 # buttons show again right away
        self.after(30, self._after_sash_drag)

    def _after_sash_drag(self):
        """Sash released: lay the clip list out once at its final width."""
        if getattr(self, "_sash_dragging", False):
            return
        self._thaw_buttons()             # no-op if the release already did it
        job = getattr(self, "_resize_job", None)
        if job:
            try:
                self.after_cancel(job)
            except Exception:
                pass
            self._resize_job = None
        try:
            self._panel_width = self._vc.winfo_width()
        except Exception:
            pass
        self._vg_on_resize()
        self._set_list_cover(False)      # clips come back, already laid out at the final width

    def _sash_drag_active(self):
        """True while the sash is really being dragged (self-heals if the
        release event was missed: left mouse button no longer down)."""
        if getattr(self, "_win_sizing", False):
            return True                  # window edge being dragged: wait for release
        if not getattr(self, "_sash_dragging", False):
            return False
        try:
            if not (ctypes.windll.user32.GetAsyncKeyState(0x01) & 0x8000):
                self._sash_dragging = False
                self.after(30, self._after_sash_drag)     # also uncovers the list
                return False
        except Exception:
            pass
        return True

    def _win_size_watch(self):
        """Polls (from Tk's own loop) for the end of a window-edge drag, then
        runs ONE final layout pass. Also self-heals if the release was missed."""
        try:
            if self._win_sizing and not (ctypes.windll.user32.GetAsyncKeyState(0x01) & 0x8000):
                self._win_sizing = False
                self._win_size_pending = True
            if self._win_size_pending and not self._win_sizing:
                self._win_size_pending = False
                self._after_window_resize()
        except Exception:
            pass
        self.after(40, self._win_size_watch)

    def _after_window_resize(self):
        """Window resize finished: re-fit the sidebar once, then lay the clip
        list out once (only if its width actually changed)."""
        try:
            self._sync_sash_16_9()
        except Exception:
            pass
        self.after(40, self._finish_window_resize)

    def _finish_window_resize(self):
        if self._sash_drag_active():
            return
        job = getattr(self, "_resize_job", None)
        if job:
            try:
                self.after_cancel(job)
            except Exception:
                pass
            self._resize_job = None
        try:
            w = self._vc.winfo_width()
            self._panel_width = w
            if w != getattr(self, "_vg_canvas_w", None):
                self._vg_on_resize()
        except Exception:
            pass

    def _vg_on_resize(self, event=None):
        if self._sash_drag_active():
            return                       # wait for the sash / window edge to be released
        if (event is not None and self._ui_ready
                and event.width == getattr(self, "_vg_canvas_w", None)):
            return                       # only the height changed: nothing to re-layout
        self._vg_layout()
        self._vg_refresh()
        if not self._ui_ready:
            self._ui_ready = True
            self.after(50, self._vg_on_resize)

    def _render_grid(self):
        _display = list(self.videos)
        if self._filter_var == "Starred":
            _display = [v for v in _display if v in self._starred_clips]
        elif self._filter_var == "Liked":
            _display = [v for v in _display if v in self._liked_clips]
        elif self._filter_var == "Trimmed":
            _display = [v for v in _display if v in self._trim_enabled]
        _display = self._apply_date_filter(self._game_filter(_display))
        self._vg_thumb_cvs.clear(); self._vg_thumb_wins.clear()
        self._vg_thumb_sizes.clear(); self._vg_thumb_pos.clear()
        self._vc.delete("all")
        if not _display:
            self._vg_display = []; self._vg_total = 0
            msg = ("No starred clips" if self._filter_var == "Starred" else
                   "No liked clips" if self._filter_var == "Liked" else
                   "No trimmed clips" if self._filter_var == "Trimmed" else
                   "No videos found")
            if self._date_from:
                _a, _b = self._date_from, self._date_to
                msg = (f"No clips on {_a.strftime('%d.%m.%Y')}" if _a == _b else
                       f"No clips from {_a.strftime('%d.%m.%Y')} to {_b.strftime('%d.%m.%Y')}")
            self._vc.create_text(10, 30, text=msg, font=FONT_UI, fill=TEXT_MUTED, anchor="w")
            return
        self._vg_total   = len(_display)
        self._vg_display = _display[:self._visible_count]
        self._vg_hover_idx = None
        self._vg_layout()
        self._vg_refresh()

    def _apply_thumb(self, name, thumb_path):
        self._vg_blit_thumb(name, self._vg_card_w - CARD_BORDER * 2, self._vg_card_h)

    def _highlight_active_card(self):
        self._vg_refresh()

    # ── Virtual grid mouse handling ───────────────────────────────────────────

    def _vc_canvas_y(self, event_y):
        return int(self._vc.canvasy(event_y))

    def _vg_click(self, event):
        if self._ignore_next_click:
            self._ignore_next_click = False; return
        cy = self._vc_canvas_y(event.y)
        items = self._vc.find_overlapping(event.x, cy, event.x, cy)
        for item in items:
            tags = self._vc.gettags(item)
            if "see_more" in tags or "load_all" in tags: return
            for t in tags:
                if t.startswith("heart_"):
                    self._toggle_like_by_name(t[len("heart_"):]); return
                if t.startswith("trim_"):
                    self._toggle_trim_by_name(t[len("trim_"):]); return
        idx = self._vg_idx_at(event.x, cy)
        if idx is None: return
        name = self._vg_display[idx]
        self._play_index(self.videos.index(name))

    def _vg_rb_press(self, event):
        cy  = self._vc_canvas_y(event.y)
        idx = self._vg_idx_at(event.x, cy)
        if idx is None: return
        self._rb_dragging = True; self._rb_toggled = set()
        name = self._vg_display[idx]
        self._rb_toggled.add(name)
        if name in self._selected_clips: self._selected_clips.discard(name)
        else: self._selected_clips.add(name)
        self._update_delete_size_lbl(); self._vg_draw_card(idx)

    def _vg_rb_release(self, event):
        self._rb_dragging = False; self._rb_toggled = set()

    def _vg_motion(self, event):
        cy  = self._vc_canvas_y(event.y)
        idx = self._vg_idx_at(event.x, cy)
        if self._rb_dragging and idx is not None:
            name = self._vg_display[idx]
            if name not in self._rb_toggled:
                self._rb_toggled.add(name)
                if name in self._selected_clips: self._selected_clips.discard(name)
                else: self._selected_clips.add(name)
                self._update_delete_size_lbl(); self._vg_draw_card(idx)
        if idx != self._vg_hover_idx:
            old = self._vg_hover_idx; self._vg_hover_idx = idx
            if old is not None: self._vg_draw_card(old, hover=False)
            if idx is not None:
                self._vg_draw_card(idx, hover=True)
                try:
                    self._at_prefetch(self._vg_display[idx])   # probe audio tracks before the click
                except Exception:
                    pass

    def _vg_leave(self, event):
        if self._vg_hover_idx is not None:
            old = self._vg_hover_idx; self._vg_hover_idx = None
            self._vg_draw_card(old, hover=False)

    # ── Scroll ────────────────────────────────────────────────────────────────

    def _on_scroll(self, event):
        if self._settings_open:
            self._settings_scroll(event)
            return
        if self._game_open:
            wx, wy = self.winfo_pointerx(), self.winfo_pointery()
            px, py = self._game_popup.winfo_rootx(), self._game_popup.winfo_rooty()
            pw, ph = self._game_popup.winfo_width(), self._game_popup.winfo_height()
            if px <= wx <= px+pw and py <= wy <= py+ph: return
        if event.delta > 0 and self._vc.yview()[0] <= 0: return
        self._vc.yview_scroll(int(-1 * (event.delta / 120)), "units")

    # ── Star / heart / like ───────────────────────────────────────────────────

    def _toggle_star(self):
        if self.current_idx is None: return
        name = self.videos[self.current_idx]
        newly_starred = name not in self._starred_clips
        if newly_starred:
            self._starred_clips.add(name)
        else:
            self._starred_clips.discard(name)
        # Update the star icon and save prefs FIRST, so the click always
        # feels instant even though picking/validating the folder
        # below can pop up a blocking dialog.
        self.star_btn._starred = newly_starred
        self.star_btn._draw(starred=newly_starred)
        self._save_prefs()
        if newly_starred and self._star_copy_enabled:
            self._copy_starred_clip(name)

    # ── Copy starred clips to a folder (configurable in Settings) ─────────────

    def _choose_star_folder(self, force=False):
        """Return the folder starred clips are copied into, asking for it if
        none is set yet. force=True always opens the picker (right-click on
        the star button); the folder can also be changed in Settings."""
        if self._star_copy_folder and not force:
            if os.path.isdir(self._star_copy_folder):
                return self._star_copy_folder
            # Previously-picked folder is gone (renamed, drive unplugged, ...)
            # - ask again instead of silently failing every copy from here on.
            self._flash_star_status("Folder missing \u2014 choose it again", _c("#e05555"))
        # Make sure our own window has focus before opening the dialog so
        # it doesn't end up hidden behind the embedded video window.
        self.lift()
        self.focus_force()
        path = filedialog.askdirectory(
            title="Choose a folder for starred clips",
            parent=self,
            **({"initialdir": self._star_copy_folder}
               if os.path.isdir(self._star_copy_folder or "") else {}))
        if path:
            self._star_copy_folder = os.path.normpath(path)
            self._save_prefs()
            self._refresh_star_settings()
            self._flash_star_status(f"Starred clips folder: {os.path.basename(path)}", _c("#7ec97e"))
        return self._star_copy_folder or ""

    def _copy_starred_clip(self, src_path):
        """Copy a newly-starred clip into the starred-clips folder in the
        background. If the clip has an active trim, a trimmed copy is cut first
        (same lossless -ss/-to/-c copy approach as the Share dialog) and that's
        what gets copied instead of the full-length file."""
        dest_folder = self._choose_star_folder(force=False)
        if not dest_folder:
            return  # user cancelled the folder picker — skip silently

        is_trimmed = src_path in self._trim_enabled and src_path in self._trims
        trim_range = self._trims.get(src_path) if is_trimmed else None

        def _copy():
            tmp_trim_path = None
            try:
                fname = os.path.basename(src_path)
                actual_src = src_path

                if is_trimmed and trim_range and self._ffmpeg:
                    dur = get_video_duration(src_path, self._ffmpeg)
                    if dur > 0:
                        t_start = (trim_range[0] / 10000.0) * dur
                        t_end   = (trim_range[1] / 10000.0) * dur
                        if t_end > t_start + 0.05:
                            self.after(0, lambda: self._flash_star_status(
                                "Cutting trimmed clip…", _c("#c8c864")))
                            import tempfile
                            ext = os.path.splitext(src_path)[1] or ".mp4"
                            fd, tmp_trim_path = tempfile.mkstemp(suffix=ext)
                            os.close(fd)
                            args = [self._ffmpeg, "-y", "-ss", str(t_start), "-to", str(t_end),
                                    "-i", src_path, "-map", "0:v?", "-map", "0:a?",
                                    "-map_metadata", "0", "-c", "copy", tmp_trim_path]
                            proc = subprocess.run(args, stdout=subprocess.DEVNULL,
                                                  stderr=subprocess.DEVNULL,
                                                  creationflags=0x08000000)
                            if (proc.returncode == 0 and os.path.exists(tmp_trim_path)
                                    and os.path.getsize(tmp_trim_path) > 0):
                                actual_src = tmp_trim_path
                            else:
                                _debug_log("trim export for starred copy failed, copying full clip instead")

                dest_path = os.path.join(dest_folder, fname)
                if os.path.exists(dest_path):
                    self.after(0, lambda: self._flash_star_status(
                        f"Already in folder: {fname}", _c("#c8c864")))
                    return
                shutil.copy2(actual_src, dest_path)
                label = "Copied trimmed clip" if actual_src != src_path else "Copied"
                self.after(0, lambda: self._flash_star_status(
                    f"{label}: {fname}", _c("#7ec97e")))
            except Exception as e:
                _debug_log(f"starred copy failed: {e}")
                self.after(0, lambda: self._flash_star_status(
                    "Copy failed \u2014 see log", _c("#e05555")))
            finally:
                if tmp_trim_path and os.path.exists(tmp_trim_path):
                    try: os.remove(tmp_trim_path)
                    except Exception: pass

        threading.Thread(target=_copy, daemon=True).start()

    def _flash_star_status(self, text, color):
        """Small transient status message confirming what just happened with
        the starred-clip copy / trim. Shown in the title row above the
        playback bar, just left of the clip size. It is placed (not packed)
        so it never pushes the clip size or the title around."""
        row = self.now_playing_mb.master
        if not hasattr(self, "_star_status_lbl"):
            self._star_status_lbl = tk.Label(row, text="", bg=BG,
                                              font=FONT_TINY, anchor="e", padx=6)
        lbl = self._star_status_lbl
        lbl.configure(text=text, fg=color)
        try:
            row.update_idletasks()
            mb_x = self.now_playing_mb.winfo_x()
            if self.now_playing_mb.winfo_width() <= 1:
                mb_x = row.winfo_width()
        except Exception:
            mb_x = row.winfo_width()
        lbl.place(x=mb_x, rely=0.5, anchor="e")
        lbl.lift()
        self._star_status_after_id = getattr(self, "_star_status_after_id", None)
        if self._star_status_after_id:
            try: self.after_cancel(self._star_status_after_id)
            except Exception: pass
        self._star_status_after_id = self.after(3500, lbl.place_forget)

    def _update_star_btn(self):
        if self.current_idx is None:
            self.star_btn._starred = False; self.star_btn._draw(starred=False); return
        name = self.videos[self.current_idx]
        starred = name in self._starred_clips
        self.star_btn._starred = starred; self.star_btn._draw(starred=starred)

    def _toggle_like(self):
        if self.current_idx is None: return
        name = self.videos[self.current_idx]
        if name in self._liked_clips:
            self._liked_clips.discard(name); self.heart_btn._liked = False
        else:
            self._liked_clips.add(name); self.heart_btn._liked = True
        self.heart_btn._draw(liked=self.heart_btn._liked)
        self._save_prefs()
        try:
            idx = self._vg_display.index(name)
            self._vg_draw_card(idx, hover=(idx == self._vg_hover_idx))
            self._vg_blit_thumb(name)
        except ValueError: pass

    def _update_trim_mode_for_current(self):
        """Show/hide trim handles on playbar based on current clip's trim_enabled state."""
        if self.current_idx is None:
            self._show_apply_trim_btn(False)
            return
        name    = self.videos[self.current_idx]
        enabled = name in self._trim_enabled
        self._trim_mode = enabled
        self._update_trim_btn()
        self.progress._trim_visible = enabled
        if enabled:
            saved = self._trims.get(name, [0, 10000])
            self.progress.set_trim(saved[0], saved[1])
        else:
            self.progress.set_trim(0, 10000)
            self.progress._move_items()
        self._show_apply_trim_btn(enabled)

    def _show_apply_trim_btn(self, show):
        if show == getattr(self, "_apply_trim_shown", False):
            return
        self._apply_trim_shown = show
        if show:
            # right next to the action buttons (Delete, when visible, sits left of it)
            self._apply_trim_pill.pack(side="right", padx=(0, 8), after=self._hdr_actions)
        else:
            self._apply_trim_pill.pack_forget()

    def _toggle_trim_by_name(self, name):
        """Toggle trim-enabled state for a clip from its card button."""
        if name in self._trim_enabled:
            self._trim_enabled.discard(name)
            self._trims.pop(name, None)
        else:
            self._trim_enabled.add(name)
        self._save_prefs()
        if name in self._vg_display:
            idx = self._vg_display.index(name)
            self._vg_draw_card(idx)
            self._vc.tag_raise(f"heart_{name}")
        if (self.current_idx is not None and
                self.videos[self.current_idx] == name):
            self._update_trim_mode_for_current()
        if self._filter_var == "Trimmed":
            self._render_grid()
            threading.Thread(target=self._load_thumbnails, daemon=True).start()

    def _toggle_like_by_name(self, name):
        """Toggle liked state for a clip from its thumbnail heart button."""
        if name in self._liked_clips:
            self._liked_clips.discard(name)
        else:
            self._liked_clips.add(name)
        self._save_prefs()
        if (self.current_idx is not None and len(self.videos) > self.current_idx
                and self.videos[self.current_idx] == name):
            self.heart_btn._liked = name in self._liked_clips
            self.heart_btn._draw(liked=self.heart_btn._liked)
        try:
            idx = self._vg_display.index(name)
            self._vg_draw_card(idx, hover=(idx == self._vg_hover_idx))
        except ValueError:
            pass

    def _update_heart_btn(self):
        if self.current_idx is None:
            self.heart_btn._liked = False; self.heart_btn._draw(liked=False); return
        name = self.videos[self.current_idx]
        liked = name in self._liked_clips
        self.heart_btn._liked = liked; self.heart_btn._draw(liked=liked)

    # ── Prefs ─────────────────────────────────────────────────────────────────

    def _prefs_path(self):
        return os.path.join(_DATA_DIR, ".clipplayer_prefs.json")

    def _prefs_backup_dir(self):
        d = os.path.join(_DATA_DIR, "backups")
        os.makedirs(d, exist_ok=True)
        return d

    def _newest_backup_path(self):
        """Return the most recent backup file path, or None if there are none."""
        try:
            backups = [os.path.join(self._prefs_backup_dir(), f)
                       for f in os.listdir(self._prefs_backup_dir())
                       if f.startswith("prefs_") and f.endswith(".json")]
            if not backups:
                return None
            return max(backups, key=os.path.getmtime)
        except Exception:
            return None

    def _load_prefs_data(self):
        # Try the main prefs file first.
        try:
            with open(self._prefs_path(), "r", encoding="utf-8") as f:
                data = json.load(f)
            if isinstance(data, dict) and data:
                return data
        except Exception:
            pass
        # Main file missing / unreadable / empty -> fall back to the newest
        # backup rather than silently returning {} (which looks like "no
        # data" and would let the next save wipe everything for real).
        newest = self._newest_backup_path()
        if newest:
            try:
                with open(newest, "r", encoding="utf-8") as f:
                    data = json.load(f)
                if isinstance(data, dict):
                    return data
            except Exception:
                pass
        return {}

    # Keys whose data we specifically don't want to lose to an accidental
    # wipe (starred/liked/category data etc). If ALL of these look empty in
    # a new save while the file on disk currently has real data in them,
    # treat the save as suspicious rather than destructive.
    _PROTECTED_PREF_KEYS = ("starred", "liked", "category_renames",
                             "keyword_groups", "trims", "trim_enabled")

    def _looks_like_accidental_wipe(self, new_data):
        try:
            with open(self._prefs_path(), "r", encoding="utf-8") as f:
                old_data = json.load(f)
        except Exception:
            return False  # nothing on disk yet, nothing to protect
        if not isinstance(old_data, dict):
            return False
        old_has_data = any(old_data.get(k) for k in self._PROTECTED_PREF_KEYS)
        new_has_data = any(new_data.get(k) for k in self._PROTECTED_PREF_KEYS)
        return old_has_data and not new_has_data

    def _backup_current_prefs(self):
        """Snapshot the existing prefs file (before it gets overwritten) into
        the backups folder, then trim old snapshots down to the most recent
        30."""
        src = self._prefs_path()
        if not os.path.exists(src):
            return
        try:
            stamp = datetime.datetime.now().strftime("%Y%m%d_%H%M%S")
            dst = os.path.join(self._prefs_backup_dir(), f"prefs_{stamp}.json")
            shutil.copy2(src, dst)
            backups = sorted(
                (os.path.join(self._prefs_backup_dir(), f)
                 for f in os.listdir(self._prefs_backup_dir())
                 if f.startswith("prefs_") and f.endswith(".json")),
                key=os.path.getmtime,
            )
            for old in backups[:-30]:
                try:
                    os.remove(old)
                except Exception:
                    pass
        except Exception:
            pass

    def _save_prefs(self):
        data = {
            "folder":          self.folder_path or "",
            "saved_folders":   list(self._saved_folders),
            "checked_folders": list(self._checked_folders),
            "active_folder":   self._active_folder or "",
            "volume":       int(self.vol_slider.get()),
            "starred":      list(self._starred_clips),
            "liked":        list(self._liked_clips),
            "star_copy_folder":  self._star_copy_folder,
            "star_copy_enabled": self._star_copy_enabled,
            "durations":    self._durations,
            "trims":        self._trims,
            "trim_enabled": list(self._trim_enabled),
            "game_filter":  self._game_var or "All",
            "game_multi":   sorted(self._game_multi),
            "category_renames": self._category_renames,
            "keyword_groups": self._keyword_groups,
        }

        # Always snapshot what's currently on disk before touching it, so a
        # bad write can be recovered from the backups folder.
        self._backup_current_prefs()

        # If this save would wipe out data that's currently on disk (e.g. a
        # bug cleared self._starred_clips etc before this call), don't let
        # it overwrite the real file -- just keep the pre-write backup and
        # bail out.
        if self._looks_like_accidental_wipe(data):
            return

        try:
            with open(self._prefs_path(), "w", encoding="utf-8") as f:
                json.dump(data, f)
        except Exception:
            pass

    # ── Helpers ───────────────────────────────────────────────────────────────

    def _rb_drag_reset(self):
        self._rb_dragging = False; self._rb_toggled = set()

    def _update_delete_size_lbl(self):
        """Show the number of selected clips next to the trash icon (as soon
        as one or more are selected). Name kept: many places call it."""
        n = len(self._selected_clips)
        new = str(n) if n >= 1 else ""
        btn, pill = self._delete_btn, self._hdr_actions
        if btn._text == new:
            return
        btn.configure(text=new)          # button width changes right away...
        try:
            # ...so settle the pill's size in the same step, before anything is
            # painted. Otherwise the buttons sit shifted for a moment while the
            # pill catches up, which looked like the icons teleporting.
            pill.inner.update_idletasks()
            pill._fit()
            pill.update_idletasks()
            btn._draw()
        except Exception:
            pass

    def _delete_selected_btn(self):
        if not self._delete_targets():
            self._flash_star_status("Open or select a clip to delete first", _c("#c8c864"))
            return
        self._delete_selected()

    # ── Custom title bar ──────────────────────────────────────────────────
    _WS_CAPTION, _WS_THICKFRAME = 0x00C00000, 0x00040000
    _WS_MINBOX, _WS_MAXBOX, _WS_SYSMENU = 0x00020000, 0x00010000, 0x00080000

    def _build_titlebar(self):
        """Themed in-app title bar (replaces the native Windows one)."""
        self._maximized = False
        self._fullscreen = False
        self._normal_geo = None
        self._drag = None
        self._tb_styled = False

        self._titlebar = tk.Frame(self, bg=BG, height=37)
        self._titlebar.pack(side="top", fill="x")
        self._titlebar.pack_propagate(False)

        self._tb_icon = tk.Label(self._titlebar, bg=BG, bd=0)
        self._tb_icon.pack(side="left", padx=(12, 6))
        self._tb_title = tk.Label(self._titlebar, text="Feather Player", font=FONT_TITLE,
                                  bg=BG, fg=TEXT_PRIMARY)
        self._tb_title.place(relx=0.5, rely=0.5, anchor="center")

        self._tb_close = TitleButton(self._titlebar, "close", self.on_close, danger=True)
        self._tb_close.pack(side="right", padx=(0, 6))
        self._tb_max = TitleButton(self._titlebar, "max", self._toggle_maximize)
        self._tb_max.pack(side="right", padx=(0, 2))
        self._tb_min = TitleButton(self._titlebar, "min", self._minimize)
        self._tb_min.pack(side="right", padx=(0, 2))

        # Accent-coloured line under the bar (follows the theme's accent)
        self._tb_line = tk.Frame(self, bg=ACCENT, height=2)
        self._tb_line.pack(side="top", fill="x")

        for w in (self._titlebar, self._tb_icon, self._tb_title):
            w.bind("<ButtonPress-1>",   self._tb_press)
            w.bind("<B1-Motion>",       self._tb_motion)
            w.bind("<ButtonRelease-1>", lambda e: setattr(self, "_drag", None))
            w.bind("<Double-Button-1>", lambda e: self._toggle_maximize())

        # Logo, top-left (the app's feather image; a drawn feather if the file isn't found)
        self._logo_key = None
        self._set_titlebar_logo()

        self._build_resize_strips()

        self.update_idletasks()
        self.after(30, self._strip_native_caption)

    # Thin invisible strips along the very top of the bar that start Windows' own
    # resize loop (top edge + both top corners). The title bar's Tk widgets cover
    # the whole top of the window, so Windows never sees the mouse over a resize
    # border there - these strips take that job.
    _RS_T, _RS_C = 5, 10      # strip thickness, corner width (px)

    def _build_resize_strips(self):
        T, C = self._RS_T, self._RS_C
        self._rs = []

        def mk(cursor, ht, **pl):
            f = tk.Frame(self, bg=BG, height=T, bd=0, highlightthickness=0, cursor=cursor)
            f._pl = dict(y=0, height=T, **pl)
            f.place(**f._pl)
            f.bind("<ButtonPress-1>", lambda e, h=ht: self._native_resize(h))
            self._rs.append(f)

        mk("size_nw_se", 13, x=0, width=C)                           # HTTOPLEFT
        mk("size_ne_sw", 14, relx=1.0, x=-C, width=C)                # HTTOPRIGHT
        mk("sb_v_double_arrow", 12, x=C, relwidth=1.0, width=-2 * C) # HTTOP
        self._sync_resize_strips()

    def _sync_resize_strips(self):
        """Strips are only active while the window is not (fake-)maximized."""
        for f in getattr(self, "_rs", []):
            try:
                if self._maximized:
                    f.place_forget()
                else:
                    f.place(**f._pl)
                    f.lift()
            except Exception:
                pass

    def _native_resize(self, ht):
        """Start Windows' own sizing loop for the given hit-test code. Posted
        (not sent), same as the title-bar drag, so it runs from the normal
        message pump and not nested inside a Tk event handler."""
        if self._maximized:
            return
        try:
            from ctypes import wintypes
            u = ctypes.windll.user32
            pt = wintypes.POINT()
            u.GetCursorPos(ctypes.byref(pt))
            u.PostMessageW.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_size_t, ctypes.c_ssize_t]
            u.PostMessageW.restype = ctypes.c_int
            lparam = (pt.x & 0xFFFF) | ((pt.y & 0xFFFF) << 16)
            u.ReleaseCapture()
            u.PostMessageW(ctypes.c_void_p(self._hwnd()), 0x00A1, ht, lparam)   # WM_NCLBUTTONDOWN
        except Exception:
            pass

    _LOGO_SIZE = 22

    def _find_logo_file(self):
        """The title bar logo: pics/feather.png (next to the script / bundled),
        or the old pics/feather2.png if feather.png isn't there."""
        for _name in ("feather.png", "feather2.png"):
            path = resource_path(os.path.join("pics", _name))
            if os.path.isfile(path):
                return path
        return None

    def _set_titlebar_logo(self):
        """Show pics/feather.png at the left of the title bar. If the file isn't
        there the spot stays blank (no placeholder is ever drawn).
        The big, thin-stroked source is thickened a little and shrunk in a
        premultiplied-alpha pipeline so the small logo stays crisp."""
        size = self._LOGO_SIZE
        path = self._find_logo_file()
        if path is not None and path == getattr(self, "_logo_key", None):
            return
        try:
            if path is None:
                self._tb_icon.configure(image="")
                self._tb_icon_photo = None
                self._logo_key = None
                return
            from PIL import Image, ImageTk, ImageFilter
            import numpy as np

            src = Image.open(path).convert("RGBA")
            bbox = src.getchannel("A").getbbox()
            if bbox:
                src = src.crop(bbox)                              # trim empty margin
            side = max(src.size)
            sq = Image.new("RGBA", (side, side), (0, 0, 0, 0))    # square canvas, keeps aspect
            sq.paste(src, ((side - src.width) // 2, (side - src.height) // 2))

            # thicken the thin outline so it survives shrinking
            a = sq.getchannel("A").filter(ImageFilter.MaxFilter(9))
            sq.putalpha(a)

            # shrink with premultiplied alpha (no dark/fuzzy fringes)
            big = size * 4
            sq = sq.resize((big, big), Image.LANCZOS)
            arr = np.asarray(sq).astype(np.float32) / 255.0
            arr[..., :3] *= arr[..., 3:4]
            pm = Image.fromarray((arr * 255).astype(np.uint8))
            pm = pm.resize((size, size), Image.BOX)
            out = np.asarray(pm).astype(np.float32) / 255.0
            al = out[..., 3:4]
            out[..., :3] = np.where(al > 0, out[..., :3] / np.maximum(al, 1e-6), 0)
            img = Image.fromarray((np.clip(out, 0, 1) * 255).astype(np.uint8))

            self._tb_icon_photo = ImageTk.PhotoImage(img)
            self._tb_icon.configure(image=self._tb_icon_photo)
            self._logo_key = path
        except Exception:
            try:
                self._tb_icon.configure(image="")
            except Exception:
                pass

    def _hwnd(self):
        return ctypes.windll.user32.GetParent(self.winfo_id())

    # ── 16:9 video area ───────────────────────────────────────────────────
    _VIDEO_AR = 16 / 9
    SIDEBAR_MAX_W = 440      # the clip sidebar can never be wider than this (px)

    def _ar_measure(self):
        """(fixed_w, fixed_h): window size minus the video area, i.e. everything
        that is not the video. None when the video area isn't on screen."""
        try:
            from ctypes import wintypes
            r = wintypes.RECT()
            ctypes.windll.user32.GetWindowRect(ctypes.c_void_p(self._hwnd()), ctypes.byref(r))
            cw, ch = self.video_canvas.winfo_width(), self.video_canvas.winfo_height()
            if cw < 50 or ch < 50 or not self.video_canvas.winfo_ismapped():
                return None
            fw, fh = (r.right - r.left) - cw, (r.bottom - r.top) - ch
            self._ar_min_vw = max(400, 860 - fw)
            return fw, fh
        except Exception:
            return None

    def _clamp_sidebar_width(self, event=None):
        """Stop the clip sidebar from being dragged (or stretched) wider than SIDEBAR_MAX_W."""
        try:
            if event is None or event.width <= self.SIDEBAR_MAX_W:
                return
            pane_w = self._pane.winfo_width()
            sash_w = int(self._pane.cget("sashwidth"))
            self._pane.sash_place(0, pane_w - self.SIDEBAR_MAX_W - sash_w, 0)
        except Exception:
            pass

    def _queue_sash_sync(self, event=None):
        """Video canvas changed size -> (debounced) re-fit the sidebar width."""
        if getattr(self, "_win_sizing", False):
            return                      # window being dragged: fitted once on release
        job = getattr(self, "_sash_job", None)
        if job is not None:
            try:
                self.after_cancel(job)
            except Exception:
                pass
        self._sash_job = self.after(10, self._sync_sash_16_9)

    def _sync_sash_16_9(self):
        """Make the video area exactly 16:9 by giving the clip sidebar whatever
        width is left over (instead of letting mpv draw black bars).

        The video height is fixed by the window height, so the left panel
        should be height * 16/9 wide and the right sidebar takes the rest.
        Only runs when the window height or total width changed, so dragging
        the divider by hand still works until the window is resized again."""
        self._sash_job = None
        if getattr(self, "_win_sizing", False):
            return
        try:
            if self._settings_open or not self.video_canvas.winfo_ismapped():
                return
            pane_w = self._pane.winfo_width()
            vh = self.video_canvas.winfo_height()
            if pane_w < 100 or vh < 50:
                return
            # Key on the WINDOW size, not the video canvas height: the canvas
            # also changes by a few px when the header/controls change (e.g. the
            # Delete button appearing on selection), which used to move the
            # sidebar and make the clip list re-layout ("warp").
            state = (self.winfo_width(), self.winfo_height())
            if state == getattr(self, "_sash_last_state", None):
                return          # window didn't change: leave the divider alone
            if not getattr(self, "_sash_armed", False):
                # Startup: leave the layout exactly as it was, just remember it.
                self._sash_last_state = state
                return
            self._sash_last_state = state

            sash_w = int(self._pane.cget("sashwidth"))
            left_min, right_min = 400, 220          # same as the pane's minsize values
            want = int(round(vh * self._VIDEO_AR))   # left width for a 16:9 video
            want = max(want, pane_w - self.SIDEBAR_MAX_W - sash_w)     # sidebar max width
            want = max(left_min, min(want, pane_w - right_min - sash_w))
            cur = self._pane.sash_coord(0)[0]
            if abs(cur - want) > 1:
                self._pane.sash_place(0, want, 0)
        except Exception:
            pass

    def _fit_video_16_9(self, tries=0):
        """One-off at startup: set the window height so the video area is 16:9."""
        try:
            from ctypes import wintypes
            if self.state() != "normal" or getattr(self, "_maximized", False) or self._settings_open:
                return
            if self._remember_size and self._saved_win:
                return                      # the remembered size wins over the 16:9 auto-fit
            fixed = self._ar_measure() if self.winfo_viewable() else None
            if not fixed:
                if tries < 10:
                    self.after(200, lambda: self._fit_video_16_9(tries + 1))
                return
            fw, fh = fixed
            r = wintypes.RECT()
            ctypes.windll.user32.GetWindowRect(ctypes.c_void_p(self._hwnd()), ctypes.byref(r))
            target_h = int(round(self.video_canvas.winfo_width() / self._VIDEO_AR)) + fh
            if abs(target_h - (r.bottom - r.top)) > 1:
                self._set_outer_rect(r.left, r.top, r.right - r.left, target_h)
        except Exception:
            pass

    def _install_frame_hook(self):
        """Sub-class the window procedure so the client area starts at the very top
        of the window. Without this Windows keeps ~5px of (black) frame above the
        title bar. The left/right/bottom resize borders are left alone, and a
        4px strip at the top still resizes the window (WM_NCHITTEST)."""
        if getattr(self, "_tb_hooked", False) and getattr(self, "_tb_hwnd", None) == self._hwnd():
            return True
        try:
            from ctypes import wintypes
            u = ctypes.windll.user32
            hwnd = self._hwnd()
            get = getattr(u, "GetWindowLongPtrW", u.GetWindowLongW)
            setw = getattr(u, "SetWindowLongPtrW", u.SetWindowLongW)
            get.restype, get.argtypes = ctypes.c_ssize_t, [ctypes.c_void_p, ctypes.c_int]
            setw.restype = ctypes.c_ssize_t
            setw.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_ssize_t]
            call = u.CallWindowProcW
            call.restype = ctypes.c_ssize_t
            call.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_uint,
                             ctypes.c_size_t, ctypes.c_ssize_t]
            u.IsZoomed.argtypes = [ctypes.c_void_p]
            u.IsIconic.argtypes = [ctypes.c_void_p]
            u.GetWindowRect.argtypes = [ctypes.c_void_p, ctypes.POINTER(wintypes.RECT)]
            old = get(hwnd, -4)                       # GWLP_WNDPROC

            class NCCALCSIZE_PARAMS(ctypes.Structure):
                _fields_ = [("rgrc", wintypes.RECT * 3), ("lppos", ctypes.c_void_p)]

            WNDPROC = ctypes.WINFUNCTYPE(ctypes.c_ssize_t, ctypes.c_void_p, ctypes.c_uint,
                                         ctypes.c_size_t, ctypes.c_ssize_t)

            def proc(h, msg, wp, lp):
                _trace("wm 0x%04X wp=%s lp=%s" % (msg, wp, lp))
                # Step 2: while minimizing / minimized, pass everything straight to Tk
                if getattr(self, "_minimizing", False) or u.IsIconic(h):
                    return call(old, h, msg, wp, lp)
                # Window being resized by the mouse: remember it, so the clip list /
                # sidebar are NOT re-laid out every frame (only once on release).
                if msg == 0x0231:                                       # WM_ENTERSIZEMOVE
                    self._win_sizing = True
                elif msg == 0x0232:                                     # WM_EXITSIZEMOVE
                    self._win_sizing = False
                    self._win_size_pending = True
                try:
                    if msg == 0x0083 and wp and not u.IsZoomed(h):      # WM_NCCALCSIZE
                        prm = ctypes.cast(lp, ctypes.POINTER(NCCALCSIZE_PARAMS)).contents
                        top = prm.rgrc[0].top
                        res = call(old, h, msg, wp, lp)
                        prm.rgrc[0].top = top                          # no frame above the bar
                        return res
                    if msg == 0x0085:                                   # WM_NCPAINT
                        # Windows would draw its thick frame (white + grey bands)
                        # over the top of our title bar, because we gave that
                        # strip back to the client area. Never let it paint.
                        return 0
                    if msg == 0x0086:                                   # WM_NCACTIVATE
                        # lParam = -1 -> update active state without repainting the frame
                        return call(old, h, msg, wp, -1)
                    if msg == 0x0084:                                   # WM_NCHITTEST
                        res = call(old, h, msg, wp, lp)
                        if res == 1 and not u.IsZoomed(h):              # HTCLIENT
                            x = ctypes.c_short(lp & 0xFFFF).value
                            y = ctypes.c_short((lp >> 16) & 0xFFFF).value
                            r = wintypes.RECT()
                            u.GetWindowRect(h, ctypes.byref(r))
                            if y - r.top < 4:
                                if x - r.left < 12:
                                    return 13                           # HTTOPLEFT
                                if r.right - x < 12:
                                    return 14                           # HTTOPRIGHT
                                return 12                               # HTTOP
                        return res
                except Exception:
                    pass
                return call(old, h, msg, wp, lp)

            self._tb_proc = WNDPROC(proc)             # keep a reference alive!
            self._tb_old_proc = old
            setw(hwnd, -4, ctypes.cast(self._tb_proc, ctypes.c_void_p).value)
            self._tb_hwnd = hwnd
            self._tb_hooked = True
        except Exception:
            self._tb_hooked = False
        return self._tb_hooked

    def _strip_native_caption(self, attempt=0):
        """Remove the native caption but keep the resize frame, so Aero snap,
        edge-resizing, minimise animation and the taskbar button all still work.
        Verifies the change and retries (Tk can rebuild the frame on some calls)."""
        ok = False
        try:
            u = ctypes.windll.user32
            hwnd = self._hwnd()
            get = getattr(u, "GetWindowLongPtrW", u.GetWindowLongW)
            setw = getattr(u, "SetWindowLongPtrW", u.SetWindowLongW)
            get.restype = ctypes.c_ssize_t
            setw.restype = ctypes.c_ssize_t
            get.argtypes = [ctypes.c_void_p, ctypes.c_int]
            setw.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_ssize_t]
            changed = False
            style = get(hwnd, -16)
            if style & self._WS_CAPTION:
                style &= ~self._WS_CAPTION
                style |= (self._WS_THICKFRAME | self._WS_MINBOX | self._WS_MAXBOX | self._WS_SYSMENU)
                setw(hwnd, -16, style)
                changed = True
            if not (getattr(self, "_tb_hooked", False) and getattr(self, "_tb_hwnd", None) == hwnd):
                changed = self._install_frame_hook() or changed
            if changed:
                # SWP_NOMOVE|NOSIZE|NOZORDER|NOACTIVATE|FRAMECHANGED -> re-run WM_NCCALCSIZE
                u.SetWindowPos(hwnd, 0, 0, 0, 0, 0, 0x0002 | 0x0001 | 0x0004 | 0x0010 | 0x0020)
            ok = not (get(hwnd, -16) & self._WS_CAPTION)
        except Exception:
            ok = False
        self._tb_styled = ok
        self._apply_dark_titlebar()
        if ok:
            self.after(120, self._refresh_frame)
        if not ok and attempt < 5:
            self.after(250, lambda: self._strip_native_caption(attempt + 1))
        elif not getattr(self, "_tb_map_bound", False):
            self._tb_map_bound = True
            # Re-check whenever the window is shown again (e.g. restored from tray)
            self.bind("<Map>", lambda e: self.after(20, self._strip_native_caption)
                      if e.widget is self else None, add="+")

    def _refresh_frame(self):
        """Force Windows to re-run the frame calculation and repaint the
        non-client area. After the native caption is stripped (and the window is
        resized at startup) a leftover strip of the old frame can stay painted
        until something repaints it - changing the theme used to do exactly that."""
        try:
            if not self.winfo_viewable():
                return
            u = ctypes.windll.user32
            hwnd = ctypes.c_void_p(self._hwnd())
            u.SetWindowPos.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_int, ctypes.c_int,
                                       ctypes.c_int, ctypes.c_int, ctypes.c_uint]
            u.RedrawWindow.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_uint]
            # SWP_NOMOVE|NOSIZE|NOZORDER|NOACTIVATE|FRAMECHANGED
            u.SetWindowPos(hwnd, None, 0, 0, 0, 0, 0x0002 | 0x0001 | 0x0004 | 0x0010 | 0x0020)
            self._apply_dark_titlebar()
            # RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW  (no RDW_FRAME: the
            # native frame must never be painted over our custom title bar)
            u.RedrawWindow(hwnd, None, None, 0x0001 | 0x0080 | 0x0100)
        except Exception:
            pass

    def _apply_dark_titlebar(self):
        """Dark mode + theme-coloured caption/border (the caption colour is the
        fallback if the native bar is ever shown; the border colour tints the
        thin window outline on Windows 11)."""
        try:
            self._set_titlebar_logo()
        except Exception:
            pass
        try:
            hwnd  = self._hwnd()
            value = ctypes.c_int(1)
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 20, ctypes.byref(value), ctypes.sizeof(value))
            # Caption colour (only a fallback, the native bar is hidden)
            r, g, b = _hex_to_rgb255(BG)
            color = ctypes.c_int(r | (g << 8) | (b << 16))
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 35, ctypes.byref(color), ctypes.sizeof(color))
            # Rounded window corners (Windows 11): ask for them explicitly
            # (DWMWCP_ROUND = 2), so they do not depend on the frame setup.
            corner = ctypes.c_int(2)
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 33, ctypes.byref(corner), ctypes.sizeof(corner))
            # Window outline: same colour as the app background, so it is invisible
            # but still lets Windows draw the rounded corner edge.
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 34, ctypes.byref(color), ctypes.sizeof(color))
        except Exception:
            pass

    def _minimize(self):
        # Run the minimize AFTER the button's click handler has returned. Doing it
        # inside the handler made Windows send the minimize messages straight back
        # into the frame hook (_install_frame_hook) while Tk was still mid-event,
        # which crashed the whole process.
        self.after(30, self._do_minimize)

    def _do_minimize(self):
        # WHY PostMessage and not ShowWindow: calling ShowWindow(SW_MINIMIZE) from
        # inside a Tk "after" callback makes Windows send the minimize messages
        # synchronously into Tk's window procedure while Tcl is NOT inside one of its
        # own calls. Tk then runs a Python-bound Tcl command right there and
        # _tkinter has no valid thread state for it -> "PyEval_RestoreThread ...
        # thread state is NULL" -> Python aborts the whole process (0xc0000409).
        # Posting WM_SYSCOMMAND/SC_MINIMIZE instead lets the message be delivered
        # from Tk's normal event loop, the same way every other window message is.
        global _trace_until
        _trace_until = time.monotonic() + 8
        _trace("--- minimize: posting SC_MINIMIZE")
        self._minimizing = True
        try:
            u = ctypes.windll.user32
            u.PostMessageW.argtypes = [ctypes.c_void_p, ctypes.c_uint,
                                       ctypes.c_size_t, ctypes.c_ssize_t]
            u.PostMessageW.restype = ctypes.c_int
            if not u.PostMessageW(ctypes.c_void_p(self._hwnd()), 0x0112, 0xF020, 0):
                raise OSError("PostMessageW failed")
        except Exception:
            try:
                self.iconify()
            except Exception:
                pass
        # keep the frame hook in pass-through mode until the minimize has settled
        self.after(800, lambda: setattr(self, "_minimizing", False))

    def _work_area(self):
        """Usable screen rectangle (excludes the taskbar) of the monitor the window is on."""
        try:
            from ctypes import wintypes
            class MONITORINFO(ctypes.Structure):
                _fields_ = [("cbSize", wintypes.DWORD), ("rcMonitor", wintypes.RECT),
                            ("rcWork", wintypes.RECT), ("dwFlags", wintypes.DWORD)]
            u = ctypes.windll.user32
            u.MonitorFromWindow.restype = ctypes.c_void_p
            hmon = u.MonitorFromWindow(ctypes.c_void_p(self._hwnd()), 2)  # MONITOR_DEFAULTTONEAREST
            mi = MONITORINFO(); mi.cbSize = ctypes.sizeof(MONITORINFO)
            u.GetMonitorInfoW(ctypes.c_void_p(hmon), ctypes.byref(mi))
            r = mi.rcWork
            return r.left, r.top, r.right - r.left, r.bottom - r.top
        except Exception:
            return 0, 0, self.winfo_screenwidth(), self.winfo_screenheight() - 40

    def _frame_insets(self):
        """Invisible-border sizes (left, top, right, bottom) between the window
        rect and the visible frame, so a 'maximised' window lines up exactly."""
        try:
            from ctypes import wintypes
            hwnd = ctypes.c_void_p(self._hwnd())
            r, e = wintypes.RECT(), wintypes.RECT()
            ctypes.windll.user32.GetWindowRect(hwnd, ctypes.byref(r))
            if ctypes.windll.dwmapi.DwmGetWindowAttribute(hwnd, 9, ctypes.byref(e), ctypes.sizeof(e)) != 0:
                return 0, 0, 0, 0
            return e.left - r.left, e.top - r.top, r.right - e.right, r.bottom - e.bottom
        except Exception:
            return 0, 0, 0, 0

    def _set_outer_rect(self, x, y, w, h):
        # SWP_NOZORDER | SWP_NOACTIVATE
        ctypes.windll.user32.SetWindowPos(ctypes.c_void_p(self._hwnd()), None,
                                          int(x), int(y), int(w), int(h), 0x0004 | 0x0010)

    def _toggle_maximize(self):
        if self._maximized:
            self._restore_window_size()
            return
        from ctypes import wintypes
        r = wintypes.RECT()
        ctypes.windll.user32.GetWindowRect(ctypes.c_void_p(self._hwnd()), ctypes.byref(r))
        self._normal_rect = (r.left, r.top, r.right - r.left, r.bottom - r.top)
        il, it, ir, ib = self._frame_insets()
        x, y, w, h = self._work_area()
        self._set_outer_rect(x - il, y - it, w + il + ir, h + it + ib)
        self._maximized = True
        self._tb_max.set_kind("restore")
        self._sync_resize_strips()

    def _restore_window_size(self, at_cursor=None):
        self._maximized = False
        self._tb_max.set_kind("max")
        self._sync_resize_strips()
        x, y, w, h = getattr(self, "_normal_rect", None) or (100, 100, 1160, 760)
        if at_cursor:
            x = at_cursor[0] - w // 2
            y = max(at_cursor[1] - 17, 0)
        self._set_outer_rect(x, y, w, h)

    # ── Fullscreen ────────────────────────────────────────────────────────
    # The video is NOT scaled up from the windowed picture: the title bar, header,
    # controls and clip sidebar are taken out of the layout, the window grows to
    # cover the whole monitor, and the canvas (and with it mpv's render surface)
    # resizes to the real screen resolution. mpv then renders each frame straight
    # at that size, so nothing is stretched or re-encoded.
    def _monitor_rect(self):
        """Full rectangle (taskbar included) of the monitor the window is on."""
        try:
            from ctypes import wintypes
            class MONITORINFO(ctypes.Structure):
                _fields_ = [("cbSize", wintypes.DWORD), ("rcMonitor", wintypes.RECT),
                            ("rcWork", wintypes.RECT), ("dwFlags", wintypes.DWORD)]
            u = ctypes.windll.user32
            u.MonitorFromWindow.restype = ctypes.c_void_p
            hmon = u.MonitorFromWindow(ctypes.c_void_p(self._hwnd()), 2)  # NEAREST
            mi = MONITORINFO(); mi.cbSize = ctypes.sizeof(MONITORINFO)
            u.GetMonitorInfoW(ctypes.c_void_p(hmon), ctypes.byref(mi))
            r = mi.rcMonitor
            return r.left, r.top, r.right - r.left, r.bottom - r.top
        except Exception:
            return 0, 0, self.winfo_screenwidth(), self.winfo_screenheight()

    def _toggle_fullscreen(self):
        if self._fullscreen:
            self._exit_fullscreen()
        else:
            self._enter_fullscreen()

    def _fs_double_click(self):
        if self._fullscreen:
            self._toggle_play()          # undo the first click's play/pause
            self._exit_fullscreen()

    def _fullscreen_escape(self, event=None):
        if self._fullscreen:
            self._exit_fullscreen()

    def _enter_fullscreen(self):
        if self._fullscreen or self._settings_open:
            return
        try:
            from ctypes import wintypes
            u = ctypes.windll.user32
            if u.IsIconic(ctypes.c_void_p(self._hwnd())):
                return
            r = wintypes.RECT()
            u.GetWindowRect(ctypes.c_void_p(self._hwnd()), ctypes.byref(r))
            prev = dict(rect=(r.left, r.top, r.right - r.left, r.bottom - r.top),
                        sash=None, sash_state=getattr(self, "_sash_last_state", None))
            try:
                prev["sash"] = self._pane.sash_coord(0)[0]
            except Exception:
                pass
            self._fs_prev = prev
            self._fullscreen = True
            # everything except the video leaves the layout
            self._tb_line.pack_forget()
            self._titlebar.pack_forget()
            self._hdr_frame.pack_forget()
            self._controls_frame.pack_forget()
            self._pane.forget(self._right_panel)
            for f in getattr(self, "_rs", []):
                f.place_forget()
            # cover the whole monitor (taskbar included)
            mx, my, mw, mh = self._monitor_rect()
            il, it, ir, ib = self._frame_insets()
            self._set_outer_rect(mx - il, my - it, mw + il + ir, mh + it + ib)
            try:
                self.attributes("-topmost", True)
            except Exception:
                pass
            self.fullscreen_btn._draw()
        except Exception as e:
            _debug_log(f"enter fullscreen failed: {e}")
            self._exit_fullscreen()

    def _exit_fullscreen(self):
        if not self._fullscreen:
            return
        self._fullscreen = False
        prev = getattr(self, "_fs_prev", None) or {}
        try:
            self.attributes("-topmost", False)
        except Exception:
            pass
        try:
            self._titlebar.pack(side="top", fill="x", before=self._pane)
            self._tb_line.pack(side="top", fill="x", before=self._pane)
            self._hdr_frame.pack(fill="x", before=self.video_frame)
            self._controls_frame.pack(fill="x")
            self._pane.add(self._right_panel, minsize=220, width=300, stretch="never")
            if prev.get("rect"):
                self._set_outer_rect(*prev["rect"])
            self._sync_resize_strips()
            self.fullscreen_btn._draw()
        except Exception as e:
            _debug_log(f"exit fullscreen failed: {e}")
        self.after(80, lambda: self._fs_restore_sash(prev.get("sash")))

    def _fs_restore_sash(self, sash):
        """Put the clip-sidebar divider back where it was before fullscreen."""
        try:
            self.update_idletasks()
            if sash is not None:
                self._pane.sash_place(0, int(sash), 0)
            self._sash_last_state = (self.winfo_width(), self.winfo_height())
        except Exception:
            pass

    # Drag the window by the bar using Windows' own move loop (smooth, with
    # Aero snap). The loop is started by *posting* WM_NCLBUTTONDOWN/HTCAPTION to
    # the window, so it runs from the normal message pump - exactly like a real
    # title-bar drag - instead of nested inside a Tk event handler (which is what
    # crashed before). Started on first motion so double-click still maximises.
    def _win_rect(self):
        from ctypes import wintypes
        r = wintypes.RECT()
        ctypes.windll.user32.GetWindowRect(ctypes.c_void_p(self._hwnd()), ctypes.byref(r))
        return r.left, r.top

    def _tb_press(self, e):
        self._drag = (e.x_root, e.y_root)

    def _native_drag(self):
        from ctypes import wintypes
        u = ctypes.windll.user32
        pt = wintypes.POINT()
        u.GetCursorPos(ctypes.byref(pt))
        u.PostMessageW.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_size_t, ctypes.c_ssize_t]
        u.PostMessageW.restype = ctypes.c_int
        lparam = (pt.x & 0xFFFF) | ((pt.y & 0xFFFF) << 16)   # screen coords, MAKELPARAM
        u.ReleaseCapture()
        u.PostMessageW(ctypes.c_void_p(self._hwnd()), 0x00A1, 2, lparam)   # WM_NCLBUTTONDOWN, HTCAPTION

    def _tb_motion(self, e):
        if not self._drag:
            return
        sx, sy = self._drag
        if abs(e.x_root - sx) + abs(e.y_root - sy) < 4:
            return
        self._drag = None
        if self._maximized:
            self._restore_window_size(at_cursor=(e.x_root, e.y_root))
            self.update_idletasks()
        try:
            self._native_drag()
        except Exception:
            pass

    def _open_share_dialog(self):
        if self.current_idx is None or not self.videos:
            return
        # Pause playback while the dialog is open
        if self._mpv_playing:
            self._mpv_pause()
            self._set_play_icon(False)
        name = self.videos[self.current_idx]
        src  = name  # already full path
        dur  = self._mpv_duration
        t_start = (self.progress._trim_start / 10000.0) * dur if dur > 0 else 0
        t_end   = (self.progress._trim_end   / 10000.0) * dur if dur > 0 else dur

        dlg = tk.Toplevel(self, bg=CARD_BG)
        dlg.title("Share"); dlg.resizable(False, False)
        dlg.grab_set(); dlg.focus_set()  # blocks clicks through to main window
        try:
            hwnd  = ctypes.windll.user32.GetParent(dlg.winfo_id())
            value = ctypes.c_int(1)
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 20, ctypes.byref(value), ctypes.sizeof(value))
            r, g, b = _hex_to_rgb255(CARD_BG)
            color = ctypes.c_int(r | (g << 8) | (b << 16))
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 35, ctypes.byref(color), ctypes.sizeof(color))
        except Exception:
            pass

        SIZE_OPTIONS = [("Original", None), ("10 MB", 9.5)]
        selected_size = [None]
        output_path   = [None]   # always a tempfile path, never in the folder
        encoding      = [False]
        _encode_proc  = [None]   # ffmpeg subprocess so we can kill it on close
        _closed       = [False]
        work_dir      = [None]   # private temp folder holding the shared file
        # shared clips are always mp4 (an .mkv source is remuxed)
        share_ext = os.path.splitext(name)[1].lower() or ".mp4"
        if share_ext == ".mkv":
            share_ext = ".mp4"
        default_stem = os.path.splitext(os.path.basename(name))[0] or "clip"
        name_var = tk.StringVar(value=default_stem)
        _RESERVED = {"CON", "PRN", "AUX", "NUL", *(f"COM{i}" for i in range(1, 10)),
                     *(f"LPT{i}" for i in range(1, 10))}

        def _safe_stem():
            st = re.sub(r'[<>:"/\\|?*\x00-\x1f]', "", name_var.get()).strip()
            st = st[:150].rstrip(". ")
            if not st:
                st = re.sub(r'[<>:"/\\|?*\x00-\x1f]', "", default_stem).strip().rstrip(". ") or "clip"
            if st.split(".")[0].upper() in _RESERVED:
                st += "_"
            return st

        def _apply_name():
            """Rename the finished file to the name typed in the box (the file is
            alone in its own temp folder, so the name can't clash)."""
            cur = output_path[0]
            if not cur or encoding[0] or not os.path.exists(cur):
                return cur
            new = os.path.join(os.path.dirname(cur), _safe_stem() + share_ext)
            if new != cur:
                try:
                    os.replace(cur, new)
                    output_path[0] = new
                    return new
                except Exception:
                    pass
            return cur

        def _on_close():
            _closed[0] = True
            # Kill encode if still running
            if _encode_proc[0] is not None:
                try: _encode_proc[0].kill()
                except Exception: pass
            # Delete temp file
            if output_path[0] and os.path.exists(output_path[0]):
                try: os.remove(output_path[0])
                except Exception: pass
            if work_dir[0]:
                import shutil
                shutil.rmtree(work_dir[0], ignore_errors=True)
            dlg.destroy()

        dlg.protocol("WM_DELETE_WINDOW", _on_close)

        pad = tk.Frame(dlg, bg=CARD_BG, padx=28, pady=22)
        pad.pack(fill="both", expand=True)

        tk.Label(pad, text="SHARE ON DISCORD", font=FONT_TITLE,
                 bg=CARD_BG, fg=TEXT_PRIMARY).pack(anchor="center", pady=(0, 14))

        # Centered size label and buttons
        tk.Label(pad, text="Choose a File Size", font=FONT_UI,
                 bg=CARD_BG, fg=TEXT_PRIMARY).pack(anchor="center")
        size_row = tk.Frame(pad, bg=CARD_BG)
        size_row.pack(anchor="center", pady=(6, 14))

        size_btns = []
        def _select_size(idx):
            selected_size[0] = SIZE_OPTIONS[idx][1]
            for i, b in enumerate(size_btns):
                b.configure(bg=ACCENT if i == idx else _c("#242444"),
                            fg=ACCENT_FG if i == idx else TEXT_PRIMARY)
            _start_encode()

        for i, (label, _) in enumerate(SIZE_OPTIONS):
            b = tk.Button(size_row, text=label, font=FONT_UI,
                          bg=ACCENT if i == 0 else _c("#242444"),
                          fg=ACCENT_FG if i == 0 else TEXT_PRIMARY,
                          activebackground=ACCENT, relief="flat",
                          bd=0, padx=12, pady=5, cursor="hand2",
                          command=lambda i=i: _select_size(i))
            b.pack(side="left", padx=(0, 6))
            size_btns.append(b)

        # ── Thumbnail ─────────────────────────────────────────────────────────
        TW, TH = 280, int(280 * 9 / 16)
        card = tk.Frame(pad, bg=_c("#1A1A2E"), width=TW, height=TH + 48)
        card.pack_propagate(False)
        card.pack(pady=(0, 10))

        thumb_canvas = tk.Canvas(card, width=TW, height=TH,
                                 bg=_c("#0F0F1A"), highlightthickness=0)
        thumb_canvas.pack()
        thumb_canvas.create_text(TW//2, TH//2, text="", fill=TEXT_DIM,
                                 font=("Segoe UI", 18), tags="status")

        thumb_canvas._photo = None
        thumb_path = get_thumb(src, self._ffmpeg)
        if thumb_path and os.path.exists(thumb_path):
            try:
                from PIL import Image, ImageTk
                img = Image.open(thumb_path).convert("RGB")
                img = img.resize((TW, TH), Image.BILINEAR)
                thumb_canvas._photo = ImageTk.PhotoImage(img)
                thumb_canvas.delete("status")
                thumb_canvas.create_image(0, 0, anchor="nw",
                                          image=thumb_canvas._photo)
            except Exception:
                thumb_canvas.delete("status")
                thumb_canvas.create_text(TW//2, TH//2,
                    text="No preview", fill=TEXT_DIM,
                    font=("Segoe UI", 11))

        # Status bar under thumbnail
        status_bar = tk.Frame(card, bg=_c("#1A1A2E"))
        status_bar.pack(fill="x", expand=True)

        prog_canvas = tk.Canvas(status_bar, bg=_c("#13131f"),
                                highlightthickness=0, height=3, width=TW)
        prog_canvas.pack(fill="x")

        status_lbl = tk.Label(status_bar, text="", font=FONT_TINY,
                              bg=_c("#1A1A2E"), fg=TEXT_MUTED, pady=6)
        status_lbl.pack()

        # ── File name (editable while and after encoding; the type is fixed) ──
        _NB = _c("#161628")
        name_box = tk.Frame(pad, bg=_NB, highlightthickness=1,
                            highlightbackground=_c("#2A2A4A"))
        name_box.pack(fill="x", pady=(0, 10))
        name_entry = tk.Entry(name_box, textvariable=name_var, font=FONT_UI,
                              bg=_NB, fg=TEXT_PRIMARY, insertbackground=TEXT_PRIMARY,
                              selectbackground=ACCENT, selectforeground=ACCENT_FG,
                              relief="flat", bd=0, highlightthickness=0)
        tk.Label(name_box, text=share_ext, font=FONT_UI, bg=_NB, fg=TEXT_DIM,
                 padx=8).pack(side="right")          # greyed out: can't be changed
        name_entry.pack(side="left", fill="x", expand=True, padx=(10, 0), pady=7)

        def _name_commit(e=None):
            name_var.set(_safe_stem())
            _apply_name()
        name_entry.bind("<Return>", _name_commit)
        name_entry.bind("<FocusOut>", lambda e: (_name_commit(),
                        name_box.configure(highlightbackground=_c("#2A2A4A"))))
        name_entry.bind("<FocusIn>", lambda e: name_box.configure(highlightbackground=ACCENT))

        # ── Action buttons ────────────────────────────────────────────────────
        action_row = tk.Frame(pad, bg=CARD_BG)
        action_row.pack(pady=(2, 0))

        def _copy_to_clipboard(path):
            try:
                import struct
                header    = struct.pack("<IIIII", 20, 0, 0, 0, 1)
                path_data = (path + "\0\0").encode("utf-16-le")
                data      = header + path_data

                GMEM_MOVEABLE = 0x0002
                CF_HDROP      = 15
                k32  = ctypes.windll.kernel32
                u32  = ctypes.windll.user32
                k32.GlobalAlloc.restype   = ctypes.c_void_p
                k32.GlobalAlloc.argtypes  = [ctypes.c_uint, ctypes.c_size_t]
                k32.GlobalLock.restype    = ctypes.c_void_p
                k32.GlobalLock.argtypes   = [ctypes.c_void_p]
                k32.GlobalUnlock.argtypes = [ctypes.c_void_p]
                u32.SetClipboardData.restype  = ctypes.c_void_p
                u32.SetClipboardData.argtypes = [ctypes.c_uint, ctypes.c_void_p]
                hmem = k32.GlobalAlloc(GMEM_MOVEABLE, len(data))
                if not hmem: return False
                ptr  = k32.GlobalLock(hmem)
                if not ptr: return False
                ctypes.memmove(ptr, data, len(data))
                k32.GlobalUnlock(hmem)
                u32.OpenClipboard(0)
                u32.EmptyClipboard()
                u32.SetClipboardData(CF_HDROP, hmem)
                u32.CloseClipboard()
                return True
            except Exception:
                return False

        def _do_copy():
            path = (_apply_name() or src) if output_path[0] else src
            if _copy_to_clipboard(path):
                copy_btn.configure(text="✓  Copied!")
                dlg.after(2000, lambda: copy_btn.configure(text="Copy to clipboard"))
            else:
                copy_btn.configure(text="✗  Failed")
                dlg.after(2000, lambda: copy_btn.configure(text="Copy to clipboard"))

        copy_btn = RoundedButton(action_row, text="Copy to clipboard",
                                 command=_do_copy,
                                 fg=TEXT_PRIMARY, bg=_c("#242444"), hover_bg=_c("#2A2A4A"),
                                 font=FONT_UI, padx=14, pady=7, px_width=160)
        retry_btn = RoundedButton(action_row, text="↺  Retry",
                                  command=lambda: _start_encode(),
                                  fg=TEXT_PRIMARY, bg=_c("#242444"), hover_bg=_c("#2A2A4A"),
                                  font=FONT_UI, padx=14, pady=6)

        hint_lbl = tk.Label(pad, text="", font=FONT_TINY,
                            bg=CARD_BG, fg=TEXT_DIM, wraplength=300)
        hint_lbl.pack(pady=(4, 0))

        def _set_status(text, color=TEXT_MUTED, state="encoding", progress=None):
            status_lbl.configure(text=text, fg=color)
            prog_canvas.delete("all")
            if progress is not None:
                w = TW
                prog_canvas.create_rectangle(0, 0, int(w * min(1.0, progress)), 3,
                                             fill=ACCENT, outline="")
            retry_btn.pack_forget()
            hint_lbl.configure(text="")
            copy_btn.pack(side="left", padx=(0, 6))
            if state == "ready":
                hint_lbl.configure(text="")
            elif state == "error":
                retry_btn.pack()

        def _start_encode():
            if encoding[0]:
                return
            encoding[0] = True
            output_path[0] = None
            copy_btn.configure(text="Copy to clipboard")
            _set_status("Preparing…", TEXT_MUTED, state="encoding", progress=0)
            snap = self._at_export_snapshot(src)     # audio tracks to share (main thread)

            def _run():
                import tempfile
                try:
                    ext      = share_ext
                    # Always write inside a private temp folder — never to the clips
                    # folder. The file is renamed to the typed name when it is done.
                    work_dir[0] = tempfile.mkdtemp(prefix="FeatherShare_")
                    tmp      = os.path.join(work_dir[0], "encoding" + ext)
                    mb_lim   = selected_size[0]
                    clip_dur = max(0.1, t_end - t_start)

                    ff      = self._ffmpeg
                    time_re = re.compile(r"time=(\d+):(\d+):(\d+\.?\d*)")

                    def _pump(proc, lo, hi):
                        buf = ""
                        for chunk in iter(lambda: proc.stderr.read(512), b""):
                            buf += chunk.decode(errors="ignore")
                            m = time_re.search(buf)
                            if m:
                                cur = (int(m.group(1))*3600 + int(m.group(2))*60
                                       + float(m.group(3))) - t_start
                                f = lo + (hi - lo) * min(1.0, max(0.0, cur / clip_dur))
                                dlg.after(0, lambda f=f: _set_status(
                                    "Encoding…", TEXT_MUTED, state="encoding", progress=f))

                    if mb_lim is None:
                        dlg.after(0, lambda: _set_status(
                            "Encoding…", TEXT_MUTED, state="encoding", progress=0.05))
                        faststart = ext in (".mp4", ".m4v", ".mov")
                        rc = 1
                        # video is copied; the audio track is copied too when it can
                        # be, otherwise (codec not allowed in mp4) re-encoded as aac
                        for _aac in (False, True):
                            aud = self._share_audio_args(src, snap, aac=_aac)
                            args = [ff, "-y", "-ss", str(t_start), "-to", str(t_end),
                                    "-i", src, *aud, "-c:v", "copy"]
                            if faststart:
                                args += ["-movflags", "+faststart"]
                            args.append(tmp)
                            proc = subprocess.Popen(args, stderr=subprocess.PIPE,
                                                    creationflags=0x08000000)
                            _encode_proc[0] = proc
                            _pump(proc, 0.05, 0.95)
                            proc.wait()
                            _encode_proc[0] = None
                            rc = proc.returncode
                            if rc == 0 or _closed[0]:
                                break
                            if _aac or not any(a == "copy" for a in aud):
                                break
                        if rc != 0:
                            raise RuntimeError("ffmpeg failed")
                    else:
                        # Single-pass encode, capped with maxrate/bufsize (VBV)
                        # so the encoder can't drift upward the way plain -b:v
                        # did before. Tries the GPU encoder first (much faster
                        # than software x264 when available) and falls back to
                        # libx264 if there's no compatible GPU.
                        audio_bits = 160_000
                        target_mb  = mb_lim
                        video_bits = max(200_000,
                            int((target_mb * 8 * 1024 * 1024 / clip_dur) - audio_bits))
                        bufsize    = video_bits * 2

                        # The real quality killer at small target sizes isn't the
                        # encoder preset — it's too few bits per pixel for the
                        # source resolution. Downscale only if the budget is
                        # genuinely too tight for the current resolution; this
                        # keeps the encode at least as fast (fewer pixels) while
                        # actually fixing the blockiness instead of an encoder
                        # setting that makes barely any difference at low bitrate.
                        src_w, src_h, src_fps = get_video_info(src, ff)
                        scale_filter = None
                        if src_w and src_h:
                            fps = src_fps or 30.0
                            BPP_FLOOR = 0.07 * 0.4   # ~0.028 bits/pixel/frame
                            def _ref_bps(w, h):
                                return w * h * fps * BPP_FLOOR
                            if video_bits < _ref_bps(src_w, src_h):
                                new_h = src_h
                                for h in (1080, 720, 540, 480, 360, 240):
                                    if h >= src_h:
                                        continue
                                    new_h = h
                                    new_w = src_w * h / src_h
                                    if video_bits >= _ref_bps(new_w, h):
                                        break
                                if new_h < src_h:
                                    scale_filter = f"scale=-2:{new_h}"

                        dlg.after(0, lambda: _set_status(
                            "Encoding…", TEXT_MUTED, state="encoding", progress=0.05))

                        def _encode(codec_args):
                            proc = subprocess.Popen(codec_args, stderr=subprocess.PIPE,
                                                    creationflags=0x08000000)
                            _encode_proc[0] = proc
                            _pump(proc, 0.05, 0.95)
                            proc.wait()
                            _encode_proc[0] = None
                            return proc.returncode

                        vf = ["-vf", scale_filter] if scale_filter else []
                        aud_args = self._share_audio_args(src, snap, aac=True, abr="160k")

                        rc = 1
                        if getattr(self, "_nvenc_ok", True):
                            nvenc_args = [ff, "-y", "-ss", str(t_start), "-to", str(t_end),
                                          "-i", src, *aud_args,
                                          "-c:v", "h264_nvenc", "-preset", "fast", "-rc", "vbr",
                                          *vf,
                                          "-b:v", str(video_bits),
                                          "-maxrate", str(video_bits),
                                          "-bufsize", str(bufsize),
                                          "-movflags", "+faststart", tmp]
                            rc = _encode(nvenc_args)
                            # Remember whether the GPU path works so later
                            # shares don't waste time retrying a dead encoder.
                            self._nvenc_ok = (rc == 0)

                        if rc != 0:
                            cpu_args = [ff, "-y", "-ss", str(t_start), "-to", str(t_end),
                                        "-i", src, *aud_args,
                                        "-c:v", "libx264", "-preset", "veryfast",
                                        *vf,
                                        "-b:v", str(video_bits),
                                        "-maxrate", str(video_bits),
                                        "-bufsize", str(bufsize),
                                        "-movflags", "+faststart", tmp]
                            rc = _encode(cpu_args)
                            if rc != 0:
                                raise RuntimeError("ffmpeg failed")

                    output_path[0] = tmp
                    size_mb  = os.path.getsize(tmp) / 1024 / 1024
                    encoding[0] = False
                    def _finish(size_mb=size_mb):
                        _apply_name()
                        _set_status(f"Ready  •  {size_mb:.1f} MB",
                                    _c("#FFFFFF"), state="ready", progress=1.0)
                    dlg.after(0, _finish)

                except Exception:
                    encoding[0] = False
                    _encode_proc[0] = None
                    dlg.after(0, lambda: _set_status(
                        "Encoding failed", _c("#e05555"), state="error"))

            threading.Thread(target=_run, daemon=True).start()

        tk.Frame(pad, bg=_c("#2A2A4A"), height=1).pack(fill="x", pady=(12, 0))
        btn_row = tk.Frame(pad, bg=CARD_BG, pady=10)
        btn_row.pack(fill="x")
        RoundedButton(btn_row, text="Close", command=_on_close,
                      fg=TEXT_PRIMARY, bg=_c("#242444"), hover_bg=_c("#2A2A4A"),
                      font=FONT_UI, padx=14, pady=6).pack(side="right")

        self.update_idletasks(); dlg.update_idletasks()
        pw, ph = self.winfo_width(), self.winfo_height()
        px, py = self.winfo_rootx(), self.winfo_rooty()
        dw, dh = dlg.winfo_reqwidth(), dlg.winfo_reqheight()
        dlg.geometry(f"+{px + (pw-dw)//2}+{py + (ph-dh)//2}")

        _start_encode()

    def _dark_dialog(self, title, message, kind="yesno", yes_text="Delete",
                     details=None, more=0, note=None, extra_text=None,
                     extra2_text=None, extra2_value="cloud_only", extra2_style="red"):
        """Themed, frameless confirmation dialog that matches the app's look
        (title bar + accent line, card body, themed colours). The confirm
        button stays red in every theme. `details` is an optional list shown
        in an inset box; `more` adds a '+ N more' line; `note` is small muted text."""
        result = [None]
        RED, AMBER = "#e8454a", "#e8c84a"
        tone = RED if kind == "yesno" else AMBER
        paras = message.split("\n\n")
        head = paras[0]
        if note is None and len(paras) > 1:
            note = "\n\n".join(paras[1:])

        dlg = tk.Toplevel(self, bg=BORDER)
        dlg.withdraw()
        dlg.overrideredirect(True)
        dlg.transient(self)
        shell = tk.Frame(dlg, bg=CARD_BG)
        shell.pack(fill="both", expand=True, padx=1, pady=1)

        import tkinter.font as _tkf
        def _pxw(text, font, padx):
            try:
                return _tkf.Font(font=font).measure(text) + padx * 2 + 4
            except Exception:
                return 100

        def _yes(): result[0] = True;  dlg.destroy()
        def _no():  result[0] = False; dlg.destroy()
        def _extra(): result[0] = "cloud"; dlg.destroy()
        def _extra2(): result[0] = extra2_value; dlg.destroy()

        # Header: same look as the main title bar (themed bg + accent line)
        hdr = tk.Frame(shell, bg=BG, height=37)
        hdr.pack(fill="x"); hdr.pack_propagate(False)
        ttl = tk.Label(hdr, text=title, font=FONT_BOLD, bg=BG, fg=TEXT_PRIMARY)
        ttl.pack(side="left", padx=(16, 0))
        TitleButton(hdr, "close", _no, danger=True).pack(side="right", padx=(0, 6))
        tk.Frame(shell, bg=ACCENT, height=2).pack(fill="x")

        def _drag_start(e):
            dlg._dx, dlg._dy = e.x_root - dlg.winfo_x(), e.y_root - dlg.winfo_y()
        def _drag_move(e):
            dlg.geometry(f"+{e.x_root - dlg._dx}+{e.y_root - dlg._dy}")
        for w_ in (hdr, ttl):
            w_.bind("<ButtonPress-1>", _drag_start)
            w_.bind("<B1-Motion>", _drag_move)

        # Body
        body = tk.Frame(shell, bg=CARD_BG, padx=22, pady=20)
        body.pack(fill="x")
        badge = tk.Canvas(body, width=46, height=46, bg=CARD_BG, highlightthickness=0, bd=0)
        badge.pack(side="left", anchor="n", padx=(0, 16))
        _rounded_rect(badge, 0, 0, 46, 46, 14, fill=_mix_hex(CARD_BG, tone, 0.45))
        _rounded_rect(badge, 1, 1, 45, 45, 13, fill=_mix_hex(CARD_BG, tone, 0.16))
        icon_name = "trash" if (kind == "yesno" and yes_text == "Delete") else "alert"
        badge.create_image(23, 23, image=ui_icon(icon_name, 22, tone), anchor="center")

        txt = tk.Frame(body, bg=CARD_BG)
        txt.pack(side="left", fill="x", expand=True)
        tk.Label(txt, text=head, font=("Segoe UI", 11, "bold"), bg=CARD_BG, fg=TEXT_PRIMARY,
                 justify="left", anchor="w", wraplength=340).pack(anchor="w", fill="x")
        if details:
            box = tk.Frame(txt, bg=BORDER)
            box.pack(fill="x", pady=(12, 0))
            inner = tk.Frame(box, bg=_c("#161628"), padx=12, pady=8)
            inner.pack(fill="x", padx=1, pady=1)
            for item in details:
                tk.Label(inner, text=_ellipsize(str(item), FONT_UI, 310), font=FONT_UI,
                         bg=_c("#161628"), fg=TEXT_MUTED, anchor="w",
                         justify="left").pack(fill="x", anchor="w")
            if more:
                tk.Label(inner, text=f"+ {more} more", font=FONT_TINY, bg=_c("#161628"),
                         fg=TEXT_DIM, anchor="w").pack(fill="x", anchor="w", pady=(3, 0))
        if note:
            tk.Label(txt, text=note, font=FONT_UI, bg=CARD_BG, fg=TEXT_DIM,
                     justify="left", anchor="w", wraplength=340).pack(anchor="w", fill="x", pady=(12, 0))

        tk.Frame(shell, bg=DIVIDER, height=1).pack(fill="x")
        btn_row = tk.Frame(shell, bg=CARD_BG, padx=16, pady=12)
        btn_row.pack(fill="x")
        if kind == "yesno":
            RoundedButton(btn_row, text="Cancel", command=_no,
                          fg=TEXT_PRIMARY, bg=_c("#242444"), hover_bg=_c("#2A2A4A"),
                          font=FONT_UI, padx=16, pady=7,
                          px_width=_pxw("Cancel", FONT_UI, 16)).pack(side="right", padx=(8, 0))
            RoundedButton(btn_row, text=yes_text, command=_yes,
                          fg="#ffffff", bg="#c9353c", hover_bg=RED,
                          font=FONT_BOLD, padx=18, pady=7,
                          px_width=_pxw(yes_text, FONT_BOLD, 18)).pack(side="right")
            if extra_text:
                RoundedButton(btn_row, text=extra_text, command=_extra,
                              fg="#ffffff", bg="#8f2a30", hover_bg="#c9353c",
                              font=FONT_BOLD, padx=16, pady=7,
                          px_width=_pxw(extra_text, FONT_BOLD, 16)).pack(side="right", padx=(0, 8))
            if extra2_text:
                if extra2_style == "neutral":
                    _b2, _h2 = _c("#2f2f58"), _c("#3a3a6a")
                else:
                    _b2, _h2 = "#8f2a30", "#c9353c"
                RoundedButton(btn_row, text=extra2_text, command=_extra2,
                              fg="#ffffff", bg=_b2, hover_bg=_h2,
                              font=FONT_BOLD, padx=16, pady=7,
                          px_width=_pxw(extra2_text, FONT_BOLD, 16)).pack(side="right", padx=(0, 8))
            dlg.bind("<Return>", lambda e: _yes()); dlg.bind("<Escape>", lambda e: _no())
        else:
            RoundedButton(btn_row, text="OK", command=_no,
                          fg=TEXT_PRIMARY, bg=_c("#242444"), hover_bg=_c("#2A2A4A"),
                          font=FONT_UI, padx=18, pady=7,
                          px_width=_pxw("OK", FONT_UI, 18)).pack(side="right")
            dlg.bind("<Return>", lambda e: _no()); dlg.bind("<Escape>", lambda e: _no())

        self.update_idletasks(); dlg.update_idletasks()
        pw, ph = self.winfo_width(), self.winfo_height()
        px, py = self.winfo_rootx(), self.winfo_rooty()
        dw, dh = dlg.winfo_reqwidth(), dlg.winfo_reqheight()
        dlg.geometry(f"+{px + (pw-dw)//2}+{py + (ph-dh)//2}")
        dlg.deiconify(); dlg.lift()
        try:
            dlg.wait_visibility()
            dlg.grab_set(); dlg.focus_force()
        except Exception:
            pass
        try:   # Windows 11: rounded corners + themed outline for the frameless window
            hwnd = ctypes.windll.user32.GetParent(dlg.winfo_id()) or dlg.winfo_id()
            pref = ctypes.c_int(2)    # DWMWCP_ROUND
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 33, ctypes.byref(pref), ctypes.sizeof(pref))
            r, g, b = _hex_to_rgb255(BORDER)
            col = ctypes.c_int(r | (g << 8) | (b << 16))
            ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 34, ctypes.byref(col), ctypes.sizeof(col))
        except Exception:
            pass
        self.wait_window(dlg)
        # The modal grab swallows <Leave>: un-hover the red buttons that opened it
        for pill, btn in ((getattr(self, "delete_sel_btn", None), getattr(self, "_delete_btn", None)),
                          (getattr(self, "_cache_pill", None), getattr(self, "_cache_btn", None))):
            try:
                if pill is not None and pill.winfo_exists():
                    pill.set_danger(False)
                if btn is not None and btn.winfo_exists():
                    btn._on_leave(None)
            except Exception:
                pass
        return result[0]

    def on_close(self):
        """X button — hide to the system tray (or quit, if turned off in Settings)."""
        self._save_window_size()
        if not self._close_to_tray:
            self._real_quit()
            return
        if getattr(self, "_closing_to_tray", False):
            return
        self._closing_to_tray = True
        self._save_prefs()
        _debug_log("tray: closing mpv")
        self._kill_mpv_fast()
        self._hide_to_tray_when_mpv_closed()

    def _show_tray_icon(self):
        """Spawn a system-tray icon in a daemon thread."""
        if getattr(self, "_tray_active", False):
            return
        self._tray_active = True

        def _run_tray():
            try:
                import pystray
                from PIL import Image as PilImage
                try:
                    img = PilImage.open(
                        resource_path(os.path.join("pics", "feather2.png"))
                    ).convert("RGBA").resize((64, 64))
                except Exception:
                    img = PilImage.new("RGBA", (64, 64), _c("#FF2A6D"))

                def _open(icon, item):
                    icon.stop()
                    self._tray_active = False
                    self.after(0, self._restore_window)

                def _quit(icon, item):
                    icon.stop()
                    self._tray_active = False
                    self.after(0, self._real_quit)

                menu = pystray.Menu(
                    pystray.MenuItem("Open Feather Player", _open, default=True),
                    pystray.MenuItem("Quit",       _quit),
                )
                self._tray = pystray.Icon("FeatherPlayer", img, "Feather Player", menu)
                self._tray.run()
            except Exception:
                self.after(0, self._real_quit)

        threading.Thread(target=_run_tray, daemon=True).start()

    def _reset_player_fully(self, tries=0):
        """Opened from the tray: shut mpv down, rebuild the video canvas (new
        window handle, so no leftover video window can stay on screen), then
        start a fresh mpv on it."""
        if tries == 0:
            self._hang_watchdog(120)
            self._stop_playback_for_tray()
            self._kill_mpv_fast()
        t = getattr(self, "_mpv_kill_thread", None)
        old_alive = t is not None and t.is_alive()
        if old_alive and tries < 50:
            self.after(100, lambda: self._reset_player_fully(tries + 1))
            return                      # wait (max ~5 s) for the old mpv to be gone
        if old_alive:
            # Old mpv is still shutting down: destroying its window now could
            # deadlock the app, so keep the old canvas and just start a new mpv.
            _debug_log("reset: old mpv still alive after 5s, keeping old canvas")
            self._start_mpv()
            return

        self._mpv_playing = False
        self._mpv_pos = 0.0
        self._mpv_duration = 0.0
        try:
            self.no_video_label.destroy()
        except Exception:
            pass
        try:
            self.video_canvas.destroy()
        except Exception:
            pass
        self.video_canvas = tk.Canvas(self.video_frame, bg=PLAYER_BG, highlightthickness=0)
        self.video_canvas.pack(fill="both", expand=True)
        self.video_canvas.bind("<Configure>", self._on_video_resize)
        self.video_canvas.bind("<Configure>", self._queue_sash_sync, add="+")
        self.no_video_label = tk.Label(self.video_canvas, text="Select a clip to play",
                                       font=("Segoe UI", 14), bg=PLAYER_BG, fg=TEXT_DIM)
        self.no_video_label.place(relx=0.5, rely=0.5, anchor="center")
        self.update_idletasks()
        self._start_mpv()

    def _hide_to_tray_when_mpv_closed(self, tries=0):
        """Hide to the tray only once mpv has fully shut down (max ~3 s), so
        its video window is destroyed while the app window is still visible."""
        t = getattr(self, "_mpv_kill_thread", None)
        if t is not None and t.is_alive() and tries < 60:
            self.after(50, lambda: self._hide_to_tray_when_mpv_closed(tries + 1))
            return
        if t is not None and t.is_alive():
            _debug_log("tray: mpv still closing after 3s, hiding anyway")
        else:
            _debug_log("tray: mpv closed, hiding window")
        self._closing_to_tray = False
        self.withdraw()
        self._hidden_to_tray = True
        self._show_tray_icon()

    def _hang_watchdog(self, left=120):
        """Debug aid: for 2 minutes after opening from the tray, if the UI
        stops responding for 6 s, every thread's stack is written to
        Downloads\\crash.txt so we can see exactly where it is stuck."""
        try:
            faulthandler.cancel_dump_traceback_later()
            if left > 0:
                faulthandler.dump_traceback_later(6, file=_crash_log_file)
                self.after(1000, lambda: self._hang_watchdog(left - 1))
        except Exception:
            pass

    def _stop_playback_for_tray(self):
        """Hiding to the tray: stop the current clip but keep the mpv instance."""
        for name in ("_load_job", "_amix_job"):
            job = getattr(self, name, None)
            if job:
                try:
                    self.after_cancel(job)
                except Exception:
                    pass
                setattr(self, name, None)
        self._mpv_loading = None
        self._mpv_hold_for_mix = None
        self._mpv_pause()
        self._mpv_cmd("stop")
        try:
            self._set_play_icon(False)
        except Exception:
            pass

    def _relaunch_fresh(self):
        """Opened from the tray: start a brand-new copy of the app and exit this
        one, so the player (mpv, video window) starts completely clean."""
        try:
            self._save_window_size()
            self._save_prefs()
            if getattr(sys, "frozen", False):
                cmd = [sys.executable]
                cwd = os.path.dirname(sys.executable)
            else:
                exe = sys.executable
                pyw = os.path.join(os.path.dirname(exe), "pythonw.exe")
                if os.path.isfile(pyw):
                    exe = pyw
                cmd = [exe, os.path.abspath(__file__)]
                cwd = os.path.dirname(os.path.abspath(__file__))
            cmd.append("--relaunch")
            # DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
            subprocess.Popen(cmd, cwd=cwd, close_fds=True, creationflags=0x00000008 | 0x00000200)
        except Exception as e:
            _debug_log(f"relaunch from tray failed: {e}")
            return False
        os._exit(0)

    def _restore_window(self):
        self._hidden_to_tray = False
        self.deiconify()
        self.lift()
        self.focus_force()
        # Reset player state
        self.current_idx = None
        self._mpv_duration = 0
        self._at_clip_reset()
        # Show "select a clip" screen
        try:
            self.no_video_label.place(relx=0.5, rely=0.5, anchor="center")
        except Exception:
            pass
        # Reset progress bar
        try:
            self.progress.set(0)
            self.time_label.configure(text="0:00")
            self.dur_label.configure(text="0:00")
        except Exception:
            pass
        # Open a completely fresh mpv (the old one was closed when hiding to the tray)
        _debug_log("tray: restoring, starting new mpv")
        self._hang_watchdog(120)
        self.after(100, self._start_mpv)

    def _real_quit(self):
        self._save_window_size()
        self._save_prefs()
        self._kill_mpv_fast()
        self.destroy()

    def _kill_mpv_fast(self):
        """Terminate the in-process libmpv instance in a background thread
        so the UI doesn't freeze while it shuts down."""
        # Invalidate this generation right away. Any property-observer
        # callbacks still firing for the old mpv instance will see the
        # mismatch on their next check and stop touching shared state,
        # instead of clobbering values meant for the next mpv session.
        self._mpv_generation += 1
        player = getattr(self, "_mpv", None)
        self._mpv           = None
        self._mpv_proc       = None
        self._mpv_hwnd       = None
        self._mpv_ipc_ready  = False

        def _do_kill():
            if player:
                try:
                    player.terminate()
                except Exception:
                    pass
        # Keep the thread so a restore can wait until the old player is really gone
        self._mpv_kill_thread = threading.Thread(target=_do_kill, daemon=True)
        self._mpv_kill_thread.start()

    def _start_mpv_after_old_gone(self, tries=0):
        """Start the new mpv only once the old one has finished shutting down
        (max ~3 s), so the old video window can't be left on top."""
        t = getattr(self, "_mpv_kill_thread", None)
        if t is not None and t.is_alive() and tries < 30:
            self.after(100, lambda: self._start_mpv_after_old_gone(tries + 1))
            return
        self._start_mpv()


_SINGLE_INSTANCE_MUTEX = "ClipsVideoPlayerApp_SingleInstance_Mutex"

# A fixed loopback port used purely as a local signal channel between app
# instances. Nothing is exposed to the network — 127.0.0.1 connections only.
_RESTORE_PORT = 51873


def _send_restore_signal():
    """Called by a second launch to ask the already-running instance to pop
    itself back into view. Uses a tiny local TCP connection rather than a
    custom window message: Tk's own Windows message pump continuously drains
    the thread's message queue as part of its event loop, so a manually
    PostMessageW'd custom message almost never survives long enough for a
    periodic PeekMessageW poll to catch it. A socket signal has no such race."""
    try:
        s = socket.create_connection(("127.0.0.1", _RESTORE_PORT), timeout=1.0)
        s.sendall(b"restore")
        s.close()
    except Exception:
        pass


def _check_single_instance():
    """
    Returns a mutex handle if this is the first instance, or None if another
    instance is already running (in which case it signals that instance to
    restore itself and the caller should exit).
    """
    kernel32 = ctypes.windll.kernel32
    user32   = ctypes.windll.user32

    mutex = kernel32.CreateMutexW(None, True, _SINGLE_INSTANCE_MUTEX)
    last_err = kernel32.GetLastError()

    ERROR_ALREADY_EXISTS = 183
    if last_err == ERROR_ALREADY_EXISTS:
        # Another instance owns the mutex — ask it to restore itself.
        _send_restore_signal()
        # Best-effort extra nudge (harmless even if the window is withdrawn).
        hwnd = user32.FindWindowW(None, "Feather Player")
        if hwnd:
            user32.ShowWindow(hwnd, 9)   # SW_RESTORE
            user32.SetForegroundWindow(hwnd)
        return None  # caller should sys.exit()

    return mutex  # keep alive for the lifetime of the process


def _start_restore_listener(app):
    """Background thread: listen on a local loopback socket for "restore"
    signals sent by newly-launched instances, then hand off to the Tk main
    thread (via app.after) to actually bring the window back."""

    def _handle_restore():
        if getattr(app, "_hidden_to_tray", False):
            if getattr(app, "_tray_active", False):
                try:
                    app._tray.stop()
                except Exception:
                    pass
                app._tray_active = False
            app._restore_window()
        else:
            app.deiconify()
            app.lift()
            app.focus_force()

    def _serve():
        try:
            srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            srv.bind(("127.0.0.1", _RESTORE_PORT))
            srv.listen(5)
        except Exception:
            # Port already taken/unavailable for some reason — restore will
            # fall back to the best-effort ShowWindow/SetForegroundWindow nudge.
            return
        while True:
            try:
                conn, _addr = srv.accept()
            except Exception:
                continue
            try:
                conn.recv(64)
            except Exception:
                pass
            finally:
                try:
                    conn.close()
                except Exception:
                    pass
            app._post_main(_handle_restore)

    threading.Thread(target=_serve, daemon=True).start()


if __name__ == "__main__":
    try:
        ctypes.windll.shell32.SetCurrentProcessExplicitAppUserModelID("FeatherPlayer.App")
    except Exception:
        pass
    _refresh_startup_entry()

    # Make the process DPI-aware *before* any Tk window is created. Without
    # this, python.exe (which Windows already treats as DPI-aware) and the
    # PyInstaller-built .exe (which isn't, unless told to be) get scaled
    # differently by Windows at anything other than 100% display scaling.
    # That's what makes icons/buttons look shifted and can throw off click
    # coordinates — including the native folder-picker dialog — only in the
    # built exe.
    if sys.platform == "win32":
        try:
            ctypes.windll.shcore.SetProcessDpiAwareness(2)  # per-monitor v2
        except Exception:
            try:
                ctypes.windll.user32.SetProcessDPIAware()   # older Windows
            except Exception:
                pass

    if "--relaunch" in sys.argv:
        # Started by the old copy (opened from the tray): wait until it has
        # exited and released the single-instance mutex, without signalling it.
        _k32 = ctypes.windll.kernel32
        for _ in range(60):
            _m = _k32.CreateMutexW(None, False, _SINGLE_INSTANCE_MUTEX)
            _busy = (_k32.GetLastError() == 183)
            if _m:
                _k32.CloseHandle(_m)
            if not _busy:
                break
            time.sleep(0.1)

    _mutex_handle = _check_single_instance()
    if _mutex_handle is None:
        sys.exit(0)   # another instance is already running — we're done

    app = VideoPlayerApp()
    if "--minimized" in sys.argv:           # started by "Start with Windows"
        app.withdraw()
        app.after(800, app._start_hidden)
    app.protocol("WM_DELETE_WINDOW", app.on_close)
    app.bind("<Control-q>", lambda e: app._real_quit())

    _start_restore_listener(app)

    app.mainloop()