//! User configuration: `%APPDATA%\ClickLight\config.toml`.

use serde::Deserialize;
use std::path::PathBuf;

use crate::render::Rgb;

pub const DEFAULT_TOML: &str = r##"# ClickLight (Rust) configuration
# Apply changes with:  clicklight.exe --reload   (or `clicklight reload` from WSL)
#
# Hotkey syntax: modifiers (Ctrl, Alt, Shift, Win) + one key, joined by "+".
#   Keys: A-Z, 0-9, F1-F24, Space, Tab, Enter, Esc, Insert, Delete, Home, End,
#         PageUp, PageDown, Left, Right, Up, Down, ` - = [ ] ; ' , . / \
#   Use "" to disable a hotkey.

[hotkeys]
rect_pen          = "Ctrl+Alt+Shift+R"   # toggle the rectangular fade pen
toggle_clicklight = "Ctrl+Alt+Shift+C"   # turn ClickLight completely on / off
clear             = "Ctrl+Alt+Shift+D"   # wipe all rectangles immediately
quit              = ""                   # exit ClickLight

[ripple]
# Normal mode: left-click shows a ripple, dragging draws nothing.
enabled     = true
color       = "#FF2905"
size        = 42.0
duration_ms = 460

[rect_pen]
# "toggle"  : hotkey enters the mode, press again to leave
# "oneshot" : mode leaves automatically after one rectangle
mode             = "toggle"
color            = "#FF2905"
stroke_width     = 3.0
fill_opacity     = 0.10
corner_radius    = 6.0
fade_delay_ms    = 700     # how long a finished rectangle stays fully visible
fade_duration_ms = 900     # then fades out over this duration
min_size         = 6.0     # drags smaller than this (px) count as a click
# true  : while the pen is on, left button presses are NOT delivered to apps
#         (drawing over a window won't select text / move icons)
# false : clicks pass through to the app underneath
block_clicks     = true
show_indicator   = true    # small "RECT PEN" badge at top of screen while active
"##;

#[derive(Deserialize, Clone, Default)]
#[serde(default)]
pub struct Config {
    pub hotkeys: Hotkeys,
    pub ripple: RippleCfg,
    pub rect_pen: RectPenCfg,
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Hotkeys {
    pub rect_pen: String,
    pub toggle_clicklight: String,
    pub clear: String,
    pub quit: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            rect_pen: "Ctrl+Alt+Shift+R".into(),
            toggle_clicklight: "Ctrl+Alt+Shift+C".into(),
            clear: "Ctrl+Alt+Shift+D".into(),
            quit: String::new(),
        }
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct RippleCfg {
    pub enabled: bool,
    pub color: String,
    pub size: f32,
    pub duration_ms: u64,
}

impl Default for RippleCfg {
    fn default() -> Self {
        Self { enabled: true, color: "#FF2905".into(), size: 42.0, duration_ms: 460 }
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct RectPenCfg {
    pub mode: String,
    pub color: String,
    pub stroke_width: f32,
    pub fill_opacity: f32,
    pub corner_radius: f32,
    pub fade_delay_ms: u64,
    pub fade_duration_ms: u64,
    pub min_size: f32,
    pub block_clicks: bool,
    pub show_indicator: bool,
}

impl Default for RectPenCfg {
    fn default() -> Self {
        Self {
            mode: "toggle".into(),
            color: "#FF2905".into(),
            stroke_width: 3.0,
            fill_opacity: 0.10,
            corner_radius: 6.0,
            fade_delay_ms: 700,
            fade_duration_ms: 900,
            min_size: 6.0,
            block_clicks: true,
            show_indicator: true,
        }
    }
}

impl RectPenCfg {
    pub fn oneshot(&self) -> bool {
        self.mode.eq_ignore_ascii_case("oneshot")
    }
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ClickLight").join("config.toml")
}

pub fn ensure_exists(path: &PathBuf) {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, DEFAULT_TOML);
    }
}

/// Returns the config plus an optional error message (on error, defaults are used).
pub fn load(path: &PathBuf) -> (Config, Option<String>) {
    ensure_exists(path);
    match std::fs::read_to_string(path) {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(cfg) => (cfg, None),
            Err(e) => (Config::default(), Some(format!("Config error: {}", e.message()))),
        },
        Err(e) => (Config::default(), Some(format!("Config unreadable: {e}"))),
    }
}

pub fn parse_color(s: &str, fallback: Rgb) -> Rgb {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return fallback;
    }
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    match (p(0), p(2), p(4)) {
        (Some(r), Some(g), Some(b)) => (r, g, b),
        _ => fallback,
    }
}
