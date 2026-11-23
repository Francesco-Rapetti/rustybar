//! The mode of the function keys (see `fnkeys`): marks that show while F1, F2… are standard
//! function keys, so that play/pause, volume and brightness want fn, and quick ways to switch.
//! The mode comes from IOHIDSystem, and Keyboard Settings' notification (which `fnkeys::set`
//! posts too) says when it changes: no timer, no process.
//!
//! Marks (`show`, any of them; `["key"]` by default), only while F1, F2… are standard keys:
//! - `key`: its own pill, drawn as the fn key of the Mac's keyboards (fn and the globe); a dot when
//!   compact;
//! - `dot`: its own, a dot in the theme's accent;
//! - `apple`: fn beside the Apple logo (see `apple`);
//! - `island`: fn beside the bars of `now_playing`'s island, on the screens with a notch while it
//!   shows one. There it's the only mark: the others are for the screens with no island, and for
//!   a notch while nothing plays;
//! - `aerospace`: an F after the workspaces, as a mode of AeroSpace (see `aerospace`);
//! - `edge`: a tab at the left edge of the screen, before the bar (see `App::bar_tree`).
//!
//! Switching: a click on its pill, the dot, the island's fn, AeroSpace's F or the tab; its peek,
//! under the mouse (`hover`, on by default), with a switch; the Apple menu; `hotkey`, a
//! combination of RustyBar's own (see `hotkey::Combo`); `rustybar msg trigger fn_keys
//! [mode=on|off]`, for AeroSpace's bindings; and the settings window. With `apps` (bundle ids) F1, F2… are standard keys while one
//! of them is in front, with `media_apps` the special features; elsewhere the mode switched by
//! hand, which a switch in one of those apps changes only until another comes to the front.
//!
//! With made-up data (see `mock`) the mode is the `on` of `fn_keys.json`, and switching changes
//! only that, never the Mac's.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::{AnyThread, MainThreadMarker};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSRunningApplication, NSWorkspace, NSWorkspaceApplicationKey, NSWorkspaceDidActivateApplicationNotification};
use objc2_foundation::{NSNotification, NSObjectProtocol, NSString, NSUserDefaults, ns_string};
use serde::Deserialize;

use super::{Cx, Event, Rx, Widget};
use crate::config::{Config, Options};
use crate::fnkeys;
use crate::gfx::{Rgba, Weight};
use crate::hotkey::{Combo, HotKey};
use crate::i18n::t;
use crate::runtime::Listener;
use crate::theme::Theme;
use crate::ui::{Action, Align, Justify, Kind, Node};

/// Where a mark of the mode goes (see the top).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Key,
    Dot,
    Apple,
    Island,
    Aerospace,
    Edge,
}

impl Mark {
    pub const ALL: [Mark; 6] = [Mark::Key, Mark::Dot, Mark::Apple, Mark::Island, Mark::Aerospace, Mark::Edge];

    pub fn name(self) -> &'static str {
        match self {
            Mark::Key => "key",
            Mark::Dot => "dot",
            Mark::Apple => "apple",
            Mark::Island => "island",
            Mark::Aerospace => "aerospace",
            Mark::Edge => "edge",
        }
    }

    fn parse(name: &str) -> Option<Mark> {
        Mark::ALL.into_iter().find(|m| m.name() == name)
    }
}

/// What the other widgets and the bar need of this one: the mode, where its marks go and the
/// action that switches. Set by the widget for its generation: none of an older one's counts
/// after a reload, nor when no `fn_keys` is in the bar.
#[derive(Clone)]
struct Shared {
    generation: u64,
    widget: String,
    on: bool,
    show: Vec<Mark>,
    combo: Option<Combo>,
}

thread_local! {
    static SHARED: RefCell<Option<Shared>> = const { RefCell::new(None) };
}

fn shared() -> Option<Shared> {
    SHARED.with(|s| s.borrow().clone()).filter(|s| s.generation == crate::app::current_gen())
}

fn toggle_action(widget: &str) -> Action {
    Action::Widget { widget: widget.to_string(), name: "toggle".into() }
}

/// For the Apple menu: whether F1, F2… are standard keys, the action that switches and the
/// hotkey's label, when a `fn_keys` is in the bar.
pub fn switch() -> Option<(bool, Action, Option<String>)> {
    shared().map(|s| (s.on, toggle_action(&s.widget), s.combo.map(|c| c.label())))
}

/// Whether the mark `m` of `show` is for a screen, whose bar has an island or not: the island's
/// only there, and the others only where the island doesn't say it already.
fn shows(show: &[Mark], m: Mark, island: bool) -> bool {
    show.contains(&m)
        && match m {
            Mark::Island => island,
            _ => !(island && show.contains(&Mark::Island)),
        }
}

/// The action that switches, while the mark `m` shows on a screen, whose bar has an island or not
/// (`island`): F1, F2… are standard keys and the config puts a mark there (see `shows`).
pub fn mark(m: Mark, island: bool) -> Option<Action> {
    shared().filter(|s| s.on && shows(&s.show, m, island)).map(|s| toggle_action(&s.widget))
}

/// "fn" as the marks beside something else write it, in the accent.
pub fn label(th: &Theme, size: f64) -> Node {
    Node::text("fn", th.font_with(Weight::Bold, size), th.primary).key("fn")
}

/// How wide the tab at the edge of the screen shows.
pub const EDGE_WIDTH: f64 = 14.0;

/// The tab at the left edge of the screen (see `Mark::Edge`), `height` tall: its left end is past
/// the edge, so that only its right one is round.
pub fn edge_tab(th: &Theme, height: f64, action: Action) -> Node {
    let r = (EDGE_WIDTH / 2.0).floor();
    Node::row(vec![Node::text("F", th.font_with(Weight::Bold, 11.0), th.on_primary).key("f")])
        .key("fn_edge")
        .size(EDGE_WIDTH + r, height)
        .padding(0.0, 0.0, 0.0, r)
        .justify(Justify::Center)
        .radius(r)
        .background(th.primary)
        .click(action)
        // The hover color is the background while the mouse is on it: the accent still, lighter
        .hover(th.primary.mix(th.on_primary, 0.18))
}

/// "fn" in the island of the notch (see `Mark::Island`), with room around it for the mouse: a
/// click switches, as on the other marks.
pub fn island_mark(th: &Theme) -> Option<Node> {
    let action = mark(Mark::Island, true)?;
    Some(Node::row(vec![label(th, 12.0)]).key("fn").padding(1.0, 4.0, 1.0, 4.0).radius(6.0).click(action).hover(th.primary.faded(0.25)))
}

/// The problems of the options of the `fn_keys` widgets in the layout, for `rustybar check`.
pub fn check(config: &Config) -> Vec<String> {
    let l = &config.layout;
    let mut problems = Vec::new();
    for name in l.left.iter().chain(&l.center).chain(&l.right) {
        let options = Options(config.widgets.get(name).cloned().unwrap_or_default());
        if options.str("type").unwrap_or(name) != "fn_keys" {
            continue;
        }
        for m in options.strings("show") {
            if Mark::parse(&m).is_none() {
                let names: Vec<&str> = Mark::ALL.iter().map(|m| m.name()).collect();
                problems.push(format!("{name}: unknown mark {m:?} in show: one of {}", names.join(", ")));
            }
        }
        if let Some(Err(e)) = options.str("hotkey").filter(|h| !h.is_empty()).map(Combo::parse) {
            problems.push(format!("{name}: hotkey: {e}"));
        }
    }
    problems
}

/// The mode now, for the settings window: the Mac's, or with made-up data the `on` of
/// `fn_keys.json` (the bar may have switched its own since).
pub fn mode() -> bool {
    match crate::mock::load::<Mock>("fn_keys") {
        Some(m) => m.on,
        None => fnkeys::read().unwrap_or(false),
    }
}

/// Switches the mode from the settings window: through the bar when a `fn_keys` is in it (`in_bar`),
/// which keeps track of the apps that want one; else here, unless the data is made up.
pub fn request(on: bool, in_bar: bool) {
    crate::runtime::spawn("fn_keys", move || {
        let words = ["trigger".to_string(), "fn_keys".into(), format!("mode={}", if on { "on" } else { "off" })];
        let sent = in_bar && crate::ipc::send(&words).is_ok();
        if !sent
            && crate::mock::load::<Mock>("fn_keys").is_none()
            && let Err(e) = fnkeys::set(on)
        {
            crate::log(&format!("fn_keys: {e}"));
        }
    });
}

/// Made-up data (see `mock`).
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    on: bool,
}

fn defaults() -> Option<Retained<NSUserDefaults>> {
    NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(ns_string!("rustybar")))
}

/// The defaults key of the mode switched by hand, for each config folder: it's what the apps of
/// `apps` and `media_apps` go back to, also after a restart while one of them is in front.
fn base_key() -> String {
    format!("fn_keys.base{}", crate::config::instance())
}

pub struct FnKeys {
    /// F1, F2… are standard function keys.
    on: bool,
    /// The mode switched by hand (or in Keyboard Settings): the one of the apps of neither list.
    base: bool,
    show: Vec<Mark>,
    hover: bool,
    combo: Option<Combo>,
    apps: Vec<String>,
    media_apps: Vec<String>,
    /// The app in front, with its name, while there are apps to follow.
    front: String,
    front_name: String,
    mock: bool,
    listener: Option<Listener>,
    hotkey: Option<HotKey>,
    activations: Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

impl FnKeys {
    pub fn new(options: &Options) -> FnKeys {
        let mut show: Vec<Mark> = Vec::new();
        for name in options.strings("show") {
            match Mark::parse(&name) {
                Some(m) if !show.contains(&m) => show.push(m),
                Some(_) => {}
                None => crate::log(&format!("fn_keys: unknown mark {name:?} in show")),
            }
        }
        if options.0.get("show").is_none() {
            show.push(Mark::Key);
        }
        let combo = options.str("hotkey").filter(|h| !h.trim().is_empty()).and_then(|h| match Combo::parse(h) {
            Ok(c) => Some(c),
            Err(e) => {
                crate::log(&format!("fn_keys: hotkey: {e}"));
                None
            }
        });
        let mock = crate::mock::load::<Mock>("fn_keys");
        let on = mock.as_ref().is_some_and(|m| m.on);
        FnKeys {
            on,
            base: on,
            show,
            hover: options.bool("hover").unwrap_or(true),
            combo,
            apps: options.strings("apps"),
            media_apps: options.strings("media_apps"),
            front: String::new(),
            front_name: String::new(),
            mock: mock.is_some(),
            listener: None,
            hotkey: None,
            activations: None,
        }
    }

    /// The mode the apps of `apps` and `media_apps` want while `bundle` is in front.
    fn rule(&self, bundle: &str) -> Option<bool> {
        if self.apps.iter().any(|a| a == bundle) {
            Some(true)
        } else if self.media_apps.iter().any(|a| a == bundle) {
            Some(false)
        } else {
            None
        }
    }

    /// What the other widgets and the bar see of it, and a render for all of them.
    fn publish(&self, cx: &mut Cx) {
        let shared = Shared { generation: cx.generation, widget: cx.name.to_string(), on: self.on, show: self.show.clone(), combo: self.combo };
        SHARED.with(|s| *s.borrow_mut() = Some(shared));
        cx.redraw();
    }

    /// Switched by hand: the mode outside the apps that want one, unless one of them is in front,
    /// which keeps it only until another app is.
    fn switch(&mut self, on: bool, cx: &mut Cx) {
        if self.rule(&self.front).is_none() {
            self.remember(on);
        }
        self.apply(on, cx);
    }

    fn remember(&mut self, base: bool) {
        if self.base != base {
            self.base = base;
            self.store();
        }
    }

    /// The mode switched by hand in the defaults, for a restart (also after a crash) while an app
    /// that wants one is in front.
    fn store(&self) {
        if !self.mock
            && let Some(d) = defaults()
        {
            d.setBool_forKey(self.base, &NSString::from_str(&base_key()));
        }
    }

    /// Sets the Mac's mode (only this one's, with made-up data).
    fn apply(&mut self, on: bool, cx: &mut Cx) {
        if on == self.on {
            return;
        }
        if !self.mock
            && let Err(e) = fnkeys::set(on)
        {
            crate::log(&format!("fn_keys: {e}"));
            return;
        }
        self.on = on;
        self.publish(cx);
    }

    /// Keyboard Settings, or `set`, changed the mode: the one switched by hand, unless an app that
    /// wants one is in front.
    fn changed(&mut self, cx: &mut Cx) {
        let Some(on) = fnkeys::read() else { return };
        if self.rule(&self.front).is_none() {
            self.remember(on);
        }
        if on != self.on {
            self.on = on;
            self.publish(cx);
        }
    }

    /// Another app came to the front: the mode it wants, or the one switched by hand.
    fn activated(&mut self, bundle: String, name: String, cx: &mut Cx) {
        self.front = bundle;
        self.front_name = name;
        let on = self.rule(&self.front).unwrap_or(self.base);
        self.apply(on, cx);
        // The peek says which app it is for
        cx.redraw();
    }

    /// Follows the app in front, for `apps` and `media_apps`.
    fn follow_apps(&mut self, cx: &mut Cx) {
        let ws = NSWorkspace::sharedWorkspace();
        if let Some(app) = ws.frontmostApplication() {
            self.front = app.bundleIdentifier().map(|b| b.to_string()).unwrap_or_default();
            self.front_name = app.localizedName().map(|n| n.to_string()).unwrap_or_default();
        }
        let handle = cx.handle::<FnKeys>();
        let block = RcBlock::new(move |n: NonNull<NSNotification>| {
            let n = unsafe { n.as_ref() };
            let app = n
                .userInfo()
                .and_then(|info| info.objectForKey(unsafe { NSWorkspaceApplicationKey }))
                .and_then(|o| o.downcast::<NSRunningApplication>().ok());
            if let Some(app) = app {
                let bundle = app.bundleIdentifier().map(|b| b.to_string()).unwrap_or_default();
                let name = app.localizedName().map(|n| n.to_string()).unwrap_or_default();
                handle.send(move |w, cx| w.activated(bundle, name, cx));
            }
        });
        self.activations = Some(unsafe { ws.notificationCenter().addObserverForName_object_queue_usingBlock(Some(NSWorkspaceDidActivateApplicationNotification), None, None, &block) });
    }

    /// Its own pill, drawn as the fn key: fn at the top right, the globe at the bottom left.
    fn key(&self, rx: &Rx) -> Node {
        let th = rx.theme;
        let side = th.pill_height;
        let face = Node::column(vec![
            Node::row(vec![label(th, 10.0)]).justify(Justify::End).key("top"),
            Node::row(vec![Node::symbol("globe", 9.0, Weight::Semibold, th.subtext).key("globe")]).justify(Justify::Start).key("bottom"),
        ])
        .align(Align::Stretch)
        .justify(Justify::Between)
        .size(side - 8.0, side - 8.0)
        .key("face");
        let pill = rx.pill(vec![face]).size(side, side).padding(4.0, 4.0, 4.0, 4.0).radius((side / 4.0).round()).click(toggle_action(rx.name));
        if self.hover { pill.hover(Rgba(0.0, 0.0, 0.0, 0.0)) } else { pill }
    }

    /// Its own dot, in the accent, with room around it for the mouse.
    fn dot(&self, rx: &Rx) -> Node {
        let th = rx.theme;
        let dot = Node::default().size(8.0, 8.0).radius(4.0).background(th.primary).key("dot");
        let node = Node::row(vec![dot]).size(16.0, th.pill_height).justify(Justify::Center).radius(8.0).click(toggle_action(rx.name));
        if self.hover { node.hover(Rgba(0.0, 0.0, 0.0, 0.0)) } else { node }
    }
}

impl Widget for FnKeys {
    fn start(&mut self, cx: &mut Cx) {
        if let Some(combo) = self.combo {
            let handle = cx.handle::<FnKeys>();
            match crate::hotkey::register(combo, move || handle.send(|w, cx| w.switch(!w.on, cx))) {
                Ok(k) => self.hotkey = Some(k),
                Err(e) => crate::log(&format!("fn_keys: hotkey: {e}")),
            }
        }
        if self.mock {
            self.publish(cx);
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else { return };
        let handle = cx.handle::<FnKeys>();
        self.listener = Some(Listener::new(mtm, fnkeys::CHANGED, move |_| handle.send(|w, cx| w.changed(cx))));
        self.on = fnkeys::read().unwrap_or(false);
        if self.apps.is_empty() && self.media_apps.is_empty() {
            self.base = self.on;
        } else {
            self.follow_apps(cx);
            // Restarted while an app that wants a mode is in front: the one switched by hand is
            // the last one remembered, if any (the first time, the Mac's)
            let key = NSString::from_str(&base_key());
            let remembered = defaults().filter(|d| d.objectForKey(&key).is_some()).map(|d| d.boolForKey(&key));
            self.base = match self.rule(&self.front) {
                Some(_) => remembered.unwrap_or(self.on),
                None => self.on,
            };
            self.store();
            let on = self.rule(&self.front).unwrap_or(self.base);
            self.apply(on, cx);
        }
        self.publish(cx);
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        if !self.on {
            return None;
        }
        let key = shows(&self.show, Mark::Key, rx.notch_island);
        if key && !rx.compact {
            Some(self.key(rx))
        } else if key || shows(&self.show, Mark::Dot, rx.notch_island) {
            Some(self.dot(rx))
        } else {
            None
        }
    }

    /// Under the mouse: the switch, what fn does now, the app that wants this mode and the hotkey.
    fn peek(&self, rx: &Rx) -> Option<Node> {
        if !self.hover || !self.on {
            return None;
        }
        let th = rx.theme;
        let switch = Node::new(Kind::Toggle { on: self.on, color: th.primary, track: th.surface }).key("toggle");
        let mut rows = vec![
            rx.row_button(vec![Node::text(t("Tasti F1, F2… standard", "F1, F2… as standard keys"), th.font(), th.text).key("title")], vec![switch], toggle_action(rx.name)).key("switch"),
            rx.detail(t("Per luminosità, volume e musica tieni premuto fn", "Hold fn for brightness, volume and music")),
        ];
        if self.rule(&self.front) == Some(true) && !self.front_name.is_empty() {
            rows.push(rx.detail(&format!("{} {}", t("Per le app della config, ora", "For the apps of the config, now"), self.front_name)));
        }
        if let Some(c) = self.combo {
            rows.push(
                Node::row(vec![
                    Node::text(t("Scorciatoia", "Shortcut"), th.small_font(), th.subtext).key("label"),
                    Node::spacer(),
                    Node::text(c.label(), th.font_with(Weight::Medium, 12.0), th.subtext).key("combo"),
                ])
                .padding(2.0, 8.0, 0.0, 8.0)
                .key("hotkey"),
            );
        }
        Some(Node::column(rows).padding(4.0, 6.0, 8.0, 6.0).gap(2.0).min_width(240.0))
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        if name == "toggle" {
            self.switch(!self.on, cx);
        }
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        match event {
            // `rustybar msg trigger fn_keys [mode=on|off]`: AeroSpace's bindings, the settings
            Event::Trigger { name, args } if name == "fn_keys" => {
                let on = match args.get("mode").map(String::as_str) {
                    Some("on") => true,
                    Some("off") => false,
                    _ => !self.on,
                };
                self.switch(on, cx);
            }
            Event::Wake if !self.mock => self.changed(cx),
            _ => {}
        }
    }

    crate::any_widget!();
}

impl Drop for FnKeys {
    fn drop(&mut self) {
        if let Some(o) = self.activations.take() {
            unsafe { NSWorkspace::sharedWorkspace().notificationCenter().removeObserver(o.as_ref()) };
        }
        // Going away (a reload, quit, out of the layout) with the mode of the app in front: the
        // one switched by hand again, as nothing else would bring it back. After a reload the new
        // widget sets the app's again.
        if !self.mock
            && self.on != self.base
            && self.rule(&self.front).is_some()
            && let Err(e) = fnkeys::set(self.base)
        {
            crate::log(&format!("fn_keys: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widget(toml: toml::Table) -> FnKeys {
        FnKeys::new(&Options(toml))
    }

    #[test]
    fn marks() {
        assert_eq!(widget(toml::Table::new()).show, vec![Mark::Key]);
        assert_eq!(widget(toml::toml! { show = ["apple", "edge", "apple", "nope"] }).show, vec![Mark::Apple, Mark::Edge]);
        assert!(widget(toml::toml! { show = [] }).show.is_empty());
    }

    #[test]
    fn the_island_says_it_alone_on_its_screen() {
        let show = [Mark::Island, Mark::Edge, Mark::Apple];
        // Beside an island, only its mark; on a screen without one, the others
        assert!(shows(&show, Mark::Island, true));
        assert!(!shows(&show, Mark::Edge, true));
        assert!(!shows(&show, Mark::Apple, true));
        assert!(!shows(&show, Mark::Island, false));
        assert!(shows(&show, Mark::Edge, false));
        assert!(shows(&show, Mark::Apple, false));
        // Marks that aren't asked for show nowhere, and with no mark in the island the others
        // stay beside one
        assert!(!shows(&show, Mark::Dot, false));
        assert!(shows(&[Mark::Edge], Mark::Edge, true));
        assert!(!shows(&[Mark::Edge], Mark::Island, true));
    }

    #[test]
    fn apps_want_a_mode_and_the_rest_the_one_switched_by_hand() {
        let w = widget(toml::toml! { apps = ["com.example.ide"] media_apps = ["com.example.player"] });
        assert_eq!(w.rule("com.example.ide"), Some(true));
        assert_eq!(w.rule("com.example.player"), Some(false));
        assert_eq!(w.rule("com.example.other"), None);
    }

    #[test]
    fn check_names_the_wrong_marks_and_hotkeys() {
        let mut config = Config::default();
        config.layout.right = vec!["fn_keys".into(), "keys2".into()];
        config.widgets.insert("fn_keys".into(), toml::toml! { show = ["key", "edg"] hotkey = "ctrl-alt-f" });
        config.widgets.insert("keys2".into(), toml::toml! { type = "fn_keys" hotkey = "hyper-f" });
        let problems = check(&config);
        assert_eq!(problems.len(), 2);
        assert!(problems[0].contains("\"edg\""));
        assert!(problems[1].starts_with("keys2: hotkey"));
    }
}
