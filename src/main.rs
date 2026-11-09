//! ClickLight (Rust) — passive screen-annotation overlay for Windows.
//!
//! * Normal mode     : left-click => ripple. Dragging draws nothing.
//! * Rect fade pen   : toggled by a configurable hotkey. Any left-button drag —
//!                     including a three-finger drag (ThreeFingersDrag / precision
//!                     touchpad emulation) — draws a glowing rectangle that fades
//!                     out after release.
//!
//! The overlay is a full-virtual-screen WS_EX_LAYERED | WS_EX_TRANSPARENT window, so it
//! never takes focus or intercepts input. Input is observed through a WH_MOUSE_LL hook.

#![windows_subsystem = "windows"]

mod config;
mod hotkey;
mod render;
mod text;

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
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
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use config::Config;
use render::{RectStroke, RectStyle, Rgb, Ripple, Toast};

const CLASS_NAME: &str = "ClickLightRsOverlay";
const MUTEX_NAME: &str = "Local\\ClickLightRsSingleInstance";

const WM_APP_MOUSE: u32 = WM_APP + 1;
const WM_APP_CMD: u32 = WM_APP + 2;

const EV_DOWN: usize = 0;
const EV_UP: usize = 1;
const EV_MOVE: usize = 2;

const CMD_QUIT: usize = 1;
const CMD_RELOAD: usize = 2;
const CMD_TOGGLE_RECT: usize = 3;
const CMD_TOGGLE: usize = 4;
const CMD_CLEAR: usize = 5;

const HK_RECT: i32 = 1;
const HK_TOGGLE: i32 = 2;
const HK_CLEAR: i32 = 3;
const HK_QUIT: i32 = 4;

const TIMER_ID: usize = 1;
const FRAME_MS: u32 = 15;

// Shared with the low-level mouse hook (which must decide synchronously and return fast).
static OVERLAY: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static TRACK_MOVES: AtomicBool = AtomicBool::new(false);
static BLOCK_CLICKS: AtomicBool = AtomicBool::new(false); // pen on + block_clicks
static SWALLOWING: AtomicBool = AtomicBool::new(false); // swallowed a DOWN, owe an UP

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
    // primary monitor, in overlay-local coordinates (for HUD placement)
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
    timer_on: bool,
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
            timer_on: false,
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

    fn sync_hook_flags(&self) {
        let pen = self.enabled && self.rect_mode;
        BLOCK_CLICKS.store(pen && self.cfg.rect_pen.block_clicks, Ordering::Relaxed);
        if !pen {
            TRACK_MOVES.store(false, Ordering::Relaxed);
        }
    }

    fn toast(&mut self, text: impl Into<String>, accent: Rgb) {
        self.toast = Some(Toast {
            text: text.into(),
            start: Instant::now(),
            dur: Duration::from_millis(1700),
            accent,
        });
        self.kick();
    }

    // ---------------------------------------------------------------- hotkeys

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
        self.sync_hook_flags();
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
        self.sync_hook_flags();
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
        self.sync_hook_flags();
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

    // ---------------------------------------------------------------- mouse

    fn spawn_ripple(&mut self, x: f32, y: f32) {
        let r = &self.cfg.ripple;
        if !r.enabled {
            return;
        }
        self.ripples.push(Ripple {
            x,
            y,
            start: Instant::now(),
            dur: Duration::from_millis(r.duration_ms.max(50)),
            size: r.size.max(4.0),
            color: config::parse_color(&r.color, (255, 41, 5)),
        });
    }

    fn on_mouse(&mut self, ev: usize, sx: i32, sy: i32) {
        if !self.enabled {
            return;
        }
        let x = (sx - self.s.ox) as f32;
        let y = (sy - self.s.oy) as f32;
        match ev {
            EV_DOWN => {
                self.bring_to_top();
                if self.rect_mode {
                    self.active = Some(RectStroke { a: (x, y), b: (x, y), released: None });
                    TRACK_MOVES.store(true, Ordering::Relaxed);
                } else {
                    self.spawn_ripple(x, y);
                }
                self.kick();
            }
            EV_MOVE => {
                if let Some(a) = self.active.as_mut() {
                    a.b = (x, y); // the frame timer picks it up (coalesces 1000 Hz mice)
                }
            }
            EV_UP => {
                TRACK_MOVES.store(false, Ordering::Relaxed);
                if let Some(mut a) = self.active.take() {
                    a.b = (x, y);
                    let min = self.cfg.rect_pen.min_size;
                    if (a.b.0 - a.a.0).abs() < min && (a.b.1 - a.a.1).abs() < min {
                        self.spawn_ripple(x, y); // tiny drag == click
                    } else {
                        a.released = Some(Instant::now());
                        self.rects.push(a);
                        if self.cfg.rect_pen.oneshot() {
                            self.rect_mode = false;
                            self.sync_hook_flags();
                        }
                    }
                    self.kick();
                }
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------- frame loop

    fn bring_to_top(&self) {
        unsafe {
            SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }

    fn kick(&mut self) {
        self.last_frame_empty = false;
        if !self.timer_on {
            unsafe { SetTimer(self.hwnd, TIMER_ID, FRAME_MS, None) };
            self.timer_on = true;
        }
        self.render();
    }

    fn stop_timer(&mut self) {
        if self.timer_on {
            unsafe { KillTimer(self.hwnd, TIMER_ID) };
            self.timer_on = false;
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
        self.kick();
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
            self.stop_timer();
            return;
        }

        let (w, h) = (self.s.w, self.s.h);
        if self.s.bits.is_null() || w <= 0 || h <= 0 {
            return;
        }
        let accent = self.accent();
        let cx = self.s.prim_cx;
        let top = self.s.prim_top;

        // HUD geometry (needs text metrics before we borrow the pixel buffer)
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

        // SAFETY: `bits` is the DIB section owned by `self.s`, sized w*h*4.
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
        if !animating {
            self.stop_timer();
        }
    }

    fn shutdown(&mut self) {
        self.stop_timer();
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
        let mut bits: *mut c_void = null_mut();
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

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lparam as *const MSLLHOOKSTRUCT);
        let ev = match wparam as u32 {
            WM_LBUTTONDOWN => Some(EV_DOWN),
            WM_LBUTTONUP => Some(EV_UP),
            WM_MOUSEMOVE if TRACK_MOVES.load(Ordering::Relaxed) => Some(EV_MOVE),
            _ => None,
        };
        if let Some(ev) = ev {
            let hwnd = OVERLAY.load(Ordering::Relaxed);
            let packed = ((info.pt.y as u16 as u32) << 16 | (info.pt.x as u16 as u32)) as LPARAM;
            PostMessageW(hwnd, WM_APP_MOUSE, ev, packed);

            // Pen mode with block_clicks: swallow the button so apps underneath
            // don't select / drag while we draw. Mouse *moves* are never swallowed.
            match ev {
                EV_DOWN if BLOCK_CLICKS.load(Ordering::Relaxed) => {
                    SWALLOWING.store(true, Ordering::Relaxed);
                    return 1;
                }
                EV_UP if SWALLOWING.swap(false, Ordering::Relaxed) => return 1,
                _ => {}
            }
        }
    }
    CallNextHookEx(null_mut(), code, wparam, lparam)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => return -1, // HTTRANSPARENT
        WM_DESTROY => {
            PostQuitMessage(0);
            return 0;
        }
        _ => {}
    }
    let handled = APP.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else { return false };
        let Some(app) = guard.as_mut() else { return false };
        match msg {
            WM_APP_MOUSE => {
                let x = (lp & 0xFFFF) as u16 as i16 as i32;
                let y = ((lp >> 16) & 0xFFFF) as u16 as i16 as i32;
                app.on_mouse(wp, x, y);
            }
            WM_HOTKEY => app.on_hotkey(wp as i32),
            WM_TIMER => app.render(),
            WM_APP_CMD => app.on_cmd(wp),
            WM_DISPLAYCHANGE => app.rebuild_surface(),
            _ => return false,
        }
        true
    });
    if handled {
        0
    } else {
        DefWindowProcW(hwnd, msg, wp, lp)
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
            // Already running: forward the command (if any) and exit.
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

        let title = wide("ClickLight Overlay");
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        if hwnd.is_null() {
            return;
        }
        OVERLAY.store(hwnd, Ordering::Relaxed);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        let mut app = App::new(hwnd, cfg);
        let mut errors = app.register_hotkeys();
        if let Some(e) = cfg_err {
            errors.insert(0, e);
        }
        app.sync_hook_flags();
        if errors.is_empty() {
            let hk = app.cfg.hotkeys.rect_pen.clone();
            let msg = if hk.is_empty() { "ClickLight ready".to_string() } else { format!("ClickLight ready  ·  Rect Pen: {hk}") };
            app.toast(msg, app.accent());
        } else {
            app.toast(errors.join("  ·  "), (255, 170, 0));
        }
        APP.with(|c| *c.borrow_mut() = Some(app));

        let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), hinst, 0);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        if !hook.is_null() {
            UnhookWindowsHookEx(hook);
        }
        APP.with(|c| {
            if let Some(mut app) = c.borrow_mut().take() {
                app.shutdown();
            }
        });
        DestroyWindow(hwnd);
    }
}
