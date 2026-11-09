# ClickLight 💡

> **High-performance, zero-mode passive screen annotation & glowing click indicator for Windows, written in Rust.**

[![Build](https://github.com/TimmyPan29/clicklight/actions/workflows/release.yml/badge.svg)](https://github.com/TimmyPan29/clicklight/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

ClickLight runs passively in the background with **0% idle CPU usage**, rendering directly to a full-desktop 32-bit ARGB layered window (`UpdateLayeredWindow` + `tiny-skia` anti-aliasing engine). It never freezes the screen, takes focus, or swallows normal mouse clicks.

---

## ✨ Features

- **🔴 Left-Click Wavelet (光芒漣漪)**:
  - Produces an expanding neon ripple with soft ambient aura and central spark core (`#FF2905`, 460ms).
  - Anti-aliased and transparent—never blocks your cursor or window interactions.

- **🔲 Rectangular Fade Pen (三指圈選 / 矩形淡出筆)**:
  - Evoked via customizable shortcut (default: <kbd>Ctrl</kbd> + <kbd>Alt</kbd> + <kbd>Shift</kbd> + <kbd>R</kbd>).
  - Displays a subtle floating glass HUD badge (`[ ] RECT PEN`) at the top of the screen when active.
  - Pair with **Three-Finger Drag** (e.g., [ThreeFingersDragOnWindows](https://github.com/ClementGre/ThreeFingerDragOnWindows) or standard mouse drag) to draw glowing neon rectangles.
  - On release, the rectangle stays visible for 700ms (`fade_delay_ms`), then smoothly dissolves away over 900ms (`fade_duration_ms`).
  - Small drags (< 6px) automatically count as click wavelets to avoid accidental 0×0 dots.

- **🔇 Silent Normal Mode**:
  - In normal mode, dragging draws **nothing**—your standard text selection, file dragging, and window management work without any visual clutter or interference.

- **⚡ Zero Idle Overhead & Native Win32 Performance**:
  - Written in Rust with `windows-sys` and direct DIB section rendering.
  - Low-level mouse hook (`WH_MOUSE_LL`) activates rendering timers only during active animations.
  - Per-Monitor DPI Aware (v2) across multi-monitor setups.
  - Single-instance mutex protection (`Local\ClickLightRsSingleInstance`).

---

## ⌨️ Default Hotkeys

| Hotkey | Action | Description |
|---|---|---|
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>R</kbd> | **Toggle Rect Pen** | Switch between Normal and Rectangular Pen mode |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> | **Master Toggle** | Enable or disable ClickLight completely |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>D</kbd> | **Clear Screen** | Wipe all active annotations immediately |

All shortcuts and timings are fully customizable in `%APPDATA%\ClickLight\config.toml`.

---

## ⚙️ Configuration

Configuration is automatically created at `%APPDATA%\ClickLight\config.toml` on first launch:

```toml
[hotkeys]
rect_pen          = "Ctrl+Alt+Shift+R"   # Shortcut to toggle the rectangular fade pen
toggle_clicklight = "Ctrl+Alt+Shift+C"   # Master ON / OFF toggle
clear             = "Ctrl+Alt+Shift+D"   # Instantly wipe all active rectangles
quit              = ""                   # Hotkey to exit ClickLight (optional)

[ripple]
enabled     = true
color       = "#FF2905"
size        = 42.0
duration_ms = 460

[rect_pen]
mode             = "toggle"    # "toggle" (switch on/off) or "oneshot" (auto-exits after 1 rectangle)
color            = "#FF2905"
stroke_width     = 3.0
fill_opacity     = 0.10
corner_radius    = 6.0
fade_delay_ms    = 700         # Duration (ms) rectangle stays fully visible before fading
fade_duration_ms = 900         # Fade-out duration (ms)
min_size         = 6.0         # Minimum drag distance (px) before activating rectangle
block_clicks     = true        # Suppress left-click pass-through to apps while pen is active
show_indicator   = true        # Display floating "RECT PEN" indicator badge at top of screen
```

To apply configuration changes on the fly:
```bash
clicklight.exe --reload
```

---

## 🚀 CLI Commands

Run `clicklight.exe [command]` (or `clicklight [command]` in WSL2):

- `clicklight` / `clicklight.exe` — Start background service
- `clicklight.exe --toggle-rect` / `clicklight rect` — Toggle Rect Pen mode
- `clicklight.exe --toggle` / `clicklight toggle` — Master ON / OFF toggle
- `clicklight.exe --clear` / `clicklight clear` — Clear all active screen strokes
- `clicklight.exe --reload` / `clicklight reload` — Hot-reload `config.toml`
- `clicklight.exe --config` / `clicklight config` — Open `config.toml` in Notepad
- `clicklight.exe --quit` / `clicklight stop` — Exit ClickLight

---

## 🛠️ Building from Source

### Prerequisites

- [Rust](https://rustup.rs/) (edition 2021)
- Target: `x86_64-pc-windows-gnu` or `x86_64-pc-windows-msvc`
- MinGW GCC (`gcc-mingw-w64-x86-64`) if cross-compiling from Linux/WSL

### Build Release Binary

```bash
# Native Windows:
cargo build --release

# Cross-compiling from Linux / WSL:
cargo build --release --target x86_64-pc-windows-gnu
```

The resulting standalone executable is located at `target/[target]/release/clicklight.exe`.

---

## 📜 License

MIT License. See [LICENSE](LICENSE) for details.
