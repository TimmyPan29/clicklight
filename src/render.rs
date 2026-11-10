//! Drawing primitives. All colors are emitted in BGRA order so tiny-skia can paint
//! directly into the Windows DIB section used by UpdateLayeredWindow (zero copies).

use std::time::{Duration, Instant};
use tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, PixmapMut, Stroke, Transform};

pub type Rgb = (u8, u8, u8);

pub fn col(c: Rgb, a: f32) -> Color {
    // Swap R/B: tiny-skia writes RGBA, Windows wants BGRA.
    Color::from_rgba8(c.2, c.1, c.0, (a.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn lighten(c: Rgb, t: f32) -> Rgb {
    let f = |v: u8| (v as f32 + (255.0 - v as f32) * t) as u8;
    (f(c.0), f(c.1), f(c.2))
}

fn paint(c: Rgb, a: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(col(c, a));
    p.anti_alias = true;
    p
}

fn fill(pm: &mut PixmapMut, path: &Path, c: Rgb, a: f32) {
    if a > 0.0 {
        pm.fill_path(path, &paint(c, a), FillRule::Winding, Transform::identity(), None);
    }
}

fn stroke(pm: &mut PixmapMut, path: &Path, c: Rgb, a: f32, width: f32) {
    if a > 0.0 && width > 0.0 {
        let s = Stroke { width, ..Stroke::default() };
        pm.stroke_path(path, &paint(c, a), &s, Transform::identity(), None);
    }
}

pub fn rounded_rect(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Option<Path> {
    let (w, h) = (x1 - x0, y1 - y0);
    if w <= 0.5 || h <= 0.5 {
        return None;
    }
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    pb.quad_to(x1, y0, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.quad_to(x1, y1, x1 - r, y1);
    pb.line_to(x0 + r, y1);
    pb.quad_to(x0, y1, x0, y1 - r);
    pb.line_to(x0, y0 + r);
    pb.quad_to(x0, y0, x0 + r, y0);
    pb.close();
    pb.finish()
}

// ---------------------------------------------------------------- ripple

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RippleKind {
    Left,
    Right,
    Middle,
}

pub struct Ripple {
    pub x: f32,
    pub y: f32,
    pub start: Instant,
    pub dur: Duration,
    pub size: f32,
    pub color: Rgb,
    pub kind: RippleKind,
}

impl Ripple {
    pub fn progress(&self, now: Instant) -> f32 {
        let d = self.dur.as_secs_f32().max(0.001);
        (now.saturating_duration_since(self.start).as_secs_f32() / d).min(1.0)
    }
    pub fn done(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }
}

pub fn draw_ripple(pm: &mut PixmapMut, r: &Ripple, now: Instant) {
    let p = r.progress(now);
    if p >= 1.0 {
        return;
    }
    let eased = 1.0 - (1.0 - p).powi(3);
    let fade = 1.0 - eased;
    let bs = r.size;

    // soft aura
    if let Some(c) = PathBuilder::from_circle(r.x, r.y, bs * (0.28 + 0.85 * eased)) {
        fill(pm, &c, r.color, fade * 0.22);
    }
    // main ring + bright inner highlight
    let ring_r = bs * (0.18 + 0.72 * eased);
    let ring_w = (bs * 0.08 * (1.0 - 0.4 * p)).max(2.0);
    if let Some(c) = PathBuilder::from_circle(r.x, r.y, ring_r) {
        stroke(pm, &c, r.color, fade * 0.88, ring_w);
        stroke(pm, &c, lighten(r.color, 0.6), fade * 0.80, (ring_w * 0.45).max(1.0));
    }
    // spark core
    if let Some(c) = PathBuilder::from_circle(r.x, r.y, (bs * 0.10 * (1.0 - p)).max(1.5)) {
        fill(pm, &c, (255, 255, 255), fade * 0.95);
    }
    // right click crosshair
    if r.kind == RippleKind::Right {
        let ch = bs * (0.16 + 0.08 * fade);
        let mut pb = PathBuilder::new();
        pb.move_to(r.x - ch, r.y);
        pb.line_to(r.x + ch, r.y);
        pb.move_to(r.x, r.y - ch);
        pb.line_to(r.x, r.y + ch);
        if let Some(path) = pb.finish() {
            stroke(pm, &path, (255, 255, 255), fade * 0.75, 1.5);
        }
    }
}

// ---------------------------------------------------------------- rectangle fade pen

pub struct RectStyle {
    pub color: Rgb,
    pub stroke_width: f32,
    pub fill_opacity: f32,
    pub corner_radius: f32,
    pub fade_delay: Duration,
    pub fade_dur: Duration,
}

pub struct RectStroke {
    pub a: (f32, f32),
    pub b: (f32, f32),
    pub released: Option<Instant>,
}

impl RectStroke {
    pub fn alpha(&self, st: &RectStyle, now: Instant) -> f32 {
        let Some(t) = self.released else { return 1.0 };
        let e = now.saturating_duration_since(t);
        if e <= st.fade_delay {
            return 1.0;
        }
        let p = ((e - st.fade_delay).as_secs_f32() / st.fade_dur.as_secs_f32().max(0.001)).min(1.0);
        1.0 - p * p * (3.0 - 2.0 * p) // smoothstep fade-out
    }
}

pub fn draw_rect(pm: &mut PixmapMut, r: &RectStroke, st: &RectStyle, now: Instant, _active: bool) {
    let al = r.alpha(st, now);
    if al <= 0.0 {
        return;
    }
    let (x0, x1) = (r.a.0.min(r.b.0), r.a.0.max(r.b.0));
    let (y0, y1) = (r.a.1.min(r.b.1), r.a.1.max(r.b.1));
    let Some(path) = rounded_rect(x0, y0, x1, y1, st.corner_radius) else { return };
    let sw = st.stroke_width;
    let c = st.color;

    fill(pm, &path, c, st.fill_opacity * al);
    stroke(pm, &path, c, 0.12 * al, sw * 5.0); // wide ambient glow
    stroke(pm, &path, c, 0.28 * al, sw * 2.4); // inner glow
    stroke(pm, &path, c, 0.95 * al, sw); // neon body
    stroke(pm, &path, lighten(c, 0.75), 0.90 * al, (sw * 0.35).max(1.0)); // hot core
}

// ---------------------------------------------------------------- HUD

pub struct Toast {
    pub text: String,
    pub start: Instant,
    pub dur: Duration,
    pub accent: Rgb,
}

impl Toast {
    pub fn alpha(&self, now: Instant) -> f32 {
        let e = now.saturating_duration_since(self.start).as_secs_f32();
        let d = self.dur.as_secs_f32();
        if e < 0.18 {
            e / 0.18
        } else if e > d - 0.3 {
            ((d - e) / 0.3).max(0.0)
        } else {
            1.0
        }
    }
    pub fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) >= self.dur
    }
}

pub fn draw_panel(pm: &mut PixmapMut, x: f32, y: f32, w: f32, h: f32, accent: Rgb, a: f32) {
    if let Some(p) = rounded_rect(x, y, x + w, y + h, h / 2.0) {
        fill(pm, &p, (18, 18, 24), 0.86 * a);
        stroke(pm, &p, accent, 0.85 * a, 1.5);
    }
}

pub fn draw_rect_icon(pm: &mut PixmapMut, x: f32, y: f32, s: f32, accent: Rgb, a: f32) {
    if let Some(p) = rounded_rect(x, y, x + s, y + s * 0.78, 2.0) {
        fill(pm, &p, accent, 0.18 * a);
        stroke(pm, &p, accent, a, 1.6);
    }
}
