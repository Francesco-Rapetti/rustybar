//! Widgets: each one keeps its own state, updates it from events, timers and background work,
//! and describes itself as nodes for the bar and, optionally, for a popup.

use std::any::Any;
use std::collections::HashMap;

use crate::config::Options;
use crate::display::DisplayInfo;
use crate::fit::Form;
use crate::runtime::{self, Timer};
use crate::theme::Theme;
use crate::gfx::Weight;
use crate::ui::{Action, Align, Kind, Node};

pub mod aerospace;
pub mod apple;
pub mod audio;
pub mod battery;
pub mod claude;
pub mod clock;
pub mod docker;
pub mod events;
pub mod front_app;
pub mod home;
pub mod network;
pub mod notifications;
pub mod system;

/// Something that happened outside the widget.
#[derive(Clone, Debug)]
pub enum Event {
    Wake,
    ScreensChanged,
    /// `rustybar msg trigger <name> [key=value…]`, or a widget's own trigger.
    Trigger { name: String, args: HashMap<String, String> },
    PopupOpened,
    PopupClosed,
}

/// What a widget needs to draw itself.
pub struct Rx<'a> {
    pub name: &'a str,
    pub theme: &'a Theme,
    pub display: &'a DisplayInfo,
    /// Draw the short form: icons, short times. When the full one doesn't leave room between the
    /// left and right sides of the bar (see `fit`), or the config asks for it.
    pub compact: bool,
    /// Leave out what the bar can do without, for the widgets of `slim_first`: the first thing to
    /// go when there's no room, before any widget turns compact; so also when compact.
    pub slim: bool,
    pub popup_open: bool,
}

impl<'a> Rx<'a> {
    pub fn new(name: &'a str, theme: &'a Theme, display: &'a DisplayInfo, form: Form, popup_open: bool) -> Self {
        let compact = form == Form::Compact;
        Rx { name, theme, display, compact, slim: compact || form == Form::Slim, popup_open }
    }

    /// An action sent back to this widget with `name`.
    pub fn action(&self, name: &str) -> Action {
        Action::Widget { widget: self.name.to_string(), name: name.to_string() }
    }

    pub fn toggle_popup(&self) -> Action {
        Action::Popup { widget: self.name.to_string() }
    }

    /// The rounded background of a component in the bar.
    pub fn pill(&self, children: Vec<Node>) -> Node {
        let t = self.theme;
        let pill = Node::row(children)
            .key(self.name)
            .height(t.pill_height)
            .padding(0.0, t.pill_padding, 0.0, t.pill_padding)
            .gap(6.0)
            .radius(t.pill_radius);
        if !t.glass {
            pill.background(t.base).border(t.pill_border, t.surface)
        } else if t.clear_glass {
            // Clear glass shows the wallpaper as it is: a veil of the base keeps the text legible
            pill.glass().background(t.base.faded(0.3))
        } else {
            pill.glass()
        }
    }

    /// The usual popup: a column with the popup's padding and a minimum width.
    pub fn popup(&self, min_width: f64, children: Vec<Node>) -> Node {
        Node::column(children).padding(8.0, 8.0, 8.0, 8.0).gap(2.0).min_width(min_width)
    }

    /// A popup row that does something: the hover highlight comes with the click.
    pub fn row_button(&self, left: Vec<Node>, right: Vec<Node>, action: Action) -> Node {
        let mut children = left;
        children.push(Node::spacer());
        children.extend(right);
        Node::row(children).gap(8.0).pad(4.0, 8.0).height(26.0).radius(self.theme.row_radius).click(action)
    }

    /// A slider at `value` (0..1) with its percentage inside; `action` gets the value where it's
    /// released.
    pub fn slider(&self, value: f64, action: Action) -> Node {
        let t = self.theme;
        Node::new(Kind::Slider { value, color: t.primary, track: t.surface, font: Some(t.font_with(Weight::Bold, 11.0)) }).click(action)
    }

    pub fn section(&self, title: &str, right: Option<Node>) -> Node {
        let t = self.theme;
        let mut row = vec![Node::text(title, t.font(), t.text), Node::spacer()];
        if let Some(r) = right {
            row.push(r);
        }
        Node::row(row).padding(8.0, 8.0, 4.0, 8.0).align(Align::Center)
    }

    pub fn detail(&self, text: &str) -> Node {
        Node::text(text, self.theme.small_font(), self.theme.subtext).key(format!("d-{text}")).padding(0.0, 8.0, 2.0, 8.0)
    }

    pub fn divider(&self) -> Node {
        Node::default().height(1.0).background(self.theme.surface).padding(0.0, 0.0, 0.0, 0.0)
    }
}

/// What a widget can do while it handles something. Changes to the app (popups, renders) are
/// collected and applied after the widget returns.
pub struct Cx<'a> {
    pub name: &'a str,
    pub generation: u64,
    pub popup_open: bool,
    pub(crate) timers: &'a mut Vec<Timer>,
    pub(crate) effects: Vec<Effect>,
}

pub enum Effect {
    Redraw,
    ClosePopup,
    TogglePopup,
}

impl Cx<'_> {
    /// Draw the widget again (bar and popup) at the end of this run loop pass.
    pub fn redraw(&mut self) {
        self.effects.push(Effect::Redraw);
    }

    pub fn close_popup(&mut self) {
        self.effects.push(Effect::ClosePopup);
    }

    /// Opens this widget's popup on the screen under the mouse, or closes it: for a widget with
    /// more than one pill, whose click picks what the popup shows first.
    pub fn toggle_popup(&mut self) {
        self.effects.push(Effect::TogglePopup);
    }

    /// Calls `f` on this widget every `secs` seconds, until a reload.
    pub fn every<W: Widget>(&mut self, secs: f64, f: impl Fn(&mut W, &mut Cx) + 'static) {
        let (name, generation) = (self.name.to_string(), self.generation);
        self.timers.push(Timer::every(secs, move |app| app.with_widget::<W>(&name, generation, |w, cx| f(w, cx))));
    }

    /// Calls `f` on this widget once, after `secs` seconds.
    pub fn after<W: Widget>(&mut self, secs: f64, f: impl FnOnce(&mut W, &mut Cx) + 'static) {
        let (name, generation) = (self.name.to_string(), self.generation);
        runtime::after(secs, move |app| app.with_widget::<W>(&name, generation, f));
    }

    /// Runs `work` on a background thread and gives its result to `done` on this widget, on the
    /// main thread. Results that arrive after a reload are dropped.
    pub fn background<W: Widget, T: Send + 'static>(
        &mut self,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&mut W, T, &mut Cx) + Send + 'static,
    ) {
        let (name, generation) = (self.name.to_string(), self.generation);
        runtime::spawn(&format!("{name}-work"), move || {
            let result = work();
            runtime::post(move |app| app.with_widget::<W>(&name, generation, move |w, cx| done(w, result, cx)));
        });
    }

    /// A handle that background threads use to reach this widget.
    pub fn handle<W: Widget>(&self) -> Handle<W> {
        Handle { name: self.name.to_string(), generation: self.generation, _w: std::marker::PhantomData }
    }
}

/// Sends closures to a widget from any thread. Stale after a reload: then they're dropped.
pub struct Handle<W> {
    name: String,
    generation: u64,
    _w: std::marker::PhantomData<fn() -> W>,
}

impl<W> Clone for Handle<W> {
    fn clone(&self) -> Self {
        Handle { name: self.name.clone(), generation: self.generation, _w: std::marker::PhantomData }
    }
}

impl<W: Widget> Handle<W> {
    pub fn send(&self, f: impl FnOnce(&mut W, &mut Cx) + Send + 'static) {
        let (name, generation) = (self.name.clone(), self.generation);
        runtime::post(move |app| app.with_widget::<W>(&name, generation, f));
    }

    /// False once the widget is gone (a reload or quit): long-running threads stop then.
    pub fn alive(&self) -> bool {
        crate::app::current_gen() == self.generation
    }
}

pub trait Widget: Any {
    /// Sets up listeners, timers and threads. Called once, after creation.
    fn start(&mut self, _cx: &mut Cx) {}

    /// The widget in the bar on this display, or None to hide it there.
    fn bar(&self, rx: &Rx) -> Option<Node>;

    /// The contents of the popup, if the widget has one.
    fn popup(&self, _rx: &Rx) -> Option<Node> {
        None
    }

    /// False until the first data arrives: the bar shows the last known state, dimmed, or a
    /// skeleton.
    fn ready(&self) -> bool {
        true
    }

    fn action(&mut self, _name: &str, _value: Option<f64>, _cx: &mut Cx) {}

    fn event(&mut self, _event: &Event, _cx: &mut Cx) {}

    fn as_any(&mut self) -> &mut dyn Any;
}

/// The kinds `create` knows, in the order of the settings window's sidebar.
pub const KINDS: &[&str] = &["apple", "aerospace", "front_app", "home", "system", "docker", "claude", "audio", "network", "battery", "events", "clock", "notifications"];

/// The name among `known` that `name` is most likely a typo of: at most two letters off.
pub fn closest<'a>(name: &str, known: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    // Levenshtein, one row at a time
    let distance = |a: &str, b: &str| {
        let b: Vec<char> = b.chars().collect();
        let mut row: Vec<usize> = (0..=b.len()).collect();
        for (i, ca) in a.chars().enumerate() {
            let mut prev = row[0];
            row[0] = i + 1;
            for (j, cb) in b.iter().enumerate() {
                let next = (row[j + 1] + 1).min(row[j] + 1).min(prev + usize::from(ca != *cb));
                prev = row[j + 1];
                row[j + 1] = next;
            }
        }
        row[b.len()]
    };
    let name = name.to_lowercase();
    known.into_iter().map(|k| (distance(&name, &k.to_lowercase()), k)).filter(|(d, _)| *d <= 2).min_by_key(|(d, _)| *d).map(|(_, k)| k)
}

/// Creates the widget of kind `kind` (the name in `[layout]`, or its `type`).
pub fn create(kind: &str, options: &Options) -> Option<Box<dyn Widget>> {
    Some(match kind {
        "audio" => Box::new(audio::Audio::new(options)),
        "network" => Box::new(network::Network::new(options)),
        "notifications" => Box::new(notifications::Notifications::new(options)),
        "clock" => Box::new(clock::Clock::new(options)),
        "docker" => Box::new(docker::Docker::new(options)),
        "events" => Box::new(events::Events::new(options)),
        "front_app" => Box::new(front_app::FrontApp::default()),
        "home" => Box::new(home::Home::new(options)),
        "apple" => Box::new(apple::Apple::new(options)),
        "aerospace" => Box::new(aerospace::Aerospace::new(options)),
        "battery" => Box::new(battery::Battery::default()),
        "claude" => Box::new(claude::Claude::new(options)),
        "system" => Box::new(system::System::new(options)),
        _ => return None,
    })
}

/// Implements `as_any` for a widget type.
#[macro_export]
macro_rules! any_widget {
    () => {
        fn as_any(&mut self) -> &mut dyn std::any::Any {
            self
        }
    };
}

/// The names of `[layout]` that the bar skips: neither a kind (themselves, or by their `type`) nor
/// one of `lua`. Each with the name it's most likely a typo of.
pub fn unknown(config: &crate::config::Config, lua: &[String]) -> Vec<(String, Option<String>)> {
    let kind = |name: &str| config.widgets.get(name).and_then(|w| w.get("type")).and_then(|t| t.as_str()).unwrap_or(name).to_string();
    let valid = |name: &str| lua.iter().any(|n| n == name) || KINDS.contains(&kind(name).as_str());
    let known: Vec<&str> = KINDS.iter().copied().chain(config.widgets.keys().map(String::as_str).filter(|n| valid(n))).chain(lua.iter().map(String::as_str)).collect();
    let l = &config.layout;
    l.left.iter().chain(&l.center).chain(&l.right).filter(|n| !valid(n)).map(|n| (n.clone(), closest(n, known.iter().copied()).map(String::from))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typos() {
        assert_eq!(closest("notification", KINDS.iter().copied()), Some("notifications"));
        assert_eq!(closest("Clock", KINDS.iter().copied()), Some("clock"));
        assert_eq!(closest("weather", KINDS.iter().copied()), None);
        let mut config = crate::config::Config::default();
        config.layout.right = vec!["clock".into(), "notification".into(), "clock_utc".into(), "weather".into()];
        config.widgets.insert("clock_utc".into(), toml::toml! { type = "clock" });
        assert_eq!(unknown(&config, &["weather".into()]), vec![("notification".into(), Some("notifications".into()))]);
    }
}
