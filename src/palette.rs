//! The colors of a picture, for a popup drawn in them, as Android's media player takes them from
//! the cover: the color the picture has most of becomes the background, and its liveliest color
//! with a fair share of it the accent. Both are set at the lightness the theme wants, dark or
//! light, keeping their hue and some of their chroma, so that text reads on them whatever the
//! picture is. Worked out in Oklab, where equal distances look about equally different.

use std::f64::consts::{PI, TAU};

use crate::gfx::Rgba;
use crate::theme::Theme;

/// A color in Oklab's polar form (Oklch): lightness 0..1, chroma (0 for a gray, about 0.32 for the
/// most vivid blue of sRGB) and hue in radians.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lch {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

/// The chroma from which a color counts as one: below it, a gray with a tinge.
const COLORFUL: f64 = 0.04;
/// How many colors the pixels are grouped into, at most, before the close ones are merged.
const CLUSTERS: usize = 16;
/// Colors closer than this are one: a flat color that the grouping split.
const SAME: f64 = 0.05;
/// The least share of the picture an accent has: not a speck.
const SHARE: f64 = 0.02;
/// How far from the background's hue a color has a hue of its own (30°), and the share it needs
/// then to be the accent rather than a lighter tone of the background.
const OWN_HUE: f64 = 0.52;
const OWN_SHARE: f64 = 0.05;

/// The two colors of a picture that a theme takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// The color it has most of: the background's.
    pub dominant: Lch,
    /// The accent's: the liveliest color with a fair share, of a hue of its own if the picture has
    /// one; the dominant one itself in a picture without colors.
    pub accent: Lch,
}

impl Palette {
    /// The palette of a picture's pixels, RGBA with premultiplied alpha as CoreGraphics draws
    /// them; None if none of them shows.
    pub fn of_pixels(rgba: &[u8]) -> Option<Palette> {
        let pixels: Vec<[f64; 3]> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] >= 128)
            .map(|p| {
                let a = p[3] as f64 / 255.0;
                let ch = |v: u8| linear((v as f64 / 255.0 / a).min(1.0));
                oklab([ch(p[0]), ch(p[1]), ch(p[2])])
            })
            .collect();
        let colors: Vec<(Lch, f64)> = clusters(pixels).into_iter().map(|(lab, share)| (lch(lab), share)).collect();
        let dominant = colors.iter().max_by(|a, b| a.1.total_cmp(&b.1))?.0;
        // Material's score, which picks the color of Android's themes from a wallpaper: the share
        // of the picture in about that hue, and the chroma, which counts more over a fair amount;
        // here also how far its hue is from the background's, as the farther stands out more
        let lively: Vec<(Lch, f64)> = colors.iter().copied().filter(|(c, _)| c.c >= COLORFUL).collect();
        let scored: Vec<(f64, f64, Lch)> = lively
            .iter()
            .map(|(c, _)| {
                let share: f64 = lively.iter().filter(|(o, _)| hue_gap(o.h, c.h) <= 20f64.to_radians()).map(|(_, s)| s).sum();
                // About the chroma of Material's color space (HCT), whose weights these are
                let chroma = c.c * 360.0;
                let mut score = share * 70.0 + (chroma - 48.0) * if chroma >= 48.0 { 0.3 } else { 0.1 };
                if dominant.c >= COLORFUL {
                    score += 20.0 * hue_gap(c.h, dominant.h) / PI;
                }
                (score, share, *c)
            })
            .filter(|(_, share, _)| *share >= SHARE)
            .collect();
        // On a colorful background, a hue of its own with a fair share stands out: the best of
        // those, else the best of all, the background's own hue too
        let own = |c: &Lch| dominant.c < COLORFUL || hue_gap(c.h, dominant.h) > OWN_HUE;
        let best = |own_only: bool| {
            scored.iter().filter(|(_, share, c)| !own_only || (own(c) && *share >= OWN_SHARE)).max_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, _, c)| *c)
        };
        let accent = best(true).or_else(|| best(false)).unwrap_or(dominant);
        Some(Palette { dominant, accent })
    }

    /// `theme` in these colors: the background, the surfaces and the text in the dominant color's
    /// hue, the accent in the accent's, dark or light as `theme` is. The rest (fonts, sizes, the
    /// colors of warnings) stays.
    pub fn theme(&self, theme: &Theme) -> Theme {
        let (d, a) = (self.dominant, self.accent);
        let tone = |of: Lch, l: f64, c: f64| rgb(Lch { l, c, h: of.h });
        // As much of the dominant color's chroma as a background takes before it shouts
        let tinge = |share: f64, max: f64| (d.c * share).min(max);
        // A color stays vivid, a gray stays one
        let accent = |max: f64| if a.c >= COLORFUL { a.c.clamp(0.09, max) } else { a.c };
        let mut t = theme.clone();
        if theme.dark() {
            t.base = tone(d, 0.25, tinge(0.7, 0.075));
            t.surface = tone(d, 0.33, tinge(0.6, 0.06));
            t.highlight = tone(d, 0.38, tinge(0.6, 0.06));
            t.overlay = tone(d, 0.56, tinge(0.4, 0.04));
            t.subtext = tone(d, 0.80, tinge(0.3, 0.03));
            t.text = tone(d, 0.96, tinge(0.15, 0.015));
            t.primary = tone(a, 0.82, accent(0.16));
            t.on_primary = tone(a, 0.25, (a.c * 0.5).min(0.06));
        } else {
            t.base = tone(d, 0.95, tinge(0.3, 0.03));
            t.surface = tone(d, 0.89, tinge(0.35, 0.04));
            t.highlight = tone(d, 0.86, tinge(0.35, 0.04));
            t.overlay = tone(d, 0.62, tinge(0.4, 0.04));
            t.subtext = tone(d, 0.45, tinge(0.3, 0.03));
            t.text = tone(d, 0.22, tinge(0.3, 0.03));
            t.primary = tone(a, 0.52, accent(0.18));
            t.on_primary = tone(a, 0.99, (a.c * 0.1).min(0.01));
        }
        for (name, color) in [
            ("base", t.base),
            ("surface", t.surface),
            ("highlight", t.highlight),
            ("overlay", t.overlay),
            ("subtext", t.subtext),
            ("text", t.text),
            ("primary", t.primary),
            ("on_primary", t.on_primary),
        ] {
            t.palette.insert(name.into(), color);
        }
        t
    }
}

/// The picture's colors: its pixels (Oklab) grouped into at most `CLUSTERS`, each one's mean and
/// its share of the picture, the close ones merged.
fn clusters(mut pixels: Vec<[f64; 3]>) -> Vec<([f64; 3], f64)> {
    if pixels.is_empty() {
        return Vec::new();
    }
    let total = pixels.len() as f64;
    // Median cut first: the box with the most pixels times its widest spread is cut in two at its
    // median along that spread, until there are enough
    let mut boxes = vec![(0, pixels.len())];
    while boxes.len() < CLUSTERS {
        let cut = boxes
            .iter()
            .enumerate()
            .filter(|(_, (s, e))| e - s >= 2)
            .map(|(i, &(s, e))| {
                let (axis, spread) = widest(&pixels[s..e]);
                (i, axis, spread * (e - s) as f64)
            })
            .filter(|(_, _, score)| *score > 0.0)
            .max_by(|a, b| a.2.total_cmp(&b.2));
        let Some((i, axis, _)) = cut else { break };
        let (s, e) = boxes[i];
        pixels[s..e].sort_unstable_by(|a, b| a[axis].total_cmp(&b[axis]));
        let m = (s + e) / 2;
        boxes[i] = (s, m);
        boxes.push((m, e));
    }
    let mut centers: Vec<[f64; 3]> = boxes.iter().map(|&(s, e)| mean(&pixels[s..e])).collect();
    // Then k-means from there: each pixel to its nearest color, each color to the mean of its
    // pixels. The cut halves the pixels, so this is what gives the shares their meaning
    let mut counts = vec![0usize; centers.len()];
    for _ in 0..8 {
        let mut sums = vec![[0.0; 3]; centers.len()];
        counts.iter_mut().for_each(|n| *n = 0);
        for p in &pixels {
            let i = (0..centers.len()).min_by(|&a, &b| distance(&centers[a], p).total_cmp(&distance(&centers[b], p))).unwrap_or(0);
            counts[i] += 1;
            (0..3).for_each(|k| sums[i][k] += p[k]);
        }
        for (i, sum) in sums.iter().enumerate() {
            if counts[i] > 0 {
                centers[i] = sum.map(|v| v / counts[i] as f64);
            }
        }
    }
    let mut order: Vec<usize> = (0..centers.len()).filter(|&i| counts[i] > 0).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(counts[i]));
    let mut merged: Vec<([f64; 3], f64)> = Vec::new();
    for i in order {
        let (c, n) = (centers[i], counts[i] as f64);
        match merged.iter_mut().find(|(m, _)| distance(m, &c).sqrt() < SAME) {
            Some((m, total)) => {
                (0..3).for_each(|k| m[k] = (m[k] * *total + c[k] * n) / (*total + n));
                *total += n;
            }
            None => merged.push((c, n)),
        }
    }
    merged.into_iter().map(|(c, n)| (c, n / total)).collect()
}

/// The axis the pixels spread most along, and how much.
fn widest(pixels: &[[f64; 3]]) -> (usize, f64) {
    (0..3)
        .map(|k| {
            let (lo, hi) = pixels.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p[k]), hi.max(p[k])));
            (k, hi - lo)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap_or((0, 0.0))
}

fn mean(pixels: &[[f64; 3]]) -> [f64; 3] {
    let n = pixels.len().max(1) as f64;
    pixels.iter().fold([0.0; 3], |s, p| [s[0] + p[0], s[1] + p[1], s[2] + p[2]]).map(|v| v / n)
}

/// Squared.
fn distance(a: &[f64; 3], b: &[f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// How far apart two hues are, 0..π.
fn hue_gap(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(TAU);
    d.min(TAU - d)
}

/// An sRGB channel, 0..1, as light.
fn linear(v: f64) -> f64 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// Light, 0..1, as an sRGB channel.
fn gamma(v: f64) -> f64 {
    if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// Linear sRGB to Oklab, as Björn Ottosson defines it.
fn oklab([r, g, b]: [f64; 3]) -> [f64; 3] {
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

/// Oklab to linear sRGB, out of 0..1 where sRGB can't show it.
fn linear_rgb([l, a, b]: [f64; 3]) -> [f64; 3] {
    let l_ = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m_ = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s_ = (l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    [
        4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
        -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
        -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_,
    ]
}

fn lch([l, a, b]: [f64; 3]) -> Lch {
    Lch { l, c: a.hypot(b), h: b.atan2(a) }
}

/// The color in sRGB: with less chroma, the same lightness and hue, where sRGB can't show it all.
pub fn rgb(c: Lch) -> Rgba {
    let at = |chroma: f64| linear_rgb([c.l, chroma * c.h.cos(), chroma * c.h.sin()]);
    let fits = |v: [f64; 3]| v.iter().all(|x| (-1e-4..=1.0 + 1e-4).contains(x));
    let mut v = at(c.c);
    if !fits(v) {
        let (mut lo, mut hi) = (0.0, c.c);
        for _ in 0..20 {
            let mid = (lo + hi) / 2.0;
            if fits(at(mid)) { lo = mid } else { hi = mid }
        }
        v = at(lo);
    }
    let ch = |x: f64| gamma(x.clamp(0.0, 1.0));
    Rgba(ch(v[0]), ch(v[1]), ch(v[2]), 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeConfig;

    /// Pixels of `colors` (sRGB, opaque), each `n` times.
    fn picture(colors: &[([u8; 3], usize)]) -> Vec<u8> {
        colors.iter().flat_map(|&(c, n)| std::iter::repeat_n([c[0], c[1], c[2], 255], n).flatten()).collect()
    }

    fn close(a: Rgba, b: Rgba) -> bool {
        (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01 && (a.2 - b.2).abs() < 0.01
    }

    #[test]
    fn oklab_goes_there_and_back() {
        for c in [Rgba(1.0, 1.0, 1.0, 1.0), Rgba(0.0, 0.0, 0.0, 1.0), Rgba(0.2, 0.5, 0.9, 1.0), Rgba(1.0, 0.8, 0.3, 1.0)] {
            let lab = oklab([linear(c.0), linear(c.1), linear(c.2)]);
            assert!(close(rgb(lch(lab)), c), "{c:?}");
        }
        // White is lightness 1 and no chroma
        let white = lch(oklab([1.0, 1.0, 1.0]));
        assert!((white.l - 1.0).abs() < 1e-3 && white.c < 1e-3);
        // Too much chroma for sRGB: less of it, the same hue
        let red = rgb(Lch { l: 0.6, c: 0.5, h: 0.5 });
        assert!(red.0 > 0.9 && red.1 < 0.4 && red.2 < 0.4, "{red:?}");
    }

    /// The color it has most of, and the liveliest with a fair share for the accent: not a speck,
    /// however vivid.
    #[test]
    fn the_dominant_color_and_the_accent() {
        let navy = [0x1b, 0x1f, 0x4a];
        let gold = [0xff, 0xc5, 0x5e];
        let p = Palette::of_pixels(&picture(&[(navy, 600), (gold, 300), ([0xff, 0x00, 0x00], 5)])).unwrap();
        assert!(hue_gap(p.dominant.h, lch(oklab(navy.map(|v| linear(v as f64 / 255.0)))).h) < 0.05, "{p:?}");
        assert!(hue_gap(p.accent.h, lch(oklab(gold.map(|v| linear(v as f64 / 255.0)))).h) < 0.05, "{p:?}");
        // Without colors, the accent is the dominant gray
        let p = Palette::of_pixels(&picture(&[([20, 20, 20], 700), ([240, 240, 240], 300)])).unwrap();
        assert!(p.dominant.l < 0.3 && p.accent == p.dominant);
        // Transparent pixels don't count; none at all, no palette
        assert!(Palette::of_pixels(&[0, 0, 0, 0, 10, 10, 10, 20]).is_none());
        assert!(Palette::of_pixels(&[]).is_none());
    }

    /// Whatever the picture, the text reads on the background and on the accent's buttons, in a
    /// dark theme and in a light one.
    #[test]
    fn its_themes_read() {
        let contrast = |a: Rgba, b: Rgba| {
            let lum = |c: Rgba| {
                let ch = |v: f64| linear(v);
                0.2126 * ch(c.0) + 0.7152 * ch(c.1) + 0.0722 * ch(c.2)
            };
            let (x, y) = (lum(a), lum(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        };
        let pictures = [
            picture(&[([0x1b, 0x1f, 0x4a], 600), ([0xff, 0xc5, 0x5e], 300)]),
            picture(&[([0xff, 0xff, 0x00], 900), ([0x00, 0x00, 0xff], 100)]),
            picture(&[([0xff, 0xff, 0xff], 1000)]),
            picture(&[([0x00, 0xff, 0x00], 500), ([0xff, 0x00, 0xff], 500)]),
        ];
        for preset in ["catppuccin-mocha", "catppuccin-latte"] {
            let theme = Theme::from_config(&ThemeConfig { preset: preset.into(), ..Default::default() });
            for px in &pictures {
                let t = Palette::of_pixels(px).unwrap().theme(&theme);
                assert_eq!(t.dark(), theme.dark());
                assert!(contrast(t.text, t.base) >= 7.0, "{preset} text {:?} on {:?}", t.text, t.base);
                assert!(contrast(t.subtext, t.base) >= 4.5, "{preset} subtext");
                assert!(contrast(t.on_primary, t.primary) >= 4.5, "{preset} on primary {:?} {:?}", t.on_primary, t.primary);
                assert!(contrast(t.primary, t.base) >= 3.0, "{preset} primary");
                assert_eq!(t.color("primary"), Some(t.primary));
            }
        }
    }
}
