//! Minimal anti-aliased text rendering straight into a premultiplied BGRA buffer.

use fontdue::{Font, FontSettings, Metrics};
use std::collections::HashMap;

use crate::render::Rgb;

pub struct TextRenderer {
    font: Option<Font>,
    cache: HashMap<(char, u32), (Metrics, Vec<u8>)>,
}

impl TextRenderer {
    pub fn new() -> Self {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
        let font = ["segoeuib.ttf", "segoeui.ttf", "arialbd.ttf", "arial.ttf"]
            .iter()
            .find_map(|f| std::fs::read(format!("{windir}\\Fonts\\{f}")).ok())
            .and_then(|bytes| Font::from_bytes(bytes, FontSettings::default()).ok());
        Self { font, cache: HashMap::new() }
    }

    fn glyph(&mut self, c: char, px: f32) -> Option<&(Metrics, Vec<u8>)> {
        let font = self.font.as_ref()?;
        let key = (c, px.to_bits());
        Some(self.cache.entry(key).or_insert_with(|| font.rasterize(c, px)))
    }

    pub fn measure(&mut self, s: &str, px: f32) -> f32 {
        s.chars().filter_map(|c| self.glyph(c, px).map(|g| g.0.advance_width)).sum()
    }

    /// Baseline that vertically centers a line of `px` text inside [top, top+height].
    pub fn centered_baseline(&self, top: f32, height: f32, px: f32) -> f32 {
        match self.font.as_ref().and_then(|f| f.horizontal_line_metrics(px)) {
            Some(m) => top + (height - (m.ascent - m.descent)) / 2.0 + m.ascent,
            None => top + height / 2.0 + px * 0.35,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        buf: &mut [u8],
        w: i32,
        h: i32,
        s: &str,
        px: f32,
        x: f32,
        baseline: f32,
        color: Rgb,
        alpha: f32,
    ) {
        let mut pen = x;
        let base = baseline.round() as i32;
        for c in s.chars() {
            let Some((m, bmp)) = self.glyph(c, px) else { return };
            let (m, bmp) = (*m, bmp.clone());
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = base - m.height as i32 - m.ymin;
            for row in 0..m.height as i32 {
                let y = gy + row;
                if y < 0 || y >= h {
                    continue;
                }
                for col in 0..m.width as i32 {
                    let xx = gx + col;
                    if xx < 0 || xx >= w {
                        continue;
                    }
                    let cov = bmp[(row * m.width as i32 + col) as usize] as f32 / 255.0;
                    let a = cov * alpha;
                    if a <= 0.0 {
                        continue;
                    }
                    let i = ((y * w + xx) * 4) as usize;
                    let inv = 1.0 - a;
                    // BGRA, premultiplied
                    buf[i] = (color.2 as f32 * a + buf[i] as f32 * inv) as u8;
                    buf[i + 1] = (color.1 as f32 * a + buf[i + 1] as f32 * inv) as u8;
                    buf[i + 2] = (color.0 as f32 * a + buf[i + 2] as f32 * inv) as u8;
                    buf[i + 3] = (255.0 * a + buf[i + 3] as f32 * inv) as u8;
                }
            }
            pen += m.advance_width;
        }
    }
}
