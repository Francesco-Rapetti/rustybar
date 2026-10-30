//! Colors, fonts and the sizes shared by every widget. A preset palette plus overrides from the
//! `[theme]` table of the config; colors anywhere in the config or in Lua can be a palette name
//! (`mauve`, `primary`…) or a hex value.

use std::collections::HashMap;

use crate::config::ThemeConfig;
use crate::gfx::{FontSpec, Rgba, Weight};

#[derive(Clone, Debug)]
pub struct Theme {
    pub palette: HashMap<String, Rgba>,
    pub primary: Rgba,
    pub on_primary: Rgba,
    pub text: Rgba,
    pub subtext: Rgba,
    pub overlay: Rgba,
    pub surface: Rgba,
    pub highlight: Rgba,
    pub base: Rgba,
    pub red: Rgba,
    pub peach: Rgba,
    pub yellow: Rgba,
    pub green: Rgba,
    pub font: String,
    pub font_size: f64,
    pub small_size: f64,
    pub icon_size: f64,
    pub pill_height: f64,
    pub pill_radius: f64,
    pub pill_border: f64,
    pub pill_padding: f64,
    pub item_gap: f64,
    pub popup_radius: f64,
    pub popup_border: f64,
    pub popup_opacity: f64,
    pub popup_blur: bool,
    pub row_radius: f64,
    pub animations: bool,
}

/// Catppuccin Mocha, the palette of the original SketchyBar setup.
const MOCHA: &[(&str, &str)] = &[
    ("rosewater", "#f5e0dc"),
    ("flamingo", "#f2cdcd"),
    ("pink", "#f5c2e7"),
    ("mauve", "#cba6f7"),
    ("red", "#f38ba8"),
    ("maroon", "#eba0ac"),
    ("peach", "#fab387"),
    ("yellow", "#f9e2af"),
    ("green", "#a6e3a1"),
    ("teal", "#94e2d5"),
    ("sky", "#89dceb"),
    ("sapphire", "#74c7ec"),
    ("blue", "#89b4fa"),
    ("lavender", "#b4befe"),
    ("text", "#cdd6f4"),
    ("subtext1", "#bac2de"),
    ("subtext0", "#a6adc8"),
    ("overlay2", "#9399b2"),
    ("overlay1", "#7f849c"),
    ("overlay0", "#6c7086"),
    ("surface2", "#585b70"),
    ("surface1", "#45475a"),
    ("surface0", "#313244"),
    ("base", "#1e1e2e"),
    ("mantle", "#181825"),
    ("crust", "#11111b"),
];

const LATTE: &[(&str, &str)] = &[
    ("rosewater", "#dc8a78"),
    ("flamingo", "#dd7878"),
    ("pink", "#ea76cb"),
    ("mauve", "#8839ef"),
    ("red", "#d20f39"),
    ("maroon", "#e64553"),
    ("peach", "#fe640b"),
    ("yellow", "#df8e1d"),
    ("green", "#40a02b"),
    ("teal", "#179299"),
    ("sky", "#04a5e5"),
    ("sapphire", "#209fb5"),
    ("blue", "#1e66f5"),
    ("lavender", "#7287fd"),
    ("text", "#4c4f69"),
    ("subtext1", "#5c5f77"),
    ("subtext0", "#6c6f85"),
    ("overlay2", "#7c7f93"),
    ("overlay1", "#8c8fa1"),
    ("overlay0", "#9ca0b0"),
    ("surface2", "#acb0be"),
    ("surface1", "#bcc0cc"),
    ("surface0", "#ccd0da"),
    ("base", "#eff1f5"),
    ("mantle", "#e6e9ef"),
    ("crust", "#dce0e8"),
];

impl Theme {
    pub fn from_config(c: &ThemeConfig) -> Theme {
        let preset = match c.preset.as_str() {
            "catppuccin-latte" | "latte" => LATTE,
            _ => MOCHA,
        };
        let mut palette: HashMap<String, Rgba> = preset
            .iter()
            .map(|(k, v)| (k.to_string(), Rgba::parse(v).unwrap()))
            .collect();
        palette.insert("transparent".into(), Rgba::TRANSPARENT);
        palette.insert("white".into(), Rgba(1.0, 1.0, 1.0, 1.0));
        palette.insert("black".into(), Rgba(0.0, 0.0, 0.0, 1.0));
        for (name, value) in &c.colors {
            if let Some(color) = resolve(&palette, value) {
                palette.insert(name.clone(), color);
            }
        }
        let get = |name: &str, fallback: &str| {
            resolve(&palette, name)
                .or_else(|| resolve(&palette, fallback))
                .unwrap_or(Rgba(1.0, 0.0, 1.0, 1.0))
        };
        let mut theme = Theme {
            primary: get(&c.primary, "mauve"),
            on_primary: get("on_primary", "base"),
            text: get("text", "text"),
            subtext: get("subtext", "subtext0"),
            overlay: get("overlay", "overlay0"),
            surface: get("surface", "surface0"),
            highlight: get("highlight", "surface1"),
            base: get("base", "base"),
            red: get("red", "red"),
            peach: get("peach", "peach"),
            yellow: get("yellow", "yellow"),
            green: get("green", "green"),
            palette: HashMap::new(),
            font: c.font.clone(),
            font_size: c.font_size,
            small_size: c.small_size,
            icon_size: c.icon_size,
            pill_height: c.pill_height,
            pill_radius: c.pill_radius,
            pill_border: c.pill_border,
            pill_padding: c.pill_padding,
            item_gap: c.item_gap,
            popup_radius: c.popup_radius,
            popup_border: c.popup_border,
            popup_opacity: c.popup_opacity,
            popup_blur: c.popup_blur,
            row_radius: c.row_radius,
            animations: c.animations,
        };
        palette.insert("primary".into(), theme.primary);
        palette.insert("on_primary".into(), theme.on_primary);
        palette.insert("subtext".into(), theme.subtext);
        palette.insert("overlay".into(), theme.overlay);
        palette.insert("surface".into(), theme.surface);
        palette.insert("highlight".into(), theme.highlight);
        theme.palette = palette;
        theme
    }

    /// A palette name or a hex color.
    pub fn color(&self, s: &str) -> Option<Rgba> {
        resolve(&self.palette, s)
    }

    pub fn font(&self) -> FontSpec {
        FontSpec::new(&self.font, Weight::Bold, self.font_size)
    }

    pub fn small_font(&self) -> FontSpec {
        FontSpec::new(&self.font, Weight::Medium, self.small_size)
    }

    pub fn font_with(&self, weight: Weight, size: f64) -> FontSpec {
        FontSpec::new(&self.font, weight, size)
    }

    /// Yellow from `warn`, red from `crit`, otherwise `normal`: the thresholds of CPU, GPU,
    /// Claude usage…
    pub fn level(&self, value: f64, warn: f64, crit: f64, normal: Rgba) -> Rgba {
        if value >= crit {
            self.red
        } else if value >= warn {
            self.yellow
        } else {
            normal
        }
    }
}

fn resolve(palette: &HashMap<String, Rgba>, s: &str) -> Option<Rgba> {
    let s = s.trim();
    if let Some(c) = palette.get(&s.to_ascii_lowercase()) {
        return Some(*c);
    }
    // `name@0.5`: a palette color with an alpha
    if let Some((name, alpha)) = s.split_once('@') {
        let base = resolve(palette, name)?;
        return Some(base.with_alpha(alpha.parse().ok()?));
    }
    Rgba::parse(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_names_hex_and_alpha() {
        let theme = Theme::from_config(&ThemeConfig::default());
        assert_eq!(theme.color("mauve"), Rgba::parse("#cba6f7"));
        assert_eq!(theme.color("primary"), theme.color("mauve"));
        assert_eq!(theme.color("0xffcba6f7"), Rgba::parse("#cba6f7"));
        assert_eq!(theme.color("base@0.5").unwrap().3, 0.5);
        assert!(theme.color("nope").is_none());
    }
}
