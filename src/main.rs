//! ClickLight (Rust) — passive screen-annotation overlay for Windows.
//!
//! * Normal mode     : left-click => ripple. Dragging draws nothing.
//! * Rect fade pen   : toggled by shortcut. Any left-button drag (including
//!                     three-finger drag) draws a glowing rectangle that fades out.
//!
//! 100% passive, zero hooks, never intercepts, delays, or swallows clicks.

#![windows_subsystem = "windows"]

mod config;
mod hotkey;
mod render;
mod text;

use std::cell::RefCell;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use tiny_skia::{Color, PixmapMut};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject, AC_SRC_ALPHA,
    AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use config::Config;
use render::{RectStroke, RectStyle, Rgb, Ripple, RippleKind, Toast};

const CLASS_NAME: &str = "ClickLightRsOverlay";
const MUTEX_NAME: &str = "Local\\ClickLightRsSingleInstance";

const WM_APP_CMD: u32 = WM_APP + 2;

const CMD_QUIT: usize = 1;
const CMD_RELOAD: usize = 2;
const CMD_TOGGLE_RECT: usize = 3;
const CMD_TOGGLE: usize = 4;
const CMD_CLEAR: usize = 5;

const HK_RECT: i32 = 1;
const HK_TOGGLE: i32 = 2;
const HK_CLEAR: i32 = 3;
const HK_QUIT: i32 = 4;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ======================================================================= App

struct Surface {
    hdc_mem: HDC,
    hbmp: HBITMAP,
    bits: *mut u8,
    ox: i32,
    oy: i32,
    w: i32,
    h: i32,
    prim_cx: f32,
    prim_top: f32,
}

struct App {
    hwnd: HWND,
    hdc_screen: HDC,
    s: Surface,
    cfg: Config,
    enabled: bool,
    rect_mode: bool,
    ripples: Vec<Ripple>,
    rects: Vec<RectStroke>,
    active: Option<RectStroke>,
    toast: Option<Toast>,
    last_frame_empty: bool,
    text: text::TextRenderer,
}

impl App {
    fn new(hwnd: HWND, cfg: Config) -> Self {
        let hdc_screen = unsafe { GetDC(null_mut()) };
        let s = unsafe { Surface::create(hdc_screen) };
        Self {
            hwnd,
            hdc_screen,
            s,
            cfg,
            enabled: true,
            rect_mode: false,
            ripples: Vec::new(),
            rects: Vec::new(),
            active: None,
            toast: None,
            last_frame_empty: false,
            text: text::TextRenderer::new(),
        }
    }

    fn accent(&self) -> Rgb {
        config::parse_color(&self.cfg.rect_pen.color, (255, 41, 5))
    }

    fn rect_style(&self) -> RectStyle {
        let r = &self.cfg.rect_pen;
        RectStyle {
            color: self.accent(),
            stroke_width: r.stroke_width.max(0.5),
            fill_opacity: r.fill_opacity.clamp(0.0, 1.0),
            corner_radius: r.corner_radius.max(0.0),
            fade_delay: Duration::from_millis(r.fade_delay_ms),
            fade_dur: Duration::from_millis(r.fade_duration_ms),
        }
    }

    fn toast(&mut self, text: impl Into<String>, accent: Rgb) {
        self.toast = Some(Toast {
            text: text.into(),
            start: Instant::now(),
            dur: Duration::from_millis(1700),
            accent,
        });
        self.last_frame_empty = false;
    }

    fn register_hotkeys(&mut self) -> Vec<String> {
        let hk = self.cfg.hotkeys.clone();
        let mut errors = Vec::new();
        for (id, spec) in [
            (HK_RECT, &hk.rect_pen),
            (HK_TOGGLE, &hk.toggle_clicklight),
            (HK_CLEAR, &hk.clear),
            (HK_QUIT, &hk.quit),
        ] {
            match hotkey::parse(spec) {
                Ok(Some((mods, vk))) => {
                    let ok = unsafe { RegisterHotKey(self.hwnd, id, mods | hotkey::MOD_NOREPEAT, vk) };
                    if ok == 0 {
                        errors.push(format!("Hotkey in use: {spec}"));
                    }
                }
                Ok(None) => {}
                Err(e) => errors.push(format!("Bad hotkey {e}")),
            }
        }
        errors
    }

    fn unregister_hotkeys(&self) {
        for id in [HK_RECT, HK_TOGGLE, HK_CLEAR, HK_QUIT] {
            unsafe { UnregisterHotKey(self.hwnd, id) };
        }
    }

    fn reload(&mut self) {
        let (cfg, err) = config::load(&config::config_path());
        self.unregister_hotkeys();
        self.cfg = cfg;
        let mut errors = self.register_hotkeys();
        if let Some(e) = err {
            errors.insert(0, e);
        }
        if errors.is_empty() {
            self.toast("Config reloaded", self.accent());
        } else {
            self.toast(errors.join("  ·  "), (255, 170, 0));
        }
    }

    fn set_rect_mode(&mut self, on: bool) {
        self.rect_mode = on;
        if on {
            self.enabled = true;
        } else {
            self.active = None;
        }
        let label = if on { "Rectangle Pen: ON" } else { "Rectangle Pen: OFF" };
        let c = if on { self.accent() } else { (136, 136, 136) };
        self.toast(label, c);
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            self.rect_mode = false;
            self.ripples.clear();
            self.rects.clear();
            self.active = None;
        }
        let c = if on { self.accent() } else { (136, 136, 136) };
        self.toast(if on { "ClickLight: ON" } else { "ClickLight: OFF" }, c);
    }

    fn clear(&mut self) {
        self.ripples.clear();
        self.rects.clear();
        self.active = None;
        self.toast("Cleared", self.accent());
    }

    fn on_hotkey(&mut self, id: i32) {
        match id {
            HK_RECT => self.set_rect_mode(!self.rect_mode),
            HK_TOGGLE => self.set_enabled(!self.enabled),
            HK_CLEAR => self.clear(),
            HK_QUIT => unsafe { PostQuitMessage(0) },
            _ => {}
        }
    }

    fn on_cmd(&mut self, cmd: usize) {
        match cmd {
            CMD_QUIT => unsafe { PostQuitMessage(0) },
            CMD_RELOAD => self.reload(),
            CMD_TOGGLE_RECT => self.set_rect_mode(!self.rect_mode),
            CMD_TOGGLE => self.set_enabled(!self.enabled),
            CMD_CLEAR => self.clear(),
            _ => {}
        }
    }

    fn spawn_ripple(&mut self, x: f32, y: f32, kind: RippleKind) {
        let r = &self.cfg.ripple;
        if !r.enabled {
            return;
        }
        let (color, dur_ms) = match kind {
            RippleKind::Right => ((255, 117, 48), 480),   // Sunset Orange
            RippleKind::Middle => ((69, 235, 169), 480),  // Mint Emerald
            RippleKind::Left => (config::parse_color(&r.color, (255, 41, 5)), r.duration_ms.max(50)),
        };
        self.ripples.push(Ripple {
            x,
            y,
            start: Instant::now(),
            dur: Duration::from_millis(dur_ms),
            size: r.size.max(4.0),
            color,
            kind,
        });
        self.last_frame_empty = false;
    }

    fn bring_to_top(&self) {
        unsafe {
            SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }

    fn rebuild_surface(&mut self) {
        unsafe {
            self.s.destroy();
            self.s = Surface::create(self.hdc_screen);
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                self.s.ox,
                self.s.oy,
                self.s.w,
                self.s.h,
                SWP_NOACTIVATE,
            );
        }
        self.last_frame_empty = false;
    }

    fn has_active_elements(&self) -> bool {
        !self.ripples.is_empty()
            || !self.rects.is_empty()
            || self.active.is_some()
            || self.toast.is_some()
            || (self.enabled && self.rect_mode && self.cfg.rect_pen.show_indicator)
    }

    fn render(&mut self) {
        let now = Instant::now();
        let st = self.rect_style();
        self.ripples.retain(|r| !r.done(now));
        self.rects.retain(|r| r.alpha(&st, now) > 0.0);
        if self.toast.as_ref().is_some_and(|t| t.done(now)) {
            self.toast = None;
        }

        let show_pill = self.enabled && self.rect_mode && self.cfg.rect_pen.show_indicator;
        let animating =
            !self.ripples.is_empty() || !self.rects.is_empty() || self.active.is_some() || self.toast.is_some();
        let empty = !animating && !show_pill;

        if empty && self.last_frame_empty {
            return;
        }

        let (w, h) = (self.s.w, self.s.h);
        if self.s.bits.is_null() || w <= 0 || h <= 0 {
            return;
        }
        let accent = self.accent();
        let cx = self.s.prim_cx;
        let top = self.s.prim_top;

        const PILL_PX: f32 = 12.0;
        const TOAST_PX: f32 = 15.0;
        let pill = show_pill.then(|| {
            let label = "RECT PEN";
            let tw = self.text.measure(label, PILL_PX);
            let pw = 12.0 + 12.0 + 7.0 + tw + 14.0;
            (label, cx - pw / 2.0, top + 10.0, pw, 26.0)
        });
        let toast = self.toast.as_ref().map(|t| (t.text.clone(), t.alpha(now), t.accent));
        let toast = toast.map(|(txt, a, c)| {
            let tw = self.text.measure(&txt, TOAST_PX);
            let bw = tw + 44.0;
            (txt, a, c, cx - bw / 2.0, top + 46.0, bw, 38.0, tw)
        });

        let buf = unsafe { std::slice::from_raw_parts_mut(self.s.bits, (w * h * 4) as usize) };
        {
            let mut pm = PixmapMut::from_bytes(buf, w as u32, h as u32).expect("pixmap");
            pm.fill(Color::TRANSPARENT);
            for r in &self.ripples {
                render::draw_ripple(&mut pm, r, now);
            }
            for r in &self.rects {
                render::draw_rect(&mut pm, r, &st, now, false);
            }
            if let Some(a) = &self.active {
                render::draw_rect(&mut pm, a, &st, now, true);
            }
            if let Some((_, x, y, pw, ph)) = pill {
                render::draw_panel(&mut pm, x, y, pw, ph, accent, 1.0);
                render::draw_rect_icon(&mut pm, x + 12.0, y + 8.0, 12.0, accent, 1.0);
            }
            if let Some((_, a, c, x, y, bw, bh, _)) = &toast {
                render::draw_panel(&mut pm, *x, *y, *bw, *bh, *c, *a);
            }
        }
        if let Some((label, x, y, _, ph)) = pill {
            let base = self.text.centered_baseline(y, ph, PILL_PX);
            self.text.draw(buf, w, h, label, PILL_PX, x + 12.0 + 12.0 + 7.0, base, (255, 255, 255), 0.95);
        }
        if let Some((txt, a, _, x, y, bw, bh, tw)) = &toast {
            let base = self.text.centered_baseline(*y, *bh, TOAST_PX);
            self.text.draw(buf, w, h, txt, TOAST_PX, x + (bw - tw) / 2.0, base, (255, 255, 255), 0.98 * a);
        }

        unsafe {
            let dst = POINT { x: self.s.ox, y: self.s.oy };
            let size = SIZE { cx: w, cy: h };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            UpdateLayeredWindow(self.hwnd, self.hdc_screen, &dst, &size, self.s.hdc_mem, &src, 0, &blend, ULW_ALPHA);
        }

        self.last_frame_empty = empty;
    }

    fn shutdown(&mut self) {
        self.unregister_hotkeys();
        unsafe {
            self.s.destroy();
            ReleaseDC(null_mut(), self.hdc_screen);
        }
    }
}

impl Surface {
    unsafe fn create(hdc_screen: HDC) -> Self {
        let ox = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let oy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let mut w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let mut h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if w <= 0 || h <= 0 {
            w = GetSystemMetrics(SM_CXSCREEN);
            h = GetSystemMetrics(SM_CYSCREEN);
        }
        let prim_w = GetSystemMetrics(SM_CXSCREEN);

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h; // top-down
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;

        let hdc_mem = CreateCompatibleDC(hdc_screen);
        let mut bits: *mut std::ffi::c_void = null_mut();
        let hbmp = CreateDIBSection(hdc_mem, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        SelectObject(hdc_mem, hbmp);

        Surface {
            hdc_mem,
            hbmp,
            bits: bits as *mut u8,
            ox,
            oy,
            w,
            h,
            prim_cx: (-ox) as f32 + prim_w as f32 / 2.0,
            prim_top: (-oy) as f32,
        }
    }

    unsafe fn destroy(&mut self) {
        if !self.hbmp.is_null() {
            DeleteObject(self.hbmp);
            self.hbmp = null_mut();
        }
        if !self.hdc_mem.is_null() {
            DeleteDC(self.hdc_mem);
            self.hdc_mem = null_mut();
        }
        self.bits = null_mut();
    }
}

// ======================================================================= Win32 plumbing

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => return -1, // HTTRANSPARENT -> 100% click-through!
        WM_DESTROY => {
            PostQuitMessage(0);
            return 0;
        }
        WM_HOTKEY => {
            APP.with(|cell| {
                if let Ok(mut guard) = cell.try_borrow_mut() {
                    if let Some(app) = guard.as_mut() {
                        app.on_hotkey(wp as i32);
                    }
                }
            });
            return 0;
        }
        WM_APP_CMD => {
            APP.with(|cell| {
                if let Ok(mut guard) = cell.try_borrow_mut() {
                    if let Some(app) = guard.as_mut() {
                        app.on_cmd(wp);
                    }
                }
            });
            return 0;
        }
        WM_DISPLAYCHANGE => {
            APP.with(|cell| {
                if let Ok(mut guard) = cell.try_borrow_mut() {
                    if let Some(app) = guard.as_mut() {
                        app.rebuild_surface();
                    }
                }
            });
            return 0;
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn main() {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

    let arg = std::env::args().nth(1).unwrap_or_default();
    let cfg_path = config::config_path();
    if arg == "--config" {
        config::ensure_exists(&cfg_path);
        let _ = std::process::Command::new("notepad.exe").arg(&cfg_path).spawn();
        return;
    }
    let cmd = match arg.as_str() {
        "--quit" | "--stop" => Some(CMD_QUIT),
        "--reload" => Some(CMD_RELOAD),
        "--toggle-rect" => Some(CMD_TOGGLE_RECT),
        "--toggle" => Some(CMD_TOGGLE),
        "--clear" => Some(CMD_CLEAR),
        _ => None,
    };

    let class = wide(CLASS_NAME);
    unsafe {
        let mutex_name = wide(MUTEX_NAME);
        let _mutex = CreateMutexW(null(), 0, mutex_name.as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Some(c) = cmd {
                let other = FindWindowW(class.as_ptr(), null());
                if !other.is_null() {
                    PostMessageW(other, WM_APP_CMD, c, 0);
                }
            }
            return;
        }
        if cmd == Some(CMD_QUIT) {
            return;
        }

        let (cfg, cfg_err) = config::load(&cfg_path);
        let hinst = GetModuleHandleW(null());

        let mut wc: WNDCLASSEXW = std::mem::zeroed();
        wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinst;
        wc.lpszClassName = class.as_ptr();
        RegisterClassExW(&wc);

        let ox = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let oy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let mut w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let mut h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if w <= 0 || h <= 0 {
            w = GetSystemMetrics(SM_CXSCREEN);
            h = GetSystemMetrics(SM_CYSCREEN);
        }

        let title = wide("ClickLight Overlay");
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            ox,
            oy,
            w,
            h,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        if hwnd.is_null() {
            return;
        }
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        let mut app = App::new(hwnd, cfg);
        let mut errors = app.register_hotkeys();
        if let Some(e) = cfg_err {
            errors.insert(0, e);
        }
        if errors.is_empty() {
            let hk = app.cfg.hotkeys.rect_pen.clone();
            let msg = if hk.is_empty() {
                "ClickLight ready".to_string()
            } else {
                format!("ClickLight ready  ·  Rect Pen: {hk}")
            };
            app.toast(msg, app.accent());
        } else {
            app.toast(errors.join("  ·  "), (255, 170, 0));
        }
        APP.with(|c| *c.borrow_mut() = Some(app));

        let mut msg: MSG = std::mem::zeroed();
        let mut prev_l = false;
        let mut prev_r = false;
        let mut prev_m = false;

        loop {
            while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                if msg.message == WM_QUIT {
                    APP.with(|c| {
                        if let Some(mut app) = c.borrow_mut().take() {
                            app.shutdown();
                        }
                    });
                    DestroyWindow(hwnd);
                    return;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            let (has_active, is_down) = APP.with(|cell| {
                let Ok(mut guard) = cell.try_borrow_mut() else { return (false, false) };
                let Some(app) = guard.as_mut() else { return (false, false) };

                if app.enabled {
                    let mut pt = POINT { x: 0, y: 0 };
                    GetCursorPos(&mut pt);
                    let cx = (pt.x - app.s.ox) as f32;
                    let cy = (pt.y - app.s.oy) as f32;

                    // 1. Left button
                    let down_l = (GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8000) != 0;
                    if down_l && !prev_l {
                        app.bring_to_top();
                        if app.rect_mode {
                            app.active = Some(RectStroke { a: (cx, cy), b: (cx, cy), released: None });
                        } else {
                            app.spawn_ripple(cx, cy, RippleKind::Left);
                        }
                    } else if down_l && prev_l {
                        if app.rect_mode {
                            if let Some(a) = app.active.as_mut() {
                                if (cx - a.b.0).hypot(cy - a.b.1) >= 2.0 {
                                    a.b = (cx, cy);
                                }
                            }
                        }
                    } else if !down_l && prev_l {
                        if app.rect_mode {
                            if let Some(mut a) = app.active.take() {
                                a.b = (cx, cy);
                                let min = app.cfg.rect_pen.min_size;
                                if (a.b.0 - a.a.0).abs() < min && (a.b.1 - a.a.1).abs() < min {
                                    app.spawn_ripple(cx, cy, RippleKind::Left);
                                } else {
                                    a.released = Some(Instant::now());
                                    app.rects.push(a);
                                    if app.cfg.rect_pen.oneshot() {
                                        app.rect_mode = false;
                                    }
                                }
                            }
                        }
                    }
                    prev_l = down_l;

                    // 2. Right button
                    let down_r = (GetAsyncKeyState(VK_RBUTTON as i32) as u16 & 0x8000) != 0;
                    if down_r && !prev_r {
                        app.bring_to_top();
                        app.spawn_ripple(cx, cy, RippleKind::Right);
                    }
                    prev_r = down_r;

                    // 3. Middle button
                    let down_m = (GetAsyncKeyState(VK_MBUTTON as i32) as u16 & 0x8000) != 0;
                    if down_m && !prev_m {
                        app.bring_to_top();
                        app.spawn_ripple(cx, cy, RippleKind::Middle);
                    }
                    prev_m = down_m;
                } else {
                    prev_l = false;
                    prev_r = false;
                    prev_m = false;
                }

                app.render();
                (app.has_active_elements(), prev_l)
            });

            std::thread::sleep(Duration::from_millis(if has_active || is_down { 13 } else { 20 }));
        }
    }
}
