//! The verification captcha image: answer characters in the font colour at mixed sizes, heights and
//! tilts, joined left to right by one traced line, with grey decoy characters behind each one and
//! spread around. The background is transparent so the image sits on the embed's own colour.
//! Shared by the bot (the real challenge) and the dashboard (the live preview).
use crate::join::CaptchaCfg;
use ab_glyph::{Font, FontRef, PxScale};
use image::{Rgba, RgbaImage};
use std::sync::LazyLock;

/// DejaVu Sans, printable ASCII only, renamed as its license asks (assets/LICENSE-captcha-font.md).
static FONT: LazyLock<FontRef<'static>> = LazyLock::new(|| {
    FontRef::try_from_slice(include_bytes!("../assets/captcha-font.ttf")).expect("captcha font")
});

/// Answer characters: 1-9, A-Z without I L O, a-z without i l o, and `! $ #.: ;` (the charset
/// setting narrows it). Look-alikes (0/O, 1/I/l) are left out because the answer ignores case.
const ANSWER: &str = "123456789ABCDEFGHJKMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz!$#.:;";
/// Decoys may be any letter or digit.
const DECOYS: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn rgb(hex: &str, fallback: [u8; 3]) -> Rgba<u8> {
    let n = hex
        .strip_prefix('#')
        .filter(|h| h.len() == 6)
        .and_then(|h| u32::from_str_radix(h, 16).ok());
    let [r, g, b] = n.map_or(fallback, |n| [(n >> 16) as u8, (n >> 8) as u8, n as u8]);
    Rgba([r, g, b, 255])
}

fn pick(pool: &[char]) -> char {
    pool[rand::random_range(0..pool.len())]
}

fn jitter(a: f32, b: f32) -> f32 {
    rand::random_range(a..b)
}

/// Lays `color` over the pixel with `cover` (0..1) opacity: the usual "over" blend.
fn blend(img: &mut RgbaImage, x: i64, y: i64, color: Rgba<u8>, cover: f32) {
    if cover <= 0.0 || x < 0 || y < 0 || x >= img.width() as i64 || y >= img.height() as i64 {
        return;
    }
    let p = img.get_pixel_mut(x as u32, y as u32);
    let (sa, da) = (cover.min(1.0), p.0[3] as f32 / 255.0);
    let out = sa + da * (1.0 - sa);
    for i in 0..3 {
        let v = (color.0[i] as f32 * sa + p.0[i] as f32 * da * (1.0 - sa)) / out;
        p.0[i] = v.round() as u8;
    }
    p.0[3] = (out * 255.0).round() as u8;
}

/// One character `px` tall around (cx, cy), turned by `angle` radians. The baseline sits a little
/// below `cy`, so `.` and `,`-like marks stay low as in normal text instead of floating mid-height.
fn glyph(img: &mut RgbaImage, ch: char, cx: f32, cy: f32, px: f32, angle: f32, color: Rgba<u8>) {
    let font = &*FONT;
    let g = font
        .glyph_id(ch)
        .with_scale_and_position(PxScale::from(px), ab_glyph::point(0.0, 0.0));
    let Some(o) = font.outline_glyph(g) else {
        return;
    };
    let b = o.px_bounds();
    let cy = cy + px * 0.36 + (b.min.y + b.max.y) / 2.0;
    let (gw, gh) = (
        b.width().ceil() as usize + 1,
        b.height().ceil() as usize + 1,
    );
    let mut cov = vec![0f32; gw * gh];
    o.draw(|x, y, c| {
        if let Some(v) = cov.get_mut(y as usize * gw + x as usize) {
            *v = c;
        }
    });
    let (hw, hh) = (gw as f32 / 2.0, gh as f32 / 2.0);
    let (sin, cos) = angle.sin_cos();
    // Whole glyphs only: the turned box stays 1 px inside the image, so nothing is cut at an
    // edge (a cut decoy at a corner was a drawing fault; it also made the clear-corner test flaky).
    let (ex, ey) = (
        cos.abs() * hw + sin.abs() * hh + 1.0,
        sin.abs() * hw + cos.abs() * hh + 1.0,
    );
    let inside = |v: f32, half: f32, len: f32| {
        if 2.0 * half >= len {
            len / 2.0
        } else {
            v.clamp(half, len - half)
        }
    };
    let cx = inside(cx, ex, img.width() as f32);
    let cy = inside(cy, ey, img.height() as f32);
    // Sample backwards from each target pixel, so a turned glyph has no holes.
    let r = (hw * hw + hh * hh).sqrt().ceil() as i64 + 1;
    for ty in (cy as i64 - r)..=(cy as i64 + r) {
        for tx in (cx as i64 - r)..=(cx as i64 + r) {
            let (dx, dy) = (tx as f32 + 0.5 - cx, ty as f32 + 0.5 - cy);
            let (lx, ly) = (
                dx * cos + dy * sin + hw - 0.5,
                -dx * sin + dy * cos + hh - 0.5,
            );
            let (x0, y0) = (lx.floor(), ly.floor());
            let (fx, fy) = (lx - x0, ly - y0);
            let at = |x: f32, y: f32| {
                if x < 0.0 || y < 0.0 || x >= gw as f32 || y >= gh as f32 {
                    0.0
                } else {
                    cov[y as usize * gw + x as usize]
                }
            };
            let c = at(x0, y0) * (1.0 - fx) * (1.0 - fy)
                + at(x0 + 1.0, y0) * fx * (1.0 - fy)
                + at(x0, y0 + 1.0) * (1.0 - fx) * fy
                + at(x0 + 1.0, y0 + 1.0) * fx * fy;
            blend(img, tx, ty, color, c);
        }
    }
}

/// A round-ended line `width` px thick, with soft edges.
fn line(
    img: &mut RgbaImage,
    (x0, y0): (f32, f32),
    (x1, y1): (f32, f32),
    width: f32,
    color: Rgba<u8>,
) {
    let (vx, vy) = (x1 - x0, y1 - y0);
    let len2 = (vx * vx + vy * vy).max(1e-6);
    let pad = width;
    let (lo_x, hi_x) = (x0.min(x1) - pad, x0.max(x1) + pad);
    let (lo_y, hi_y) = (y0.min(y1) - pad, y0.max(y1) + pad);
    for y in lo_y as i64..=hi_y as i64 {
        for x in lo_x as i64..=hi_x as i64 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let t = (((px - x0) * vx + (py - y0) * vy) / len2).clamp(0.0, 1.0);
            let (ex, ey) = (px - (x0 + t * vx), py - (y0 + t * vy));
            let d = (ex * ex + ey * ey).sqrt();
            blend(img, x, y, color, width / 2.0 - d + 0.5);
        }
    }
}

/// The answer and its PNG.
pub fn new_captcha(c: &CaptchaCfg) -> (String, Vec<u8>) {
    let (w, h) = (c.width.clamp(150, 300), c.height.clamp(60, 200));
    let (wf, hf) = (w as f32, h as f32);
    let n = c.length.clamp(3, 7) as usize;
    // The font size, kept small enough that n characters fit across and one fits in the height.
    let size = (c.font_size.clamp(30, 60) as f32)
        .min(hf * 0.75)
        .min(wf / n as f32 * 1.25);
    let (font, decoy) = (
        rgb(&c.font_color, [0x32, 0xcf, 0x7e]),
        rgb(&c.decoy_color, [0x64, 0x65, 0x66]),
    );
    let pool: Vec<char> = ANSWER.chars().filter(|&x| c.charset.allows(x)).collect();
    let decoys: Vec<char> = DECOYS.chars().collect();
    let code: Vec<char> = (0..n).map(|_| pick(&pool)).collect();
    let mut img = RgbaImage::new(w, h);

    let margin = size * 0.45;
    let slot = (wf - 2.0 * margin) / n as f32;
    let spots: Vec<(f32, f32, f32, f32)> = (0..n)
        .map(|i| {
            let px = size * jitter(0.85, 1.2);
            let x = margin + slot * (i as f32 + 0.5) + slot * jitter(-0.15, 0.15);
            let half = px * 0.45;
            let y = (hf / 2.0 + hf * jitter(-0.25, 0.25)).clamp(half, (hf - half).max(half));
            (x, y, px, jitter(-0.35, 0.35))
        })
        .collect();

    if c.spread {
        for _ in 0..n + 2 {
            let px = size * jitter(0.5, 0.8);
            glyph(
                &mut img,
                pick(&decoys),
                jitter(0.0, wf),
                jitter(0.0, hf),
                px,
                jitter(-0.5, 0.5),
                decoy,
            );
        }
    }
    if c.mixed {
        // One decoy close behind each answer character, so cutting at gaps does not split them.
        for &(x, y, px, _) in &spots {
            let (dx, dy) = (px * jitter(-0.35, 0.35), px * jitter(-0.3, 0.3));
            glyph(
                &mut img,
                pick(&decoys),
                x + dx,
                y + dy,
                px * jitter(0.6, 0.85),
                jitter(-0.5, 0.5),
                decoy,
            );
        }
    }
    if c.trace {
        let width = (size * 0.1).max(2.5);
        // Each character is touched at a random height, not always through its middle.
        let at: Vec<(f32, f32)> = spots
            .iter()
            .map(|&(x, y, px, _)| {
                (
                    x,
                    (y + px * jitter(-0.25, 0.3)).clamp(width, (hf - width).max(width)),
                )
            })
            .collect();
        for p in at.windows(2) {
            line(&mut img, p[0], p[1], width, font);
        }
    }
    for (&ch, &(x, y, px, a)) in code.iter().zip(&spots) {
        glyph(&mut img, ch, x, y, px, a, font);
    }

    let mut png = std::io::Cursor::new(vec![]);
    let ok = img.write_to(&mut png, image::ImageFormat::Png).is_ok();
    (
        code.into_iter().collect(),
        if ok { png.into_inner() } else { vec![] },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(png: &[u8]) -> RgbaImage {
        image::load_from_memory(png).unwrap().into_rgba8()
    }

    #[test]
    fn every_answer_character_has_a_glyph() {
        for ch in ANSWER.chars().chain(DECOYS.chars()) {
            assert!(
                FONT.outline_glyph(FONT.glyph_id(ch).with_scale(40.0))
                    .is_some(),
                "{ch}"
            );
        }
    }

    #[test]
    fn follows_its_settings_within_bounds() {
        let (code, png) = new_captcha(&CaptchaCfg::default());
        assert_eq!(code.chars().count(), 6);
        assert!(code.chars().all(|c| c.is_ascii_uppercase()));
        assert_eq!(decode(&png).dimensions(), (300, 100));
        for (length, width, height) in [(1, 1, 1), (99, 9999, 9999), (7, 150, 60)] {
            let c = CaptchaCfg {
                length,
                width,
                height,
                font_size: width,
                decoy_color: "bad".into(),
                ..Default::default()
            };
            let (code, png) = new_captcha(&c);
            assert_eq!(code.chars().count(), length.clamp(3, 7) as usize);
            let (w, h) = decode(&png).dimensions();
            assert_eq!((w, h), (width.clamp(150, 300), height.clamp(60, 200)));
        }
    }

    #[test]
    fn draws_text_and_decoys_in_their_colours_on_a_clear_background() {
        let c = CaptchaCfg {
            font_color: "#ff0000".into(),
            decoy_color: "#0000ff".into(),
            ..Default::default()
        };
        let img = decode(&new_captcha(&c).1);
        let solid = |f: fn(&Rgba<u8>) -> bool| img.pixels().filter(|p| p.0[3] == 255).any(f);
        assert!(solid(|p| p.0[0] > 200 && p.0[2] < 50), "text");
        assert!(solid(|p| p.0[2] > 200 && p.0[0] < 50), "decoys");
        // Glyphs are kept whole inside the image, so every draw leaves the corners clear.
        for _ in 0..200 {
            let img = decode(&new_captcha(&c).1);
            let (w, h) = img.dimensions();
            for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
                assert_eq!(img.get_pixel(x, y).0[3], 0, "corner {x},{y} is clear");
            }
        }
        // Without decoys the image holds the font colour only.
        let c = CaptchaCfg {
            mixed: false,
            spread: false,
            font_color: "#ff0000".into(),
            ..Default::default()
        };
        let img = decode(&new_captcha(&c).1);
        assert!(img
            .pixels()
            .filter(|p| p.0[3] > 0)
            .all(|p| p.0[0] > 200 && p.0[1] < 30 && p.0[2] < 30));
    }

    #[test]
    fn answers_use_the_charset() {
        for (set, ok) in [
            (
                crate::join::Charset::Lower,
                (|c: char| c.is_ascii_lowercase()) as fn(char) -> bool,
            ),
            (crate::join::Charset::LowerDigitsSymbols, |c: char| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || "!$#.:;".contains(c)
            }),
        ] {
            for _ in 0..20 {
                let (code, _) = new_captcha(&CaptchaCfg {
                    charset: set,
                    length: 7,
                    ..Default::default()
                });
                assert!(code.chars().all(ok), "{code}");
                assert!(
                    !code.contains(['o', 'O', '0', 'I', 'L', 'i', 'l']),
                    "{code}"
                );
            }
        }
    }
}
