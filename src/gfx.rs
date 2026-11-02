//! Rasterization of text, SF Symbols and app icons into CGImages that CALayers show as contents.
//! Everything is drawn at the pixel scale of the screen it goes on and cached, so a layer that
//! shows the same text again (a digit of the calendar, the label of a pill) reuses the bitmap.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSFontWeight, NSFontWeightBold, NSFontWeightHeavy, NSFontWeightLight,
    NSFontWeightMedium, NSFontWeightRegular, NSFontWeightSemibold, NSGraphicsContext, NSImage,
    NSImageSymbolConfiguration, NSWorkspace,
};
use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType, CGPoint, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGBlendMode, CGColor, CGColorSpace, CGContext,
    CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
};
use objc2_core_text::{CTFont, CTLine, kCTFontAttributeName, kCTForegroundColorAttributeName};
use objc2_foundation::{NSRect, NSSize, NSString};

/// A color as straight (not premultiplied) sRGB components, 0..1.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Rgba(pub f64, pub f64, pub f64, pub f64);

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba(0.0, 0.0, 0.0, 0.0);

    /// `#rrggbb`, `#rrggbbaa`, `0xaarrggbb` (SketchyBar's order) or `rrggbb`.
    pub fn parse(s: &str) -> Option<Rgba> {
        let s = s.trim();
        let (hex, argb) = if let Some(h) = s.strip_prefix("0x") {
            (h, true)
        } else {
            (s.trim_start_matches('#'), false)
        };
        let v = u32::from_str_radix(hex, 16).ok()?;
        let c = |shift: u32| ((v >> shift) & 0xff) as f64 / 255.0;
        match (hex.len(), argb) {
            (6, _) => Some(Rgba(c(16), c(8), c(0), 1.0)),
            (8, true) => Some(Rgba(c(16), c(8), c(0), c(24))),
            (8, false) => Some(Rgba(c(24), c(16), c(8), c(0))),
            _ => None,
        }
    }

    /// At `a` of its opacity: the same as setting the alpha for an opaque color, and right for the
    /// translucent ones of the macOS palette.
    pub fn faded(self, a: f64) -> Rgba {
        Rgba(self.0, self.1, self.2, self.3 * a)
    }

    /// Linear blend towards `other` by `t` (0 = self).
    pub fn mix(self, other: Rgba, t: f64) -> Rgba {
        let l = |a: f64, b: f64| a + (b - a) * t;
        Rgba(l(self.0, other.0), l(self.1, other.1), l(self.2, other.2), l(self.3, other.3))
    }

    pub fn cg(self) -> CFRetained<CGColor> {
        CGColor::new_srgb(self.0, self.1, self.2, self.3)
    }

    fn key(self) -> u32 {
        let b = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u32;
        (b(self.0) << 24) | (b(self.1) << 16) | (b(self.2) << 8) | b(self.3)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub enum Weight {
    Light,
    #[default]
    Regular,
    Medium,
    Semibold,
    Bold,
    Heavy,
}

impl Weight {
    pub fn parse(s: &str) -> Weight {
        match s.to_ascii_lowercase().as_str() {
            "light" => Weight::Light,
            "medium" => Weight::Medium,
            "semibold" => Weight::Semibold,
            "bold" => Weight::Bold,
            "heavy" | "black" => Weight::Heavy,
            _ => Weight::Regular,
        }
    }

    fn ns(self) -> NSFontWeight {
        unsafe {
            match self {
                Weight::Light => NSFontWeightLight,
                Weight::Regular => NSFontWeightRegular,
                Weight::Medium => NSFontWeightMedium,
                Weight::Semibold => NSFontWeightSemibold,
                Weight::Bold => NSFontWeightBold,
                Weight::Heavy => NSFontWeightHeavy,
            }
        }
    }

    /// The PostScript suffix of the Helvetica Neue style closest to this weight.
    fn helvetica_suffix(self) -> &'static str {
        match self {
            Weight::Light => "-Light",
            Weight::Regular => "",
            Weight::Medium => "-Medium",
            Weight::Semibold | Weight::Bold | Weight::Heavy => "-Bold",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FontSpec {
    pub family: String,
    pub weight: Weight,
    /// Size in hundredths of a point, so the spec can be hashed.
    pub size_c: u32,
}

impl FontSpec {
    pub fn new(family: &str, weight: Weight, size: f64) -> FontSpec {
        FontSpec { family: family.to_string(), weight, size_c: (size * 100.0).round() as u32 }
    }

    pub fn size(&self) -> f64 {
        self.size_c as f64 / 100.0
    }

    /// The PostScript name CoreText is asked for. Families with known weights map to their styles;
    /// anything else is taken as a PostScript name, and CoreText falls back to the system font.
    fn postscript_name(&self) -> String {
        match self.family.as_str() {
            "Helvetica Neue" | "HelveticaNeue" => {
                format!("HelveticaNeue{}", self.weight.helvetica_suffix())
            }
            "SF Pro" | "System" | "system" => match self.weight {
                Weight::Light => ".AppleSystemUIFontLight".into(),
                Weight::Regular => ".AppleSystemUIFont".into(),
                Weight::Medium => ".AppleSystemUIFontMedium".into(),
                Weight::Semibold => ".AppleSystemUIFontDemi".into(),
                Weight::Bold | Weight::Heavy => ".AppleSystemUIFontBold".into(),
            },
            other => other.to_string(),
        }
    }
}

/// A rasterized piece of content and its size in points.
#[derive(Clone)]
pub struct Bitmap {
    pub image: CFRetained<CGImage>,
    pub width: f64,
    pub height: f64,
}

impl Bitmap {
    pub fn as_contents(&self) -> &AnyObject {
        let cf: &CFType = &self.image;
        cf.as_ref()
    }
}

/// Measured text: the width and the line metrics, in points.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextMetrics {
    pub width: f64,
    pub ascent: f64,
    pub descent: f64,
}

impl TextMetrics {
    pub fn height(&self) -> f64 {
        self.ascent + self.descent
    }
}

#[derive(Hash, PartialEq, Eq)]
enum Key {
    Text(String, FontSpec, u32, u32),
    Symbol(String, u32, Weight, u32, u32),
    App(String, u32, u32),
    File(String, u32, u32, Option<u32>, u32),
}

thread_local! {
    /// The bitmaps drawn since the last trim, and in `OLD` the ones from before it: a hit there
    /// moves back here, so what is still on screen stays cached and what moved on (the minutes of
    /// a clock, past CPU loads) goes at the next trim.
    static CACHE: RefCell<HashMap<Key, Option<Bitmap>>> = RefCell::new(HashMap::new());
    static OLD: RefCell<HashMap<Key, Option<Bitmap>>> = RefCell::new(HashMap::new());
    static FONTS: RefCell<HashMap<FontSpec, CFRetained<CTFont>>> = RefCell::new(HashMap::new());
    static METRICS: RefCell<HashMap<(String, FontSpec), TextMetrics>> = RefCell::new(HashMap::new());
}

/// Keeps the caches bounded, called after every render: once more than `max_entries` bitmaps were
/// drawn since the last trim, they become the old ones and the old ones are dropped. 0 drops
/// everything (a reload).
pub fn trim_cache(max_entries: usize) {
    if CACHE.with(|c| c.borrow().len()) > max_entries {
        let recent = CACHE.with(|c| c.take());
        OLD.with(|o| o.replace(recent));
    }
    if max_entries == 0 {
        OLD.with(|o| o.borrow_mut().clear());
    }
    METRICS.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() > max_entries * 4 {
            m.clear();
        }
    });
}

fn font(spec: &FontSpec) -> CFRetained<CTFont> {
    FONTS.with(|f| {
        f.borrow_mut()
            .entry(spec.clone())
            .or_insert_with(|| unsafe {
                CTFont::with_name(&CFString::from_str(&spec.postscript_name()), spec.size(), ptr::null())
            })
            .clone()
    })
}

fn line(text: &str, spec: &FontSpec, color: Rgba) -> CFRetained<CTLine> {
    let font = font(spec);
    let cg = color.cg();
    let keys: [&CFString; 2] = unsafe { [kCTFontAttributeName, kCTForegroundColorAttributeName] };
    let values: [&CFType; 2] = [&font, &cg];
    let attrs = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
    // The narrow no-break space of 12-hour times ("11:24 AM") has no width in Helvetica Neue
    let string = CFString::from_str(&text.replace('\u{202f}', "\u{a0}"));
    unsafe {
        let attributed =
            objc2_core_foundation::CFAttributedString::new(None, Some(&string), Some(attrs.as_ref()))
                .expect("CFAttributedString");
        CTLine::with_attributed_string(&attributed)
    }
}

pub fn measure_text(text: &str, spec: &FontSpec) -> TextMetrics {
    let key = (text.to_string(), spec.clone());
    if let Some(m) = METRICS.with(|m| m.borrow().get(&key).copied()) {
        return m;
    }
    let line = line(text, spec, Rgba(1.0, 1.0, 1.0, 1.0));
    let (mut ascent, mut descent, mut leading) = (0.0, 0.0, 0.0);
    let width = unsafe { line.typographic_bounds(&mut ascent, &mut descent, &mut leading) };
    let m = TextMetrics { width: width.ceil(), ascent, descent };
    METRICS.with(|c| c.borrow_mut().insert(key, m));
    m
}

/// A premultiplied BGRA bitmap context of `w`×`h` points at `scale`, with the CTM scaled so that
/// drawing happens in points.
fn bitmap_context(w: f64, h: f64, scale: f64) -> Option<CFRetained<CGContext>> {
    let pw = (w * scale).ceil().max(1.0) as usize;
    let ph = (h * scale).ceil().max(1.0) as usize;
    let space = CGColorSpace::new_device_rgb()?;
    let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
    let ctx = unsafe { CGBitmapContextCreate(ptr::null_mut(), pw, ph, 8, 0, Some(&space), info)? };
    CGContext::scale_ctm(Some(&ctx), scale, scale);
    Some(ctx)
}

fn finish(ctx: &CGContext, w: f64, h: f64) -> Option<Bitmap> {
    let image = CGBitmapContextCreateImage(Some(ctx))?;
    Some(Bitmap { image, width: w, height: h })
}

pub fn text(text: &str, spec: &FontSpec, color: Rgba, scale: f64) -> Option<Bitmap> {
    let key = Key::Text(text.to_string(), spec.clone(), color.key(), (scale * 100.0) as u32);
    cached(key, || {
        let m = measure_text(text, spec);
        let (w, h) = (m.width.max(1.0), m.height().ceil().max(1.0));
        let ctx = bitmap_context(w, h, scale)?;
        CGContext::set_should_antialias(Some(&ctx), true);
        CGContext::set_text_position(Some(&ctx), 0.0, h - m.ascent);
        unsafe { line(text, spec, color).draw(&ctx) };
        finish(&ctx, w, h)
    })
}

fn cached(key: Key, make: impl FnOnce() -> Option<Bitmap>) -> Option<Bitmap> {
    if let Some(hit) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let made = match OLD.with(|o| o.borrow_mut().remove(&key)) {
        Some(old) => old,
        None => make(),
    };
    CACHE.with(|c| c.borrow_mut().insert(key, made.clone()));
    made
}

/// Draws an NSImage filling `w`×`h` points into a new bitmap.
fn draw_nsimage(image: &NSImage, w: f64, h: f64, scale: f64) -> Option<Bitmap> {
    draw_tinted(image, w, h, None, scale)
}

/// Draws an NSImage filling `w`×`h` points, then paints `tint` over it: source atop paints only
/// where the image is, keeping its antialiased edges.
fn draw_tinted(image: &NSImage, w: f64, h: f64, tint: Option<Rgba>, scale: f64) -> Option<Bitmap> {
    let ctx = bitmap_context(w, h, scale)?;
    let ns_ctx = NSGraphicsContext::graphicsContextWithCGContext_flipped(&ctx, false);
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ns_ctx));
    image.drawInRect(NSRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w, h)));
    NSGraphicsContext::restoreGraphicsState_class();
    if let Some(color) = tint {
        self::tint(&ctx, w, h, color);
    }
    finish(&ctx, w, h)
}

/// Paints `color` where the context isn't transparent.
fn tint(ctx: &CGContext, w: f64, h: f64, color: Rgba) {
    CGContext::set_blend_mode(Some(ctx), CGBlendMode::SourceAtop);
    CGContext::set_fill_color_with_color(Some(ctx), Some(&color.cg()));
    CGContext::fill_rect(Some(ctx), objc2_core_foundation::CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w, h)));
}

/// `wifi@0.5`: a symbol with a variable value, which lights part of it (two bars of three).
/// Monochrome, in black: the caller paints it in its color afterwards, which keeps the glyphs that
/// fill symbols cut out of their shape (the bars of `pause.circle.fill`); a palette of one color
/// would fill them too.
fn symbol_image(name: &str, size: f64, weight: Weight) -> Option<Retained<NSImage>> {
    let base = match name.split_once('@') {
        Some((name, value)) => NSImage::imageWithSystemSymbolName_variableValue_accessibilityDescription(
            &NSString::from_str(name),
            value.parse().unwrap_or(1.0),
            None,
        )?,
        None => NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)?,
    };
    let sized = NSImageSymbolConfiguration::configurationWithPointSize_weight(size, weight.ns());
    let config = sized.configurationByApplyingConfiguration(&NSImageSymbolConfiguration::configurationPreferringMonochrome());
    base.imageWithSymbolConfiguration(&config)
}

/// An SF Symbol at `size` points and `weight`, in `color`. The bitmap has the symbol's own
/// proportions: its height follows the point size, as for text.
///
/// `laptopcomputer+speaker.wave.2.fill+mic.fill` is a symbol with badges: the others, small, at
/// its bottom right, cut out of it with a gap around them (a Mac that plays and records).
pub fn symbol(name: &str, size: f64, weight: Weight, color: Rgba, scale: f64) -> Option<Bitmap> {
    let key = Key::Symbol(name.to_string(), (size * 100.0) as u32, weight, color.key(), (scale * 100.0) as u32);
    cached(key, || {
        let _mtm = MainThreadMarker::new()?;
        let mut names = name.split('+');
        let base = names.next()?;
        let image = symbol_image(base, size, weight)?;
        let badges: Vec<Retained<NSImage>> = {
            let names: Vec<&str> = names.collect();
            let badge_size = size * if names.len() > 1 { 0.57 } else { 0.7 };
            names.iter().filter_map(|b| symbol_image(b, badge_size, weight)).collect()
        };
        let NSSize { width, height } = image.size();
        if badges.is_empty() {
            return draw_tinted(&image, width.ceil(), height.ceil(), Some(color), scale);
        }
        // The badges' corner sticks out a little to the right and below the symbol
        let unit = size / 14.0;
        let (gap, out) = (1.2 * unit, 0.4 * unit);
        let sizes: Vec<NSSize> = badges.iter().map(|b| b.size()).collect();
        let badges_width = sizes.iter().map(|s| s.width).sum::<f64>() + gap * (sizes.len() - 1) as f64;
        // On glasses a badge would hide a lens: they go further right
        let right = if base == "eyeglasses" { badges_width * 0.6 } else { out };
        let w = (width + right).max(badges_width).ceil();
        let h = (height + out).max(sizes.iter().map(|s| s.height).fold(0.0, f64::max)).ceil();
        let ctx = bitmap_context(w, h, scale)?;
        let ns_ctx = NSGraphicsContext::graphicsContextWithCGContext_flipped(&ctx, false);
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&ns_ctx));
        image.drawInRect(NSRect::new(CGPoint::new(0.0, out), CGSize::new(width, height)));
        let mut x = w;
        let places: Vec<NSRect> = sizes
            .iter()
            .map(|s| {
                x -= s.width;
                let r = NSRect::new(CGPoint::new(x, 0.0), *s);
                x -= gap;
                r
            })
            .collect();
        // The gap: each badge, moved around itself, erases what's under it
        for (badge, r) in badges.iter().zip(&places) {
            for i in 0..16 {
                let a = i as f64 * std::f64::consts::PI / 8.0;
                let moved = NSRect::new(CGPoint::new(r.origin.x + gap * a.cos(), r.origin.y + gap * a.sin()), r.size);
                badge.drawInRect_fromRect_operation_fraction(moved, NSRect::ZERO, objc2_app_kit::NSCompositingOperation::DestinationOut, 1.0);
            }
        }
        for (badge, r) in badges.iter().zip(&places) {
            badge.drawInRect(*r);
        }
        NSGraphicsContext::restoreGraphicsState_class();
        tint(&ctx, w, h, color);
        finish(&ctx, w, h)
    })
}

/// The icon of the app with `bundle_id` at `size` points square, as in the Dock.
pub fn app_icon(bundle_id: &str, size: f64, scale: f64) -> Option<Bitmap> {
    let key = Key::App(bundle_id.to_string(), (size * 100.0) as u32, (scale * 100.0) as u32);
    cached(key, || {
        let ws = NSWorkspace::sharedWorkspace();
        let url = ws.URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle_id))?;
        let path = url.path()?;
        let image = ws.iconForFile(&path);
        draw_nsimage(&image, size, size, scale)
    })
}

/// An image file (PNG, PDF, SVG…) scaled to `w`×`h` points, or an image of an app,
/// `/Applications/Some.app#name` (one of its resources, as `NSBundle.imageForResource` finds it),
/// in `tint` if given. A height of 0 keeps the image's proportions at width `w`, and a width of 0
/// at height `h`.
pub fn image_file(path: &str, w: f64, h: f64, tint: Option<Rgba>, scale: f64) -> Option<Bitmap> {
    let key = Key::File(path.to_string(), (w * 100.0) as u32, (h * 100.0) as u32, tint.map(|t| t.key()), (scale * 100.0) as u32);
    cached(key, || {
        let image = match path.split_once(".app#") {
            Some((app, name)) => {
                let bundle = objc2_foundation::NSBundle::bundleWithPath(&NSString::from_str(&format!("{app}.app")))?;
                let image: Option<Retained<NSImage>> = unsafe { objc2::msg_send![&*bundle, imageForResource: &*NSString::from_str(name)] };
                image?
            }
            None => NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(path))?,
        };
        let NSSize { width, height } = image.size();
        let (w, h) = match (w, h) {
            (w, 0.0) if width > 0.0 => (w, (w * height / width).round()),
            (0.0, h) if height > 0.0 => ((h * width / height).round(), h),
            size => size,
        };
        draw_tinted(&image, w, h, tint, scale)
    })
}
