//! Lyric layer renderer. Draws one slide of lyrics into a premultiplied BGRA
//! image sized to the text, positioned in the lower third of the frame.
//! It runs only when the slide changes, never per frame.

use ab_glyph::{Font, FontRef, PxScale, PxScaleFont, ScaleFont, point};

type Scaled<'a, 'f> = PxScaleFont<&'a FontRef<'f>>;

pub struct Style {
    /// Text height as a fraction of frame height (0.06 = 65px at 1080p).
    pub size: f32,
    /// Gap between the text block and the bottom edge, as a fraction of frame height.
    pub bottom_margin: f32,
}

impl Default for Style {
    fn default() -> Self {
        Self { size: 0.06, bottom_margin: 0.08 }
    }
}

/// A rendered layer: premultiplied BGRA pixels plus where to place them.
pub struct Layer {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Renders `lines` for a `frame_w` x `frame_h` frame. Returns None for a blank slide.
pub fn render(font: &FontRef, lines: &[&str], frame_w: u32, frame_h: u32, style: &Style) -> Option<Layer> {
    let lines: Vec<&str> = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() || frame_w == 0 || frame_h == 0 {
        return None;
    }
    let px = (frame_h as f32 * style.size).max(8.0);
    let sf = font.as_scaled(PxScale::from(px));
    let line_h = (sf.ascent() - sf.descent() + sf.line_gap()).ceil();
    let shadow = (px / 16.0).ceil().max(1.0);
    let pad = shadow.ceil() as u32 + 2;

    let widths: Vec<f32> = lines.iter().map(|l| line_width(&sf, l)).collect();
    let max_w = widths.iter().cloned().fold(0.0, f32::max);
    let width = ((max_w.ceil() as u32) + pad * 2).min(frame_w);
    let height = ((line_h * lines.len() as f32).ceil() as u32 + pad * 2).min(frame_h);
    let mut bgra = vec![0u8; (width * height * 4) as usize];

    // Shadow first, then the text on top.
    for (pass, (color, offset)) in [([0u8, 0, 0], shadow), ([255u8, 255, 255], 0.0)].into_iter().enumerate() {
        let alpha = if pass == 0 { 0.7 } else { 1.0 };
        for (i, line) in lines.iter().enumerate() {
            let x0 = (width as f32 - widths[i]) / 2.0 + offset;
            let baseline = pad as f32 + sf.ascent() + line_h * i as f32 + offset;
            draw_line(&sf, line, x0, baseline, color, alpha, width, height, &mut bgra);
        }
    }

    let x = (frame_w as i32 - width as i32) / 2;
    let bottom = (frame_h as f32 * style.bottom_margin) as i32;
    let y = (frame_h as i32 - bottom - height as i32).max(0);
    Some(Layer { x, y, width, height, bgra })
}

fn line_width(sf: &Scaled, line: &str) -> f32 {
    let mut w = 0.0;
    let mut prev = None;
    for ch in line.chars() {
        let id = sf.glyph_id(ch);
        if let Some(p) = prev {
            w += sf.kern(p, id);
        }
        w += sf.h_advance(id);
        prev = Some(id);
    }
    w
}

#[allow(clippy::too_many_arguments)]
fn draw_line(
    sf: &Scaled,
    line: &str,
    x0: f32,
    baseline: f32,
    color: [u8; 3],
    alpha: f32,
    width: u32,
    height: u32,
    out: &mut [u8],
) {
    let mut caret = x0;
    let mut prev = None;
    for ch in line.chars() {
        let id = sf.glyph_id(ch);
        if let Some(p) = prev {
            caret += sf.kern(p, id);
        }
        let glyph = id.with_scale_and_position(sf.scale(), point(caret, baseline));
        caret += sf.h_advance(id);
        prev = Some(id);
        let Some(outlined) = sf.font().outline_glyph(glyph) else { continue };
        let b = outlined.px_bounds();
        outlined.draw(|gx, gy, cov| {
            let px = b.min.x as i32 + gx as i32;
            let py = b.min.y as i32 + gy as i32;
            if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                return;
            }
            let a = (cov.clamp(0.0, 1.0) * alpha).min(1.0);
            let i = ((py as u32 * width + px as u32) * 4) as usize;
            // Premultiplied "source over": out = src + dst * (1 - a). BGRA order.
            let keep = 1.0 - a;
            out[i] = (color[2] as f32 * a + out[i] as f32 * keep) as u8;
            out[i + 1] = (color[1] as f32 * a + out[i + 1] as f32 * keep) as u8;
            out[i + 2] = (color[0] as f32 * a + out[i + 2] as f32 * keep) as u8;
            out[i + 3] = (255.0 * a + out[i + 3] as f32 * keep) as u8;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> FontRef<'static> {
        let bytes: &'static [u8] = Box::leak(crate::load_font(None).expect("a system font").into_boxed_slice());
        FontRef::try_from_slice(bytes).unwrap()
    }

    #[test]
    fn blank_slide_renders_nothing() {
        let f = font();
        assert!(render(&f, &[], 1920, 1080, &Style::default()).is_none());
        assert!(render(&f, &["  ", ""], 1920, 1080, &Style::default()).is_none());
    }

    #[test]
    fn layer_is_centered_in_the_lower_third_and_has_ink() {
        let f = font();
        let l = render(&f, &["Amazing grace, how sweet the sound", "That saved a wretch like me"], 1920, 1080, &Style::default()).unwrap();
        assert_eq!(l.bgra.len(), (l.width * l.height * 4) as usize);
        assert!(l.y as u32 > 1080 / 2, "text should sit low on screen");
        assert!((l.x + l.width as i32 / 2 - 960).abs() <= 1, "text should be centered");
        let opaque = l.bgra.chunks(4).filter(|p| p[3] > 200).count();
        assert!(opaque > 1000, "expected visible glyph pixels, got {opaque}");
        // Premultiplied alpha: no channel may exceed alpha.
        assert!(l.bgra.chunks(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
    }

    #[test]
    fn long_lines_never_exceed_the_frame() {
        let f = font();
        let long = "word ".repeat(200);
        let l = render(&f, &[&long], 1280, 720, &Style::default()).unwrap();
        assert!(l.width <= 1280 && l.x >= 0);
    }
}
