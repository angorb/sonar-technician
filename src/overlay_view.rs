//! Platform-independent half of the overlay (FR-7): what to show for a
//! [`Snapshot`], the layout at a given scale, where the window goes, and the
//! pixel painting (rounded panel, volume bar, alpha). `platform::overlay`
//! adds the window and draws the text with GDI on top of [`Canvas`].

use crate::config::Corner;
use crate::controller::Snapshot;
use crate::sonar::Mode;

// Colours (RGB).
pub const BG: (u8, u8, u8) = (0x1C, 0x1C, 0x21);
pub const TEXT: (u8, u8, u8) = (0xF4, 0xF4, 0xF5);
pub const DIM: (u8, u8, u8) = (0xA1, 0xA1, 0xAA);
pub const ACCENT: (u8, u8, u8) = (0xFF, 0x6A, 0x13);
pub const MUTED: (u8, u8, u8) = (0xEF, 0x44, 0x44);
const TRACK: (u8, u8, u8) = (0x3F, 0x3F, 0x46);
const BAR_MUTED: (u8, u8, u8) = (0x71, 0x71, 0x7A);

/// What the overlay says.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    /// Small line at the top: mode (and mix in Streamer mode).
    pub header: String,
    /// Large line: channel name (or the status).
    pub title: String,
    /// Right side of the title line: "42%".
    pub value: Option<String>,
    /// Bar fill 0..=1; `None` = no bar.
    pub level: Option<f64>,
    pub muted: bool,
    /// Small bottom line: device switch / status / error text.
    pub footer: Option<String>,
}

pub fn view(s: &Snapshot) -> View {
    let Some(mode) = s.mode.filter(|_| s.connected) else {
        return View {
            header: "Sonar Technician".into(),
            title: "Sonar unavailable".into(),
            value: None,
            level: None,
            muted: false,
            footer: s.message.clone().filter(|m| m != "Sonar unavailable"),
        };
    };
    let header = match mode {
        Mode::Classic => "Classic".to_owned(),
        Mode::Stream => format!("Streamer · {}", s.mix.label()),
    };
    View {
        header,
        title: s.channel.label().to_owned(),
        value: s.volume.map(|v| format!("{:.0}%", v * 100.0)),
        level: s.volume,
        muted: s.muted == Some(true),
        footer: s.message.clone(),
    }
}

/// Pixel layout at a given scale (1.0 = 96 DPI, config scale 1.0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub width: i32,
    pub height: i32,
    pub radius: f32,
    pub pad: i32,
    /// (top, height) of each text line / the bar.
    pub header: (i32, i32),
    pub title: (i32, i32),
    pub bar: Option<(i32, i32)>,
    pub footer: Option<(i32, i32)>,
    pub small_font: i32,
    pub title_font: i32,
}

pub fn layout(scale: f32, has_bar: bool, has_footer: bool) -> Layout {
    let px = |v: f32| (v * scale).round() as i32;
    let pad = px(16.0);
    let mut y = px(12.0);
    let header = (y, px(18.0));
    y += header.1 + px(2.0);
    let title = (y, px(30.0));
    y += title.1;
    let bar = has_bar.then(|| {
        y += px(8.0);
        let b = (y, px(6.0).max(2));
        y += b.1;
        b
    });
    let footer = has_footer.then(|| {
        y += px(8.0);
        let f = (y, px(18.0));
        y += f.1;
        f
    });
    y += px(14.0);
    Layout {
        width: px(300.0),
        height: y,
        radius: 12.0 * scale,
        pad,
        header,
        title,
        bar,
        footer,
        small_font: px(13.0),
        title_font: px(22.0),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Top-left position for a `size` window in the `work` area.
pub fn place(work: Rect, size: (i32, i32), corner: Corner, offset: (i32, i32)) -> (i32, i32) {
    let (w, h) = size;
    let (ox, oy) = offset;
    let cx = work.left + (work.right - work.left - w) / 2;
    let left = work.left + ox;
    let right = work.right - w - ox;
    let top = work.top + oy;
    let bottom = work.bottom - h - oy;
    match corner {
        Corner::TopLeft => (left, top),
        Corner::TopCenter => (cx, top),
        Corner::TopRight => (right, top),
        Corner::BottomLeft => (left, bottom),
        Corner::BottomCenter => (cx, bottom),
        Corner::BottomRight => (right, bottom),
    }
}

/// Anti-aliased coverage of pixel (px, py) by a rounded rectangle.
fn rr_coverage(px: i32, py: i32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
    let r = r.min(w / 2.0).min(h / 2.0);
    let dx = (x + r - cx).max(cx - (x + w - r)).max(0.0);
    let dy = (y + r - cy).max(cy - (y + h - r)).max(0.0);
    let d = (dx * dx + dy * dy).sqrt() - r;
    (0.5 - d).clamp(0.0, 1.0)
}

fn pack(rgb: (u8, u8, u8)) -> u32 {
    (u32::from(rgb.0) << 16) | (u32::from(rgb.1) << 8) | u32::from(rgb.2)
}

fn unpack(p: u32) -> (f32, f32, f32) {
    (
        ((p >> 16) & 0xFF) as f32,
        ((p >> 8) & 0xFF) as f32,
        (p & 0xFF) as f32,
    )
}

/// BGRA pixels (top-down, `0xAARRGGBB` as u32) plus the panel's coverage mask.
/// Paint opaque colours first, let GDI draw text, then [`Canvas::finish`]
/// applies the mask as premultiplied alpha.
pub struct Canvas {
    pub width: i32,
    pub height: i32,
    pub px: Vec<u32>,
    mask: Vec<u8>,
}

impl Canvas {
    /// The rounded background panel.
    pub fn panel(l: &Layout) -> Self {
        let (w, h) = (l.width, l.height);
        let mut mask = vec![0u8; (w * h) as usize];
        let mut px = vec![0u32; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let c = rr_coverage(x, y, 0.0, 0.0, w as f32, h as f32, l.radius);
                let i = (y * w + x) as usize;
                mask[i] = (c * 255.0).round() as u8;
                if c > 0.0 {
                    px[i] = pack(BG);
                }
            }
        }
        Self {
            width: w,
            height: h,
            px,
            mask,
        }
    }

    /// Blend an anti-aliased rounded rect of `rgb` onto the (opaque) pixels.
    fn rounded(&mut self, x: f32, y: f32, w: f32, h: f32, rgb: (u8, u8, u8)) {
        if w <= 0.0 {
            return;
        }
        let r = h / 2.0;
        let src = (f32::from(rgb.0), f32::from(rgb.1), f32::from(rgb.2));
        let (x0, x1) = (x.floor() as i32, (x + w).ceil() as i32);
        let (y0, y1) = (y.floor() as i32, (y + h).ceil() as i32);
        for py in y0.max(0)..y1.min(self.height) {
            for px in x0.max(0)..x1.min(self.width) {
                let c = rr_coverage(px, py, x, y, w, h, r);
                if c <= 0.0 {
                    continue;
                }
                let i = (py * self.width + px) as usize;
                let d = unpack(self.px[i]);
                let mix = |s: f32, d: f32| (s * c + d * (1.0 - c)).round() as u32;
                self.px[i] = (mix(src.0, d.0) << 16) | (mix(src.1, d.1) << 8) | mix(src.2, d.2);
            }
        }
    }

    /// Volume bar: track plus fill (grey when muted).
    pub fn bar(&mut self, l: &Layout, level: f64, muted: bool) {
        let Some((top, height)) = l.bar else { return };
        let x = l.pad as f32;
        let w = (l.width - 2 * l.pad) as f32;
        self.rounded(x, top as f32, w, height as f32, TRACK);
        let fill =
            (w * level.clamp(0.0, 1.0) as f32).max(if level > 0.0 { height as f32 } else { 0.0 });
        self.rounded(
            x,
            top as f32,
            fill,
            height as f32,
            if muted { BAR_MUTED } else { ACCENT },
        );
    }

    /// Apply the panel mask as premultiplied alpha (after GDI text, which
    /// leaves the alpha byte at 0).
    pub fn finish(&mut self) {
        for (p, &a) in self.px.iter_mut().zip(&self.mask) {
            let (r, g, b) = unpack(*p);
            let a32 = u32::from(a);
            let pm = |v: f32| (v as u32 * a32 / 255) & 0xFF;
            *p = (a32 << 24) | (pm(r) << 16) | (pm(g) << 8) | pm(b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sonar::{Channel, Mix};

    fn snap() -> Snapshot {
        Snapshot {
            connected: true,
            mode: Some(Mode::Classic),
            channel: Channel::Game,
            mix: Mix::Monitoring,
            volume: Some(0.42),
            muted: Some(false),
            device: Some("Speakers".into()),
            message: None,
        }
    }

    #[test]
    fn view_classic() {
        let v = view(&snap());
        assert_eq!(v.header, "Classic");
        assert_eq!(v.title, "Game");
        assert_eq!(v.value.as_deref(), Some("42%"));
        assert_eq!(v.level, Some(0.42));
        assert!(!v.muted);
        assert_eq!(
            v.footer, None,
            "device only shown on a switch (via message)"
        );
    }

    #[test]
    fn view_streamer_muted_with_message() {
        let mut s = snap();
        s.mode = Some(Mode::Stream);
        s.mix = Mix::Streaming;
        s.muted = Some(true);
        s.message = Some("→ Headphones".into());
        let v = view(&s);
        assert_eq!(v.header, "Streamer · Streaming");
        assert!(v.muted);
        assert_eq!(v.footer.as_deref(), Some("→ Headphones"));
    }

    #[test]
    fn view_unavailable() {
        let mut s = snap();
        s.connected = false;
        s.message = Some("Sonar unavailable".into());
        let v = view(&s);
        assert_eq!(v.title, "Sonar unavailable");
        assert_eq!(v.level, None);
        assert_eq!(v.footer, None, "no duplicate text");
    }

    #[test]
    fn layout_grows_with_content_and_scale() {
        let a = layout(1.0, false, false);
        let b = layout(1.0, true, false);
        let c = layout(1.0, true, true);
        assert!(a.height < b.height && b.height < c.height);
        let big = layout(1.5, true, true);
        assert_eq!(big.width, 450);
        assert!((big.height as f32 / c.height as f32 - 1.5).abs() < 0.05);
        let bar = c.bar.unwrap();
        let footer = c.footer.unwrap();
        assert!(c.title.0 + c.title.1 <= bar.0);
        assert!(bar.0 + bar.1 <= footer.0);
        assert!(footer.0 + footer.1 < c.height);
    }

    #[test]
    fn placement() {
        let work = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        assert_eq!(
            place(work, (300, 100), Corner::BottomRight, (24, 24)),
            (1596, 916)
        );
        assert_eq!(place(work, (300, 100), Corner::TopLeft, (24, 24)), (24, 24));
        assert_eq!(
            place(work, (300, 100), Corner::TopCenter, (0, 10)),
            (810, 10)
        );
        // Secondary monitor to the left, with a taskbar at the bottom.
        let left = Rect {
            left: -2560,
            top: 0,
            right: 0,
            bottom: 1400,
        };
        assert_eq!(
            place(left, (300, 100), Corner::BottomLeft, (10, 10)),
            (-2550, 1290)
        );
    }

    #[test]
    fn canvas_corners_transparent_centre_opaque() {
        let l = layout(1.0, true, false);
        let mut c = Canvas::panel(&l);
        c.bar(&l, 0.5, false);
        c.finish();
        let at = |x: i32, y: i32| c.px[(y * c.width + x) as usize];
        assert_eq!(at(0, 0) >> 24, 0, "corner is transparent");
        assert_eq!(at(l.width / 2, 2) >> 24, 255, "top edge middle is opaque");
        let (bar_top, bar_h) = l.bar.unwrap();
        let mid_y = bar_top + bar_h / 2;
        let filled = at(l.pad + 10, mid_y);
        let empty = at(l.width - l.pad - 10, mid_y);
        assert_eq!(filled & 0xFF_FFFF, pack(ACCENT));
        assert_eq!(empty & 0xFF_FFFF, pack(TRACK));
    }

    #[test]
    fn finish_premultiplies() {
        let l = layout(1.0, false, false);
        let mut c = Canvas::panel(&l);
        c.finish();
        for p in &c.px {
            let a = p >> 24;
            for shift in [0, 8, 16] {
                assert!((p >> shift) & 0xFF <= a, "channel exceeds alpha");
            }
        }
    }
}
