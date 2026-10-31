//! `~/.config/rustybar/rustybar.toml`: the bar, the theme, which widgets go where and their options.
//! Every key has a default, so an empty file (or none) gives a working bar. `rustybar.local.toml`
//! next to it, if there is one, goes over it: for what stays out of a shared repo, such as a server
//! or accounts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const DEFAULT_CONFIG: &str = include_str!("../config/rustybar.toml");

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub bar: BarConfig,
    pub theme: ThemeConfig,
    pub layout: LayoutConfig,
    pub displays: DisplayConfig,
    /// Options of each widget, by the name used in `[layout]`. `type` picks the kind of widget when
    /// the name isn't one (`[widgets.claude_work] type = "claude"`).
    pub widgets: BTreeMap<String, toml::Table>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct BarConfig {
    pub height: f64,
    /// From the screen edges to the first and last pill.
    pub margin: f64,
    /// Above the menu bar (true) or below it, so that the menu bar shows over the bar when it
    /// slides in.
    pub topmost: bool,
    /// Moves the bar down: for trying RustyBar under another bar.
    pub offset: f64,
    /// Between the bar and the popups.
    pub popup_gap: f64,
}

impl Default for BarConfig {
    fn default() -> Self {
        BarConfig { height: 40.0, margin: 8.0, topmost: false, offset: 0.0, popup_gap: 6.0 }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    pub preset: String,
    pub primary: String,
    /// Palette overrides and additions: `base = "#1e1e2e"`, `accent2 = "peach"`.
    pub colors: BTreeMap<String, String>,
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
    /// Off: no animations at all. They also stop when macOS has Reduce Motion on.
    pub animations: bool,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        ThemeConfig {
            preset: "catppuccin-mocha".into(),
            primary: "mauve".into(),
            colors: BTreeMap::new(),
            font: "Helvetica Neue".into(),
            font_size: 13.0,
            small_size: 11.0,
            icon_size: 14.0,
            pill_height: 28.0,
            pill_radius: 10.0,
            pill_border: 2.0,
            pill_padding: 10.0,
            item_gap: 8.0,
            popup_radius: 10.0,
            popup_border: 2.0,
            popup_opacity: 0.9,
            popup_blur: true,
            row_radius: 6.0,
            animations: true,
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct LayoutConfig {
    pub left: Vec<String>,
    pub center: Vec<String>,
    pub right: Vec<String>,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        LayoutConfig {
            left: vec!["apple".into(), "aerospace".into(), "front_app".into()],
            center: vec![],
            right: vec!["system".into(), "battery".into(), "clock".into()],
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct DisplayConfig {
    /// On screens with a notch the widgets draw their compact form (icons, short times), so that
    /// the right side doesn't end up under the notch. Off: always the full form.
    pub compact_on_notch: bool,
    /// Widgets hidden on screens with a notch.
    pub hide_on_notch: Vec<String>,
    /// Widgets that draw their compact form on every screen.
    pub always_compact: Vec<String>,
    /// From this width in points a screen is "large": widgets can show more there (one pill per
    /// Claude account…).
    pub large_width: f64,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        DisplayConfig {
            compact_on_notch: true,
            hide_on_notch: vec![],
            always_compact: vec![],
            large_width: 3840.0,
        }
    }
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RUSTYBAR_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".config"));
    base.join("rustybar")
}

pub fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/"))
}

pub fn config_path() -> PathBuf {
    config_dir().join("rustybar.toml")
}

pub fn local_path() -> PathBuf {
    config_dir().join("rustybar.local.toml")
}

/// Reads the config, writing the default one first if there is none. An error keeps the message
/// for the bar to show; the caller keeps the config it had.
pub fn load() -> Result<Config, String> {
    let path = config_path();
    if !path.exists() {
        let _ = std::fs::create_dir_all(config_dir());
        let _ = std::fs::write(&path, DEFAULT_CONFIG);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let local = local_path();
    let Ok(more) = std::fs::read_to_string(&local) else {
        return parse(&text).map_err(|e| format!("{}: {e}", short(&path)));
    };
    let mut table: toml::Table = text.parse().map_err(|e| format!("{}: {e}", short(&path)))?;
    merge(&mut table, more.parse().map_err(|e| format!("{}: {e}", short(&local)))?);
    toml::Value::Table(table).try_into().map_err(|e: toml::de::Error| format!("{} + {}: {e}", short(&path), short(&local)))
}

/// Puts `more` over `base`: tables merge key by key, anything else (arrays too) is replaced.
fn merge(base: &mut toml::Table, more: toml::Table) {
    for (key, value) in more {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(b)), toml::Value::Table(m)) => merge(b, m),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

pub fn parse(text: &str) -> Result<Config, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

fn short(path: &Path) -> String {
    let home = home();
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Typed access to the options of a widget.
#[derive(Clone, Debug, Default)]
pub struct Options(pub toml::Table);

impl Options {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.as_str())
    }

    pub fn f64(&self, key: &str) -> Option<f64> {
        self.0.get(key).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.0.get(key).and_then(|v| v.as_bool())
    }

    pub fn strings(&self, key: &str) -> Vec<String> {
        self.0
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_goes_over() {
        let mut base: toml::Table = "[layout]\nright = [\"clock\"]\n[widgets.home]\ndashboard = \"/\"".parse().unwrap();
        merge(&mut base, "[layout]\nright = [\"home\", \"clock\"]\n[widgets.home]\nserver = \"http://ha.local\"".parse().unwrap());
        let c: Config = toml::Value::Table(base).try_into().unwrap();
        assert_eq!(c.layout.right, vec!["home", "clock"]);
        let home = Options(c.widgets["home"].clone());
        assert_eq!((home.str("server"), home.str("dashboard")), (Some("http://ha.local"), Some("/")));
    }

    #[test]
    fn default_file_parses() {
        let c = parse(DEFAULT_CONFIG).unwrap();
        assert!(!c.layout.left.is_empty());
        assert_eq!(c.bar.height, 40.0);
    }

    #[test]
    fn empty_file_is_default() {
        let c = parse("").unwrap();
        assert_eq!(c.theme.pill_height, 28.0);
    }

    #[test]
    fn unknown_keys_are_errors() {
        assert!(parse("[bar]\nheigth = 3").is_err());
    }
}
