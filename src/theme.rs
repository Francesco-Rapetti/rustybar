//! Colors, fonts and the sizes shared by every widget. A preset palette plus overrides from the
//! `[theme]` table of the config; colors anywhere in the config or in Lua can be a palette name
//! (`mauve`, `primary`…) or a hex value.

use std::collections::HashMap;

use objc2::rc::Retained;
use objc2_app_kit::{NSColor, NSColorSpace};

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
    /// Liquid Glass behind the pills and the popups, instead of the palette's `base`.
    pub glass: bool,
    /// The pills' glass is clear, as the Dock's, rather than frosted.
    pub clear_glass: bool,
}

/// The colors every preset fills, in the order of `Preset::colors`: Catppuccin's, so that `sky` or
/// `mauve` in a widget, in the config or in Lua is the same kind of color in every preset.
pub const NAMES: [&str; 26] = [
    "rosewater", "flamingo", "pink", "mauve", "red", "maroon", "peach", "yellow", "green", "teal", "sky", "sapphire", "blue", "lavender",
    "text", "subtext1", "subtext0", "overlay2", "overlay1", "overlay0", "surface2", "surface1", "surface0", "base", "mantle", "crust",
];

/// A built-in palette, from the theme's own repository. A theme with fewer accents than
/// Catppuccin repeats its nearest one (Dracula has no blue: its terminal blue is purple), and the
/// shades it lacks between the text and the background are blends of the two around them.
pub struct Preset {
    pub name: &'static str,
    pub label: &'static str,
    /// The 14 accents, then the text down to the background, as in `NAMES`.
    colors: [u32; 26],
}

impl Preset {
    pub fn color(&self, name: &str) -> Option<Rgba> {
        NAMES.iter().position(|n| *n == name).map(|i| rgb(self.colors[i]))
    }
}

/// The first one is the default.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "catppuccin-mocha",
        label: "Catppuccin Mocha",
        colors: [
            0xf5e0dc, 0xf2cdcd, 0xf5c2e7, 0xcba6f7, 0xf38ba8, 0xeba0ac, 0xfab387, 0xf9e2af, 0xa6e3a1, 0x94e2d5, 0x89dceb, 0x74c7ec, 0x89b4fa, 0xb4befe,
            0xcdd6f4, 0xbac2de, 0xa6adc8, 0x9399b2, 0x7f849c, 0x6c7086, 0x585b70, 0x45475a, 0x313244, 0x1e1e2e, 0x181825, 0x11111b,
        ],
    },
    Preset {
        name: "catppuccin-macchiato",
        label: "Catppuccin Macchiato",
        colors: [
            0xf4dbd6, 0xf0c6c6, 0xf5bde6, 0xc6a0f6, 0xed8796, 0xee99a0, 0xf5a97f, 0xeed49f, 0xa6da95, 0x8bd5ca, 0x91d7e3, 0x7dc4e4, 0x8aadf4, 0xb7bdf8,
            0xcad3f5, 0xb8c0e0, 0xa5adcb, 0x939ab7, 0x8087a2, 0x6e738d, 0x5b6078, 0x494d64, 0x363a4f, 0x24273a, 0x1e2030, 0x181926,
        ],
    },
    Preset {
        name: "catppuccin-frappe",
        label: "Catppuccin Frappé",
        colors: [
            0xf2d5cf, 0xeebebe, 0xf4b8e4, 0xca9ee6, 0xe78284, 0xea999c, 0xef9f76, 0xe5c890, 0xa6d189, 0x81c8be, 0x99d1db, 0x85c1dc, 0x8caaee, 0xbabbf1,
            0xc6d0f5, 0xb5bfe2, 0xa5adce, 0x949cbb, 0x838ba7, 0x737994, 0x626880, 0x51576d, 0x414559, 0x303446, 0x292c3c, 0x232634,
        ],
    },
    Preset {
        name: "catppuccin-latte",
        label: "Catppuccin Latte",
        colors: [
            0xdc8a78, 0xdd7878, 0xea76cb, 0x8839ef, 0xd20f39, 0xe64553, 0xfe640b, 0xdf8e1d, 0x40a02b, 0x179299, 0x04a5e5, 0x209fb5, 0x1e66f5, 0x7287fd,
            0x4c4f69, 0x5c5f77, 0x6c6f85, 0x7c7f93, 0x8c8fa1, 0x9ca0b0, 0xacb0be, 0xbcc0cc, 0xccd0da, 0xeff1f5, 0xe6e9ef, 0xdce0e8,
        ],
    },
    // Dracula's bright colors are its softer accents
    Preset {
        name: "dracula",
        label: "Dracula",
        colors: [
            0xff92df, 0xff6e6e, 0xff79c6, 0xbd93f9, 0xff5555, 0xff6e6e, 0xffb86c, 0xf1fa8c, 0x50fa7b, 0x8be9fd, 0xa4ffff, 0x8be9fd, 0xbd93f9, 0xd6acff,
            0xf8f8f2, 0xdadde2, 0xbcc2d3, 0x9ea8c3, 0x808db4, 0x6272a4, 0x535c7f, 0x44475a, 0x343746, 0x282a36, 0x21222c, 0x191a21,
        ],
    },
    Preset {
        name: "everforest-dark",
        label: "Everforest Dark",
        colors: [
            0xd699b6, 0xe67e80, 0xd699b6, 0xd699b6, 0xe67e80, 0xe67e80, 0xe69875, 0xdbbc7f, 0xa7c080, 0x83c092, 0x7fbbb3, 0x7fbbb3, 0x7fbbb3, 0x7fbbb3,
            0xd3c6aa, 0xc1bca7, 0xafb3a3, 0x9da9a0, 0x859289, 0x7a8478, 0x4f585e, 0x475258, 0x3d484d, 0x2d353b, 0x232a2e, 0x1a2022,
        ],
    },
    Preset {
        name: "everforest-light",
        label: "Everforest Light",
        colors: [
            0xdf69ba, 0xf85552, 0xdf69ba, 0xdf69ba, 0xf85552, 0xf85552, 0xf57d26, 0xdfa000, 0x8da101, 0x35a77c, 0x3a94c5, 0x3a94c5, 0x3a94c5, 0x3a94c5,
            0x5c6a72, 0x697777, 0x75847c, 0x829181, 0x939f91, 0xa6b0a0, 0xb2baa8, 0xbdc3af, 0xe0dcc7, 0xfdf6e3, 0xf4f0d9, 0xefebd4,
        ],
    },
    // Gruvbox, medium contrast: its bright colors on the dark one, the faded ones on the light one
    Preset {
        name: "gruvbox-dark",
        label: "Gruvbox Dark",
        colors: [
            0xd3869b, 0xfb4934, 0xd3869b, 0xd3869b, 0xfb4934, 0xfb4934, 0xfe8019, 0xfabd2f, 0xb8bb26, 0x8ec07c, 0x83a598, 0x458588, 0x83a598, 0x83a598,
            0xebdbb2, 0xd5c4a1, 0xbdae93, 0xa89984, 0x928374, 0x7c6f64, 0x665c54, 0x504945, 0x3c3836, 0x282828, 0x1d2021, 0x161819,
        ],
    },
    Preset {
        name: "gruvbox-light",
        label: "Gruvbox Light",
        colors: [
            0x8f3f71, 0x9d0006, 0x8f3f71, 0x8f3f71, 0x9d0006, 0xcc241d, 0xaf3a03, 0xb57614, 0x79740e, 0x427b58, 0x076678, 0x458588, 0x076678, 0x076678,
            0x3c3836, 0x504945, 0x665c54, 0x7c6f64, 0x928374, 0xa89984, 0xbdae93, 0xd5c4a1, 0xebdbb2, 0xfbf1c7, 0xf2e5bc, 0xeee0b7,
        ],
    },
    Preset {
        name: "kanagawa-wave",
        label: "Kanagawa Wave",
        colors: [
            0xd27e99, 0xe46876, 0xd27e99, 0x957fb8, 0xe46876, 0xff5d62, 0xffa066, 0xe6c384, 0x98bb6c, 0x7aa89f, 0xa3d4d5, 0x7fb4ca, 0x7e9cd8, 0x9cabca,
            0xdcd7ba, 0xc8c093, 0xb2ac88, 0x9d987e, 0x888574, 0x727169, 0x54546d, 0x363646, 0x2a2a37, 0x1f1f28, 0x181820, 0x16161d,
        ],
    },
    Preset {
        name: "kanagawa-dragon",
        label: "Kanagawa Dragon",
        colors: [
            0xa292a3, 0xc4746e, 0xa292a3, 0x8992a7, 0xc4746e, 0xc4746e, 0xb6927b, 0xc4b28a, 0x87a987, 0x8ea4a2, 0x8ba4b0, 0x658594, 0x8ba4b0, 0x949fb5,
            0xc5c9c5, 0xb6b8b0, 0xa6a69c, 0x9e9b93, 0x7a8382, 0x737c73, 0x625e5a, 0x393836, 0x282727, 0x181616, 0x12120f, 0x0d0c0c,
        ],
    },
    Preset {
        name: "kanagawa-lotus",
        label: "Kanagawa Lotus",
        colors: [
            0xb35b79, 0xd7474b, 0xb35b79, 0x624c83, 0xc84053, 0xd7474b, 0xcc6d00, 0xde9800, 0x6f894e, 0x597b75, 0x4e8ca2, 0x6693bf, 0x4d699b, 0x5d57a3,
            0x545464, 0x5f5f6a, 0x6a696f, 0x747475, 0x7f7e7a, 0x8a8980, 0xa09cac, 0xbab5a8, 0xd5cea3, 0xf2ecbc, 0xe5ddb0, 0xdcd5ac,
        ],
    },
    // Nord has nothing darker than nord0: mantle and crust darken it
    Preset {
        name: "nord",
        label: "Nord",
        colors: [
            0xd08770, 0xbf616a, 0xb48ead, 0xb48ead, 0xbf616a, 0xbf616a, 0xd08770, 0xebcb8b, 0xa3be8c, 0x8fbcbb, 0x88c0d0, 0x81a1c1, 0x81a1c1, 0x5e81ac,
            0xd8dee9, 0xc0c8d6, 0xa8b1c2, 0x919baf, 0x79849b, 0x616e88, 0x4c566a, 0x434c5e, 0x3b4252, 0x2e3440, 0x272c36, 0x20242d,
        ],
    },
    Preset {
        name: "one-dark",
        label: "One Dark",
        colors: [
            0xe06c75, 0xe06c75, 0xc678dd, 0xc678dd, 0xe06c75, 0xbe5046, 0xd19a66, 0xe5c07b, 0x98c379, 0x56b6c2, 0x56b6c2, 0x61afef, 0x61afef, 0x528bff,
            0xabb2bf, 0x9da4b2, 0x9097a4, 0x828997, 0x6f7684, 0x5c6370, 0x545a6a, 0x4b5263, 0x3e4452, 0x282c34, 0x21252b, 0x181a1f,
        ],
    },
    Preset {
        name: "one-light",
        label: "One Light",
        colors: [
            0xe45649, 0xe45649, 0xa626a4, 0xa626a4, 0xe45649, 0xca1243, 0x986801, 0xc18401, 0x50a14f, 0x0184bc, 0x0184bc, 0x4078f2, 0x4078f2, 0x526fff,
            0x383a42, 0x484b54, 0x595b65, 0x696c77, 0x84868f, 0xa0a1a7, 0xc2c2c3, 0xd0d0d0, 0xdfdfdf, 0xfafafa, 0xf0f0f0, 0xe7e9e1,
        ],
    },
    // Rosé Pine has no orange (gold) and a single green (leaf)
    Preset {
        name: "rose-pine",
        label: "Rosé Pine",
        colors: [
            0xebbcba, 0xebbcba, 0xeb6f92, 0xc4a7e7, 0xeb6f92, 0xeb6f92, 0xf6c177, 0xf6c177, 0x95b1ac, 0x31748f, 0x9ccfd8, 0x31748f, 0x9ccfd8, 0xc4a7e7,
            0xe0def4, 0xc5c3db, 0xaba7c3, 0x908caa, 0x7f7b98, 0x6e6a86, 0x605c76, 0x524f67, 0x403d52, 0x191724, 0x16141f, 0x100f17,
        ],
    },
    Preset {
        name: "rose-pine-moon",
        label: "Rosé Pine Moon",
        colors: [
            0xea9a97, 0xea9a97, 0xeb6f92, 0xc4a7e7, 0xeb6f92, 0xeb6f92, 0xf6c177, 0xf6c177, 0x95b1ac, 0x3e8fb0, 0x9ccfd8, 0x3e8fb0, 0x9ccfd8, 0xc4a7e7,
            0xe0def4, 0xc5c3db, 0xaba7c3, 0x908caa, 0x7f7b98, 0x6e6a86, 0x625e7a, 0x56526e, 0x44415a, 0x232136, 0x1f1d30, 0x171624,
        ],
    },
    Preset {
        name: "rose-pine-dawn",
        label: "Rosé Pine Dawn",
        colors: [
            0xd7827e, 0xd7827e, 0xb4637a, 0x907aa9, 0xb4637a, 0xb4637a, 0xea9d34, 0xea9d34, 0x6d8f89, 0x286983, 0x56949f, 0x286983, 0x56949f, 0x907aa9,
            0x464261, 0x575372, 0x686482, 0x797593, 0x88849c, 0x9893a5, 0xb3aeb9, 0xcecacd, 0xdfdad9, 0xfaf4ed, 0xf8f0e7, 0xf4ede8,
        ],
    },
    // Solarized: the same accents on both
    Preset {
        name: "solarized-dark",
        label: "Solarized Dark",
        colors: [
            0xd33682, 0xdc322f, 0xd33682, 0x6c71c4, 0xdc322f, 0xdc322f, 0xcb4b16, 0xb58900, 0x859900, 0x2aa198, 0x2aa198, 0x268bd2, 0x268bd2, 0x6c71c4,
            0x93a1a1, 0x839496, 0x74888c, 0x657b83, 0x5e747c, 0x586e75, 0x3d5b64, 0x224953, 0x073642, 0x002b36, 0x00252e, 0x001e26,
        ],
    },
    Preset {
        name: "solarized-light",
        label: "Solarized Light",
        colors: [
            0xd33682, 0xdc322f, 0xd33682, 0x6c71c4, 0xdc322f, 0xdc322f, 0xcb4b16, 0xb58900, 0x859900, 0x2aa198, 0x2aa198, 0x268bd2, 0x268bd2, 0x6c71c4,
            0x586e75, 0x657b83, 0x74888c, 0x839496, 0x8b9a9c, 0x93a1a1, 0xb1b9b2, 0xd0d0c4, 0xeee8d5, 0xfdf6e3, 0xf8f1de, 0xf3edda,
        ],
    },
    Preset {
        name: "tokyo-night",
        label: "Tokyo Night",
        colors: [
            0xf7768e, 0xf7768e, 0xff007c, 0xbb9af7, 0xf7768e, 0xdb4b4b, 0xff9e64, 0xe0af68, 0x9ece6a, 0x73daca, 0x7dcfff, 0x2ac3de, 0x7aa2f7, 0x9d7cd8,
            0xc0caf5, 0xa9b1d6, 0x8e96bc, 0x737aa2, 0x565f89, 0x545c7e, 0x414868, 0x3b4261, 0x292e42, 0x1a1b26, 0x16161e, 0x0c0e14,
        ],
    },
    Preset {
        name: "tokyo-night-storm",
        label: "Tokyo Night Storm",
        colors: [
            0xf7768e, 0xf7768e, 0xff007c, 0xbb9af7, 0xf7768e, 0xdb4b4b, 0xff9e64, 0xe0af68, 0x9ece6a, 0x73daca, 0x7dcfff, 0x2ac3de, 0x7aa2f7, 0x9d7cd8,
            0xc0caf5, 0xa9b1d6, 0x8e96bc, 0x737aa2, 0x646c96, 0x565f89, 0x545c7e, 0x414868, 0x3b4261, 0x24283b, 0x1f2335, 0x1b1e2d,
        ],
    },
    Preset {
        name: "tokyo-night-moon",
        label: "Tokyo Night Moon",
        colors: [
            0xfca7ea, 0xff757f, 0xfca7ea, 0xc099ff, 0xff757f, 0xc53b53, 0xff966c, 0xffc777, 0xc3e88d, 0x4fd6be, 0x86e1fc, 0x65bcff, 0x82aaff, 0x82aaff,
            0xc8d3f5, 0xa5afd6, 0x828bb8, 0x737aa2, 0x636da6, 0x545c7e, 0x444a73, 0x3b4261, 0x2f334d, 0x222436, 0x1e2030, 0x191b29,
        ],
    },
    Preset {
        name: "tokyo-night-day",
        label: "Tokyo Night Day",
        colors: [
            0xf52a65, 0xf52a65, 0xd20065, 0x9854f1, 0xf52a65, 0xc64343, 0xb15c00, 0x8c6c3e, 0x587539, 0x387068, 0x007197, 0x188092, 0x2e7de9, 0x7847bd,
            0x3760bf, 0x68709a, 0x6172b0, 0x767ea8, 0x848cb5, 0x8990b3, 0xa1a6c5, 0xa8aecb, 0xc4c8da, 0xe1e2e7, 0xd0d5e3, 0xc1c9df,
        ],
    },
];

/// The preset that follows macOS, and the default: its system colors, light or dark as the Mac is,
/// the accent of System Settings, and Liquid Glass behind the pills and the popups.
pub const MACOS: (&str, &str) = ("macos", "macOS");

/// Every preset's name and label, macOS first.
pub fn presets() -> impl Iterator<Item = (&'static str, &'static str)> {
    std::iter::once(MACOS).chain(PRESETS.iter().map(|p| (p.name, p.label)))
}

/// A preset's name as written in the config, as its name.
fn normalize(name: &str) -> String {
    name.trim().to_lowercase().replace('é', "e").replace([' ', '_'], "-")
}

/// The built-in palette called `name`, also written as its label (`Rosé Pine Moon`), and
/// Catppuccin's flavors without the prefix (`latte`).
pub fn preset(name: &str) -> Option<&'static Preset> {
    let name = normalize(name);
    PRESETS.iter().find(|p| p.name == name || p.name.strip_prefix("catppuccin-") == Some(name.as_str()))
}

pub fn is_macos(name: &str) -> bool {
    normalize(name) == MACOS.0
}

pub fn known(name: &str) -> bool {
    is_macos(name) || preset(name).is_some()
}

/// The colors of the preset called `name`, by palette name: those of `NAMES` and `accent`, the
/// default of `primary` (a built-in palette's `mauve`).
pub fn palette(name: &str) -> Option<Vec<(&'static str, Rgba)>> {
    if is_macos(name) {
        return Some(system_palette());
    }
    let p = preset(name)?;
    let mut colors: Vec<(&str, Rgba)> = NAMES.iter().copied().zip(p.colors.iter().map(|v| rgb(*v))).collect();
    colors.extend(p.color("mauve").map(|c| ("accent", c)));
    Some(colors)
}

/// The system colors as macOS draws them now, light or dark, on Catppuccin's names. The grays are
/// the label color at the opacities of the secondary and tertiary labels and of the fills, so on
/// glass they let it show through as the system's own controls do.
fn system_palette() -> Vec<(&'static str, Rgba)> {
    let srgb = NSColorSpace::sRGBColorSpace();
    // Resolved in the current appearance: the app's, which is the system's
    let get = |c: Retained<NSColor>| {
        c.colorUsingColorSpace(&srgb)
            .map(|c| Rgba(c.redComponent(), c.greenComponent(), c.blueComponent(), c.alphaComponent()))
            .unwrap_or(Rgba(1.0, 0.0, 1.0, 1.0))
    };
    let label = get(NSColor::labelColor());
    let secondary = get(NSColor::secondaryLabelColor());
    let tertiary = get(NSColor::tertiaryLabelColor());
    let fill = |a: f64| Rgba(label.0, label.1, label.2, a);
    let base = get(NSColor::windowBackgroundColor());
    let on_color = get(NSColor::alternateSelectedControlTextColor());
    let (pink, red, orange) = (get(NSColor::systemPinkColor()), get(NSColor::systemRedColor()), get(NSColor::systemOrangeColor()));
    vec![
        ("rosewater", pink),
        ("flamingo", pink),
        ("pink", pink),
        ("mauve", get(NSColor::systemPurpleColor())),
        ("red", red),
        ("maroon", red),
        ("peach", orange),
        ("yellow", get(NSColor::systemYellowColor())),
        ("green", get(NSColor::systemGreenColor())),
        ("teal", get(NSColor::systemMintColor())),
        ("sky", get(NSColor::systemCyanColor())),
        ("sapphire", get(NSColor::systemTealColor())),
        ("blue", get(NSColor::systemBlueColor())),
        ("lavender", get(NSColor::systemIndigoColor())),
        ("text", label),
        ("subtext1", label.mix(secondary, 0.5)),
        ("subtext0", secondary),
        ("overlay2", secondary.mix(tertiary, 1.0 / 3.0)),
        ("overlay1", secondary.mix(tertiary, 2.0 / 3.0)),
        ("overlay0", tertiary),
        ("surface2", fill(0.2)),
        ("surface1", fill(0.14)),
        ("surface0", fill(0.1)),
        ("base", base),
        ("mantle", base.mix(Rgba(0.0, 0.0, 0.0, 1.0), 0.08)),
        // What the other palettes use for text on their colors (`crust`), as macOS does: white
        ("crust", on_color),
        ("accent", get(NSColor::controlAccentColor())),
        ("on_primary", on_color),
    ]
}

fn rgb(v: u32) -> Rgba {
    let c = |shift: u32| ((v >> shift) & 0xff) as f64 / 255.0;
    Rgba(c(16), c(8), c(0), 1.0)
}

impl Theme {
    pub fn from_config(c: &ThemeConfig) -> Theme {
        let (colors, glass) = match palette(&c.preset) {
            Some(colors) => (colors, is_macos(&c.preset)),
            None => {
                crate::log(&format!("unknown theme preset {:?}, using {}", c.preset, MACOS.0));
                (system_palette(), true)
            }
        };
        let mut palette: HashMap<String, Rgba> = colors.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
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
            primary: get(&c.primary, "accent"),
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
            glass,
            clear_glass: !c.pill_glass.trim().eq_ignore_ascii_case("regular"),
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

    /// The background of a dashboard tile: a lit one takes a little of its color.
    pub fn tile(&self, color: Rgba, lit: bool) -> Rgba {
        if lit { self.surface.faded(0.55).mix(color.faded(0.55), 0.22) } else { self.surface.faded(0.55) }
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
        return Some(base.faded(alpha.parse().ok()?));
    }
    Rgba::parse(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_by_name_and_label() {
        for p in PRESETS {
            assert!(std::ptr::eq(preset(p.name).unwrap(), p));
            assert!(std::ptr::eq(preset(p.label).unwrap(), p), "{}", p.label);
        }
        assert_eq!(preset("latte").unwrap().name, "catppuccin-latte");
        assert_eq!(preset("Catppuccin Frappé").unwrap().name, "catppuccin-frappe");
        assert!(preset("nope").is_none());
        assert!(known("macOS") && !known("nope"));
    }

    /// The system's palette has every name of the others, and glass.
    #[test]
    fn macos_has_every_name() {
        let theme = Theme::from_config(&ThemeConfig::default());
        assert!(theme.glass);
        for name in NAMES.iter().chain(&["accent", "on_primary"]) {
            assert!(theme.color(name).is_some_and(|c| c.3 > 0.0), "{name}");
        }
        assert_eq!(theme.primary, theme.color("accent").unwrap());
        assert!(!Theme::from_config(&ThemeConfig { preset: "dracula".into(), ..Default::default() }).glass);
    }

    /// From the text down to the background each shade is farther from the text, in dark and light
    /// presets alike, and the text reads on the background.
    #[test]
    fn presets_go_from_text_to_base() {
        let lum = |c: Rgba| {
            let ch = |v: f64| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
            0.2126 * ch(c.0) + 0.7152 * ch(c.1) + 0.0722 * ch(c.2)
        };
        let ladder = &NAMES[14..24];
        for p in PRESETS {
            let l: Vec<f64> = ladder.iter().map(|n| lum(p.color(n).unwrap())).collect();
            let dark = l[0] > l[9];
            assert!(l.windows(2).all(|w| if dark { w[0] > w[1] } else { w[0] < w[1] }), "{}", p.name);
            let (hi, lo) = if dark { (l[0], l[9]) } else { (l[9], l[0]) };
            assert!((hi + 0.05) / (lo + 0.05) >= 4.5, "{}", p.name);
        }
    }

    #[test]
    fn resolves_names_hex_and_alpha() {
        let theme = Theme::from_config(&ThemeConfig { preset: "catppuccin-mocha".into(), primary: "mauve".into(), ..Default::default() });
        assert_eq!(theme.color("mauve"), Rgba::parse("#cba6f7"));
        assert_eq!(theme.color("primary"), theme.color("mauve"));
        assert_eq!(theme.color("0xffcba6f7"), Rgba::parse("#cba6f7"));
        assert_eq!(theme.color("base@0.5").unwrap().3, 0.5);
        assert!(theme.color("nope").is_none());
    }
}
