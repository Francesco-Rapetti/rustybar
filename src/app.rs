//! The app state: config, widgets, one bar per screen and the popup. It lives on the main thread;
//! everything reaches it through `with`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAnimationContext, NSApplication, NSApplicationDidChangeScreenParametersNotification,
    NSEvent, NSEventMask, NSPopUpMenuWindowLevel, NSStatusWindowLevel,
    NSSystemColorsDidChangeNotification, NSWorkspace, NSWorkspaceDidWakeNotification,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSKeyValueObservingOptions, NSNotification, NSNotificationCenter, NSNumber, NSObjectNSKeyValueObserverRegistration,
    NSObjectProtocol, NSString, ns_string,
};
use objc2_quartz_core::{CAMediaTiming, CASpringAnimation};

use crate::cache::Cache;
use crate::config::{self, Config, Options};
use crate::display::{self, DisplayInfo};
use crate::fit::{self, Form, Room};
use crate::gfx::Weight;
use crate::runtime::{self, Timer};
use crate::surface::{Backdrop, Mouse, SurfaceId, Window};
use crate::theme::Theme;
use crate::ui::{Action, Align, HitKind, Kind, Layout, Node, Surface};
use crate::widgets::{self, Cx, Effect, Event, Rx, Widget};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Center,
    Right,
}

pub struct Slot {
    pub name: String,
    pub side: Side,
    pub widget: Box<dyn Widget>,
    pub timers: Vec<Timer>,
    /// A panic or a Lua error: the widget shows it instead of itself.
    pub failed: Option<String>,
}

pub struct Bar {
    pub display: DisplayInfo,
    pub window: Window,
    pub surface: Surface,
    /// The frame of each widget's pill, in bar coordinates.
    pub items: HashMap<String, CGRect>,
    /// How far the sides gave way at the last render, for the next (see `fit`).
    pub fit: Vec<usize>,
    /// The form each widget took at the last render.
    pub forms: HashMap<String, Form>,
    /// On a screen with a notch, where the center group went at the last render: beside the left
    /// or the right side, in its own form or compact; none when it had no room.
    pub center: Option<(Side, Form)>,
    /// The room the center group had beside it at the last render (see `spare`).
    pub spare: f64,
    /// What it drew at the last render, for a peek to start from the pill as it is.
    tree: Node,
    /// The pill hidden under a peek, as it would show.
    peek_pill: Option<Node>,
}

pub struct Popup {
    pub widget: String,
    pub display: u32,
    pub window: Window,
    pub surface: Surface,
    pub anchor: CGRect,
    pub side: Side,
}

/// A widget's pill grown down under the mouse, with what its `peek` shows: a window of its own
/// over the pill, at the popups' level, that starts as the pill and grows, while the pill in the
/// bar hides under it. Going back, it shrinks to the pill, the pill in the bar shows again under
/// it, and then it goes.
pub struct Peek {
    pub widget: String,
    pub display: u32,
    window: Window,
    surface: Surface,
    /// Grown, or growing; false while it goes back.
    open: bool,
    /// The pill in the bar is hidden under it (from the next render of the bar, for a new one);
    /// false once it's handed back to the pill, just before it goes.
    hides_pill: bool,
    /// Its window, at the size it has grown (Cocoa coordinates).
    frame: CGRect,
    /// Counts its openings and closings: what was set up for an older one doesn't apply.
    serial: u64,
    /// The mouse is out of it, and it goes back in a moment unless the mouse comes back.
    leaving: bool,
    /// Its glass has the grown size, from before it grows: the glass takes a new size a few
    /// frames after the layers, so the card grows inside it instead of ahead of it. Going back,
    /// it shrinks behind the card.
    grown_glass: bool,
}

/// How long the mouse rests on a pill before it grows, so that one crossed on the way somewhere
/// else stays as it is; and how long it may be out of the grown one before it goes back.
const PEEK_DELAY: f64 = 0.1;
const PEEK_LEAVE: f64 = 0.2;
/// The time the layers take to move and resize (see `Surface::render`).
const PEEK_ANIMATION: f64 = 0.32;

pub struct App {
    pub mtm: MainThreadMarker,
    pub config: Config,
    pub theme: Theme,
    /// A config or script error, shown at the start of the bar until fixed.
    pub error: Option<String>,
    pub slots: Vec<Slot>,
    pub bars: Vec<Bar>,
    pub popup: Option<Popup>,
    pub peek: Option<Peek>,
    /// The pill under the mouse in a bar: its display and its widget.
    hovered_pill: Option<(u32, String)>,
    /// Counts the mouse's comings and goings on the pills: a pending grow for an older one
    /// doesn't happen.
    peek_ticket: u64,
    /// The same for its goings out of the peek and back in: a pending shrink doesn't happen.
    leave_ticket: u64,
    /// The next render of the bars isn't animated: a pill hides under a peek, or shows again
    /// under one going away, at once.
    instant: bool,
    pub cache: Cache,
    pub lua: Option<crate::lua::Host>,
    generation: u64,
    dirty: bool,
    flush_queued: bool,
    pressed: Option<(SurfaceId, String)>,
    edge_requested: Option<String>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    /// Watches the app's appearance, which follows macOS between light and dark.
    appearance: Option<Retained<AppearanceObserver>>,
    monitor: Option<Retained<AnyObject>>,
    watcher: Option<notify::RecommendedWatcher>,
    closing: Vec<Window>,
    started: Instant,
    first_frame_logged: bool,
}

static GEN: AtomicU64 = AtomicU64::new(1);

define_class!(
    /// Told by key-value observing when the app's appearance changes: macOS went light or dark.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    pub struct AppearanceObserver;

    impl AppearanceObserver {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(&self, _key: Option<&NSString>, _object: Option<&AnyObject>, _change: Option<&AnyObject>, _context: *mut std::ffi::c_void) {
            with(|app| app.retheme());
        }
    }
);

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

pub fn current_gen() -> u64 {
    GEN.load(Ordering::Relaxed)
}

/// Runs `f` with the app. Called from inside another `with` (an AppKit callback that fired while
/// the app was busy), it runs right after instead. A panic is logged, not fatal.
pub fn with(f: impl FnOnce(&mut App) + 'static) {
    APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => {
            let Some(app) = guard.as_mut() else { return };
            if catch_unwind(AssertUnwindSafe(|| f(app))).is_err() {
                crate::log("panic in a handler");
            }
            if app.dirty {
                app.queue_flush();
            }
        }
        Err(_) => runtime::after(0.0, f),
    });
}

pub fn mouse(id: SurfaceId, kind: Mouse, p: CGPoint) {
    with(move |app| app.on_mouse(id, kind, p));
}

pub fn start(mtm: MainThreadMarker) {
    let started = Instant::now();
    let (config, error) = match config::load() {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(e)),
    };
    let theme = Theme::from_config(&config.theme);
    let app = App {
        mtm,
        theme,
        config,
        error,
        slots: Vec::new(),
        bars: Vec::new(),
        popup: None,
        peek: None,
        hovered_pill: None,
        peek_ticket: 0,
        leave_ticket: 0,
        instant: false,
        cache: Cache::load(),
        lua: None,
        generation: current_gen(),
        dirty: true,
        flush_queued: false,
        pressed: None,
        edge_requested: None,
        observers: Vec::new(),
        appearance: None,
        monitor: None,
        watcher: None,
        closing: Vec::new(),
        started,
        first_frame_logged: false,
    };
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    with(|app| {
        // The bars first, drawn from the cache, so they show before any widget starts
        app.reconcile_displays();
        app.render();
        app.create_widgets();
        app.observe();
        app.watch_config();
        crate::ipc::serve();
    });
}

impl App {
    fn animations(&self) -> bool {
        self.theme.animations && !NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }

    fn create_widgets(&mut self) {
        self.slots.clear();
        let lua = crate::lua::Host::load(&self.config, &self.theme);
        let (host, lua_widgets, lua_error) = match lua {
            Ok((host, widgets)) => (Some(host), widgets, None),
            Err(e) => (None, Vec::new(), Some(e)),
        };
        self.lua = host;
        crate::lua::set_current(self.lua.clone());
        if let Some(e) = lua_error {
            self.error = Some(e);
        }
        let layout = self.config.layout.clone();
        for (side, names) in [(Side::Left, &layout.left), (Side::Center, &layout.center), (Side::Right, &layout.right)] {
            for name in names {
                let options = Options(self.config.widgets.get(name).cloned().unwrap_or_default());
                let kind = options.str("type").unwrap_or(name).to_string();
                let widget: Option<Box<dyn Widget>> = if lua_widgets.contains(name) {
                    Some(Box::new(crate::lua::LuaWidget::new(name)))
                } else {
                    widgets::create(&kind, &options)
                };
                match widget {
                    Some(widget) => self.slots.push(Slot {
                        name: name.clone(),
                        side,
                        widget,
                        timers: Vec::new(),
                        failed: None,
                    }),
                    None => {
                        let hint = widgets::closest(name, widgets::KINDS.iter().copied().chain(lua_widgets.iter().map(String::as_str)));
                        crate::log(&format!("unknown widget {name} (type {kind}){}", hint.map(|h| format!(": did you mean {h}?")).unwrap_or_default()));
                    }
                }
            }
        }
        let names: Vec<String> = self.slots.iter().map(|s| s.name.clone()).collect();
        for name in names {
            self.with_slot(&name, |w, cx| w.start(cx));
        }
        self.dirty = true;
    }

    /// Re-reads the config and starts the widgets again. The bars stay; the popup closes.
    pub fn reload(&mut self) {
        match config::load() {
            Ok(c) => {
                self.config = c;
                self.error = None;
            }
            Err(e) => {
                self.error = Some(e);
                self.dirty = true;
                return;
            }
        }
        self.theme = Theme::from_config(&self.config.theme);
        self.close_popup();
        self.drop_peek();
        self.generation = GEN.fetch_add(1, Ordering::Relaxed) + 1;
        self.slots.clear();
        crate::gfx::trim_cache(0);
        for bar in &mut self.bars {
            bar.surface.highlight = self.theme.highlight;
            bar.fit.clear();
        }
        self.reconcile_displays();
        self.create_widgets();
        crate::log("reloaded");
    }

    /// The theme again, with the widgets running: macOS changed between light and dark, or its
    /// accent, which the macOS palette follows.
    fn retheme(&mut self) {
        let theme = Theme::from_config(&self.config.theme);
        if theme.palette == self.theme.palette {
            return;
        }
        self.theme = theme;
        for bar in &mut self.bars {
            bar.surface.highlight = self.theme.highlight;
        }
        if let Some(popup) = &mut self.popup {
            popup.surface.highlight = self.theme.highlight;
            popup.surface.glass_fill = Some(self.theme.surface);
        }
        if let Some(peek) = &mut self.peek {
            peek.surface.highlight = self.theme.highlight;
        }
        if let Some(lua) = &self.lua {
            lua.retheme(&self.theme);
        }
        self.dirty = true;
    }

    // ---- widgets -------------------------------------------------------------------------------

    pub fn with_widget<W: Widget>(&mut self, name: &str, generation: u64, f: impl FnOnce(&mut W, &mut Cx)) {
        if generation != self.generation {
            return;
        }
        self.with_slot(name, |w, cx| {
            if let Some(w) = w.as_any().downcast_mut::<W>() {
                f(w, cx)
            }
        });
    }

    pub fn with_slot(&mut self, name: &str, f: impl FnOnce(&mut dyn Widget, &mut Cx)) {
        let Some(i) = self.slots.iter().position(|s| s.name == name) else { return };
        let popup_open = self.popup.as_ref().is_some_and(|p| p.widget == name);
        let peek_open = self.peek.as_ref().is_some_and(|p| p.widget == name && p.open);
        let slot = &mut self.slots[i];
        let mut cx = Cx {
            name: &slot.name,
            generation: self.generation,
            popup_open,
            peek_open,
            timers: &mut slot.timers,
            effects: Vec::new(),
        };
        let widget = &mut *slot.widget;
        let result = catch_unwind(AssertUnwindSafe(|| f(widget, &mut cx)));
        let effects = std::mem::take(&mut cx.effects);
        let name = name.to_string();
        if result.is_err() {
            slot.failed = Some(format!("crashed: see {}", crate::service::log_path().display()));
            crate::log(&format!("widget {name} panicked"));
            self.dirty = true;
        }
        self.apply(&name, effects);
    }

    fn apply(&mut self, widget: &str, effects: Vec<Effect>) {
        for e in effects {
            match e {
                Effect::Redraw => self.dirty = true,
                Effect::ClosePopup => self.close_popup(),
                Effect::TogglePopup => {
                    let display = self.mouse_display();
                    self.toggle_popup(widget, display);
                }
            }
        }
    }

    pub fn broadcast(&mut self, event: &Event) {
        let names: Vec<String> = self.slots.iter().map(|s| s.name.clone()).collect();
        for name in names {
            let ev = event.clone();
            self.with_slot(&name, move |w, cx| w.event(&ev, cx));
        }
    }

    pub fn dispatch(&mut self, action: Action, value: Option<f64>, display: Option<u32>) {
        if std::env::var_os("RUSTYBAR_DEBUG").is_some() {
            crate::log(&format!("action {action:?} {value:?}"));
        }
        match action {
            Action::Widget { widget, name } => {
                self.with_slot(&widget, |w, cx| w.action(&name, value, cx));
                self.dirty = true;
            }
            Action::Lua { widget, index } => {
                if let Some(lua) = &self.lua
                    && let Err(e) = lua.call_action(&widget, index, value) {
                        self.set_failed(&widget, e);
                    }
                self.dirty = true;
            }
            Action::Popup { widget } => {
                let display = display.unwrap_or_else(|| self.mouse_display());
                self.toggle_popup(&widget, display);
            }
            Action::Shell(cmd) => runtime::shell(&cmd),
            Action::Open(target) => runtime::open(&target),
        }
    }

    pub fn set_failed(&mut self, widget: &str, error: String) {
        crate::log(&format!("{widget}: {error}"));
        if let Some(s) = self.slots.iter_mut().find(|s| s.name == widget) {
            s.failed = Some(error);
        }
        self.dirty = true;
    }

    // ---- rendering -----------------------------------------------------------------------------

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn queue_flush(&mut self) {
        if self.flush_queued {
            return;
        }
        self.flush_queued = true;
        runtime::after(0.0, |app| {
            app.flush_queued = false;
            if app.dirty {
                app.render();
            }
        });
    }

    /// The form the config gives a widget on `d`, whether or not there's room.
    fn config_form(&self, name: &str, d: &DisplayInfo) -> Form {
        let displays = &self.config.displays;
        if d.notch.is_some() && displays.hide_on_notch.iter().any(|n| n == name) {
            Form::Hidden
        } else if (d.notch.is_some() && displays.compact_on_notch) || displays.always_compact.iter().any(|n| n == name) {
            Form::Compact
        } else {
            Form::Full
        }
    }

    /// The node of a widget in the bar of `d`: its own, its last known one (dimmed) until it's
    /// ready, a skeleton the very first time, or its error.
    fn item(&mut self, i: usize, d: &DisplayInfo, form: Form) -> Option<Node> {
        self.item_wider(i, d, form, 0.0)
    }

    /// `item`, `spare` points wider at most (see `Rx::spare`). The cache keeps only the node as
    /// wide as its form, the one the bar measures at start.
    fn item_wider(&mut self, i: usize, d: &DisplayInfo, form: Form, spare: f64) -> Option<Node> {
        let popup_open = self.popup.as_ref().is_some_and(|p| p.widget == self.slots[i].name);
        let slot = &self.slots[i];
        let mut rx = Rx::new(&slot.name, &self.theme, d, form, popup_open);
        rx.spare = spare;
        if let Some(err) = &slot.failed {
            return Some(error_pill(&rx, err));
        }
        let variant = match form {
            Form::Slim => "slim",
            Form::Compact => "compact",
            _ => "full",
        };
        if !slot.widget.ready() {
            return Some(match self.cache.get(&slot.name, variant) {
                Some(node) => node.clone().opacity(0.55),
                None => skeleton_pill(&rx),
            });
        }
        let widget = &slot.widget;
        match catch_unwind(AssertUnwindSafe(|| widget.bar(&rx))) {
            Ok(Some(mut node)) => {
                node.key = Some(slot.name.clone());
                let name = slot.name.clone();
                if spare == 0.0 {
                    self.cache.put(&name, variant, &node);
                }
                Some(node)
            }
            Ok(None) => None,
            Err(_) => {
                let name = self.slots[i].name.clone();
                self.set_failed(&name, "crashed while drawing".into());
                None
            }
        }
    }

    /// The bar of `self.bars[b]`: the left side at the left edge, the right one at the right edge,
    /// the center one in the middle, with the widgets of the left and right sides in the form that
    /// keeps them apart (see `fit`). Those that aren't in the bar are in the popup of a `⋯` pill, at
    /// the inner end of their side. On a screen with a notch the middle is the notch's: the center
    /// group goes beside it, at the inner end of the left side if that has room for it, else of the
    /// right one, else in its compact form the same way, else nowhere (see `place_center`). The
    /// room left beside the center group, there or in the middle, goes to the text it had to cut
    /// (see `widen`).
    fn bar_tree(&mut self, b: usize) -> Node {
        let d = self.bars[b].display.clone();
        let height = self.config.bar.height;
        let width = d.frame.size.width;
        let margin = self.config.bar.margin;
        let gap = self.theme.item_gap;
        let layout = Layout { scale: d.scale };
        let mut forms: HashMap<String, Form> = HashMap::new();

        let centered: Vec<(usize, Form)> = (0..self.slots.len())
            .filter(|&i| self.slots[i].side == Side::Center)
            .map(|i| (i, self.config_form(&self.slots[i].name, &d)))
            .filter(|(_, form)| *form != Form::Hidden)
            .collect();
        let draw_center = |app: &mut App, compact: bool| -> Vec<Node> {
            centered.iter().filter_map(|&(i, form)| app.item(i, &d, if compact { Form::Compact } else { form })).collect()
        };
        let mut center = draw_center(self, false);
        let center_w = center.iter().map(|n| layout.intrinsic(n, None).0).sum::<f64>() + gap * center.len().saturating_sub(1) as f64;

        // The room of the sides: up to the notch, or to the center group, with a gap; or all the
        // bar between the margins when there's nothing in the middle
        let middle = match d.notch {
            Some(notch) => Some(notch),
            None if center_w > 0.0 => {
                let x = ((width - center_w) / 2.0).round();
                Some((x, x + center_w))
            }
            None => None,
        };
        let room = match middle {
            Some((l, r)) => Room::Split { left: l - margin - gap, right: width - margin - r - gap },
            None => Room::Shared(width - 2.0 * margin - gap),
        };

        let mut slots = Vec::new();
        let mut items = Vec::new();
        for left in [true, false] {
            let side = if left { Side::Left } else { Side::Right };
            let of_side: Vec<usize> = (0..self.slots.len()).filter(|&i| self.slots[i].side == side).collect();
            for (k, &i) in of_side.iter().enumerate() {
                let rank = if left { k } else { of_side.len() - 1 - k };
                let slim = self.config.displays.slim_first.iter().position(|n| *n == self.slots[i].name).filter(|_| self.slots[i].failed.is_none());
                items.push(fit::Item { left, rank, form: self.config_form(&self.slots[i].name, &d), slim });
                slots.push(i);
            }
        }
        let error = self.error.clone().map(|e| config_error_pill(&self.theme, &e));
        let error_w = error.as_ref().map(|n| layout.intrinsic(n, None).0);
        let more = [Side::Left, Side::Right].map(|side| self.more_pill(side, &d));
        let more_w = layout.intrinsic(&more[0], None).0;

        // Each widget is drawn in a form only when the fit needs to know how wide it is then
        let mut drawn = vec![[None, None, None]; slots.len()];
        let mut draw = |app: &mut App, k: usize, form: Form| -> Option<(f64, Node)> {
            drawn[k][form as usize]
                .get_or_insert_with(|| app.item(slots[k], &d, form).map(|n| (layout.intrinsic(&n, None).0, n)))
                .clone()
        };
        let prev = std::mem::take(&mut self.bars[b].fit);
        let (levels, chosen) = fit::fit(&items, room, &prev, |forms| {
            let mut sides = [(0.0, 0usize), (0.0, 0usize)];
            if let Some(w) = error_w {
                sides[0] = (w, 1);
            }
            let mut hidden = [false, false];
            for (k, form) in forms.iter().enumerate() {
                let s = !items[k].left as usize;
                if *form == Form::Hidden {
                    hidden[s] = true;
                } else if let Some((w, _)) = draw(self, k, *form) {
                    sides[s].0 += w;
                    sides[s].1 += 1;
                }
            }
            for s in 0..2 {
                if hidden[s] {
                    sides[s].0 += more_w;
                    sides[s].1 += 1;
                }
            }
            let side = |(w, n): (f64, usize)| w + gap * n.saturating_sub(1) as f64;
            (side(sides[0]), side(sides[1]))
        });

        let mut groups = [Vec::new(), Vec::new()];
        groups[0].extend(error);
        for (k, form) in chosen.iter().enumerate() {
            forms.insert(self.slots[slots[k]].name.clone(), *form);
            if *form != Form::Hidden
                && let Some((_, node)) = draw(self, k, *form)
            {
                groups[!items[k].left as usize].push(node);
            }
        }
        let [more_left, more_right] = more;
        if (0..items.len()).any(|k| items[k].left && chosen[k] == Form::Hidden) {
            groups[0].push(more_left);
        }
        if (0..items.len()).any(|k| !items[k].left && chosen[k] == Form::Hidden) {
            groups[1].insert(0, more_right);
        }

        // Beside the notch, the center group takes what room the sides leave. Then, there or in
        // the middle, what's left beside it goes to the text it had to cut (see `Rx::spare`)
        let mut center_form = |name: &str, form: Form| forms.insert(name.to_string(), form);
        let width_of = |nodes: &[Node]| nodes.iter().map(|n| layout.intrinsic(n, None).0).sum::<f64>() + gap * nodes.len().saturating_sub(1) as f64;
        let used = groups.each_ref().map(|g| width_of(g));
        if d.notch.is_some() && !center.is_empty() {
            let Room::Split { left, right } = room else { unreachable!("a notch splits the bar") };
            let prev = self.bars[b].center;
            // The compact form is drawn only when the group's own doesn't fit on either side
            let mut compact = None;
            let mut placed = place_center(used, [left, right], center_w, None, gap, prev);
            if placed.is_none() && !centered.iter().all(|(_, f)| *f == Form::Compact) {
                let nodes = draw_center(self, true);
                placed = place_center(used, [left, right], center_w, Some(width_of(&nodes)), gap, prev);
                compact = Some(nodes);
            }
            self.bars[b].center = placed;
            let nodes = match placed {
                Some((side, form)) => {
                    let nodes = if form == Form::Compact { compact.unwrap_or_default() } else { std::mem::take(&mut center) };
                    let s = (side == Side::Right) as usize;
                    let spare = [left, right][s] - used[s] - if used[s] > 0.0 { gap } else { 0.0 } - width_of(&nodes);
                    self.widen(b, &centered, nodes, form == Form::Compact, spare)
                }
                None => Vec::new(),
            };
            for &(i, form) in &centered {
                let form = match placed {
                    Some((_, Form::Compact)) => Form::Compact,
                    Some(_) => form,
                    None => Form::Hidden,
                };
                center_form(&self.slots[i].name, form);
            }
            center.clear();
            match placed {
                Some((Side::Left, _)) => groups[0].extend(nodes),
                Some(_) => drop(groups[1].splice(0..0, nodes)),
                None => {}
            }
        } else {
            self.bars[b].center = None;
            for &(i, form) in &centered {
                center_form(&self.slots[i].name, form);
            }
            // In the middle: on both sides as far as the wider side lets it
            if !center.is_empty() {
                let spare = width - 2.0 * (margin + used[0].max(used[1]) + gap) - center_w;
                center = self.widen(b, &centered, center, false, spare);
            }
        }
        // A pill grown under the mouse is in its own window: here it keeps its place, unseen
        self.bars[b].peek_pill = None;
        if let Some(peek) = self.peek.as_ref().filter(|p| p.hides_pill && p.display == d.id) {
            for node in groups.iter_mut().flatten().chain(center.iter_mut()) {
                if node.key.as_deref() == Some(peek.widget.as_str()) {
                    self.bars[b].peek_pill = Some(node.clone());
                    node.style.opacity = 0.0;
                }
            }
        }
        let center = Node::row(center).key("center");
        self.bars[b].fit = levels;
        self.bars[b].forms = forms;

        let [left, right] = groups;
        let mut root = Node::default().size(width, height).key("bar");
        for (side, group) in [(Side::Left, Node::row(left).key("left")), (Side::Center, center), (Side::Right, Node::row(right).key("right"))] {
            let group = group.gap(gap);
            let (w, h) = layout.intrinsic(&group, None);
            let x = match side {
                Side::Left => margin,
                Side::Center => ((width - w) / 2.0).round(),
                Side::Right => width - margin - w,
            };
            root.children.push(group.at(x, ((height - h) / 2.0).round()));
        }
        root
    }

    /// The center group of `self.bars[b]` drawn again with `spare` points more room, which each of
    /// its widgets takes in turn as far as it needs it (see `Rx::spare`). The room follows a side
    /// that grows at once, one that shrinks only past a few points: a pill beside the group that
    /// changes by a point or two (`system`'s digits) doesn't make it change too.
    fn widen(&mut self, b: usize, centered: &[(usize, Form)], nodes: Vec<Node>, compact: bool, spare: f64) -> Vec<Node> {
        let d = self.bars[b].display.clone();
        let layout = Layout { scale: d.scale };
        let mut spare = steady_spare(self.bars[b].spare, spare);
        self.bars[b].spare = spare;
        let mut wide = Vec::with_capacity(nodes.len());
        for node in nodes {
            let slot = centered.iter().find(|(i, _)| node.key.as_deref() == Some(self.slots[*i].name.as_str())).copied();
            let drawn = match slot {
                Some((i, form)) if spare >= 1.0 => self.item_wider(i, &d, if compact { Form::Compact } else { form }, spare),
                _ => None,
            };
            match drawn {
                Some(w) => {
                    spare -= (layout.intrinsic(&w, None).0 - layout.intrinsic(&node, None).0).max(0.0);
                    wide.push(w);
                }
                None => wide.push(node),
            }
        }
        wide
    }

    fn more_pill(&self, side: Side, d: &DisplayInfo) -> Node {
        let name = more_name(side);
        let popup_open = self.popup.as_ref().is_some_and(|p| p.widget == name);
        let rx = Rx::new(name, &self.theme, d, Form::Full, popup_open);
        rx.pill(vec![Node::symbol("ellipsis", self.theme.icon_size, Weight::Semibold, self.theme.text)]).click(rx.toggle_popup())
    }

    pub fn render(&mut self) {
        self.dirty = false;
        if let Some(lua) = &self.lua {
            lua.begin_frame();
        }
        let animate = self.animations();
        for b in 0..self.bars.len() {
            let d = self.bars[b].display.clone();
            let mut tree = self.bar_tree(b);
            tree.normalize();
            let laid = Layout { scale: d.scale }.run(&tree, Some(d.frame.size.width));
            let bar = &mut self.bars[b];
            bar.surface.animate = animate && self.first_frame_logged && !self.instant;
            bar.surface.motion = animate;
            bar.surface.render(&tree, &laid);
            bar.window.set_dark(self.theme.glass_dark);
            bar.window.set_glass(&bar.surface.glass, self.theme.clear_glass, bar.surface.animate);
            // Where each widget's pill is, for its popup
            bar.items.clear();
            for (group, gl) in tree.children.iter().zip(&laid.children) {
                for (item, il) in group.children.iter().zip(&gl.children) {
                    if let Some(k) = &item.key {
                        let r = CGRect::new(CGPoint::new(gl.x + il.x, gl.y + il.y), CGSize::new(il.w, il.h));
                        bar.items.insert(k.clone(), r);
                    }
                }
            }
            // The popups of the widgets in a `⋯` popup open from the `⋯` pill; a center one with
            // no room beside the notch is in none
            for slot in &self.slots {
                if slot.side != Side::Center
                    && bar.forms.get(&slot.name) == Some(&Form::Hidden)
                    && let Some(r) = bar.items.get(more_name(slot.side)).copied()
                {
                    bar.items.insert(slot.name.clone(), r);
                }
            }
            bar.tree = tree;
        }
        // A `⋯` popup goes when its widgets are back in the bar
        if !self.render_popup(animate) && self.popup.as_ref().is_some_and(|p| more_side(&p.widget).is_some()) {
            self.close_popup();
        }
        self.instant = false;
        // A peek goes with its pill, or with what it showed
        if self.peek.is_some() && !self.render_peek(animate) {
            self.drop_peek();
        }
        crate::gfx::trim_cache(256);
        if !self.first_frame_logged && !self.bars.is_empty() {
            self.first_frame_logged = true;
            crate::log(&format!("first frame in {} ms", self.started.elapsed().as_millis()));
        }
        self.cache.save_soon();
    }

    // ---- displays ------------------------------------------------------------------------------

    fn bar_level(&self) -> isize {
        // Notification Center's banners are at 21, just above the Dock's level (20; its constant
        // is deprecated): there the bar stays above every window and under the banners
        const DOCK_LEVEL: isize = 20;
        if self.config.bar.topmost { NSStatusWindowLevel } else { DOCK_LEVEL }
    }

    fn bar_frame(&self, d: &DisplayInfo) -> CGRect {
        let h = self.config.bar.height;
        let f = d.frame;
        CGRect::new(
            CGPoint::new(f.origin.x, f.origin.y + f.size.height - h - self.config.bar.offset),
            CGSize::new(f.size.width, h),
        )
    }

    /// Makes sure every screen has its bar, at the right place, and no bar is left without one.
    pub fn reconcile_displays(&mut self) -> bool {
        let displays = display::displays(self.mtm, self.config.displays.large_width);
        let mut changed = false;
        self.bars.retain(|b| {
            let keep = displays.iter().any(|d| d.id == b.display.id);
            changed |= !keep;
            keep
        });
        let level = self.bar_level();
        for d in displays {
            let frame = self.bar_frame(&d);
            match self.bars.iter_mut().find(|b| b.display.id == d.id) {
                Some(bar) => {
                    if bar.display != d || bar.window.panel.frame() != frame || !bar.window.panel.isVisible() {
                        bar.surface.set_scale(d.scale);
                        bar.window.set_frame(frame);
                        bar.window.panel.setLevel(level);
                        bar.window.show();
                        bar.display = d;
                        changed = true;
                    }
                }
                None => {
                    let mut surface = Surface::new(d.scale, self.theme.highlight);
                    surface.animate = false;
                    let window = Window::new(self.mtm, SurfaceId::Bar(d.id), frame, level, &surface.root, Backdrop::None, 0.0);
                    window.show();
                    self.bars.push(Bar { display: d, window, surface, items: HashMap::new(), fit: Vec::new(), forms: HashMap::new(), center: None, spare: 0.0, tree: Node::default(), peek_pill: None });
                    changed = true;
                }
            }
        }
        if changed {
            self.dirty = true;
        }
        changed
    }

    fn observe(&mut self) {
        let center = NSNotificationCenter::defaultCenter();
        let screens = RcBlock::new(|_: NonNull<NSNotification>| {
            // Screens come and go in bursts: settle first
            runtime::after(0.4, |app| app.screens_changed());
        });
        let o = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSApplicationDidChangeScreenParametersNotification),
                None,
                None,
                &screens,
            )
        };
        self.observers.push(o);

        let ws_center = NSWorkspace::sharedWorkspace().notificationCenter();
        let wake = RcBlock::new(|_: NonNull<NSNotification>| {
            with(|app| {
                app.broadcast(&Event::Wake);
                // After a wake the screens come back one at a time: check them for a while
                for i in 1..=10 {
                    runtime::after(2.0 * i as f64, |app| {
                        if app.reconcile_displays() {
                            app.broadcast(&Event::ScreensChanged);
                        }
                    });
                }
            });
        });
        let o = unsafe { ws_center.addObserverForName_object_queue_usingBlock(Some(NSWorkspaceDidWakeNotification), None, None, &wake) };
        self.observers.push(o);

        // The macOS palette follows light and dark mode, and the accent color
        let observer: Retained<AppearanceObserver> = unsafe { msg_send![AppearanceObserver::alloc(self.mtm), init] };
        unsafe {
            NSApplication::sharedApplication(self.mtm).addObserver_forKeyPath_options_context(
                &observer,
                ns_string!("effectiveAppearance"),
                NSKeyValueObservingOptions::New,
                std::ptr::null_mut(),
            )
        };
        self.appearance = Some(observer);
        let colors = RcBlock::new(|_: NonNull<NSNotification>| with(|app| app.retheme()));
        let o = unsafe { center.addObserverForName_object_queue_usingBlock(Some(NSSystemColorsDidChangeNotification), None, None, &colors) };
        self.observers.push(o);

        // A click anywhere else closes the popup
        let block = RcBlock::new(|_: NonNull<NSEvent>| with(|app| app.close_popup()));
        self.monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
            NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown,
            &block,
        );
    }

    fn screens_changed(&mut self) {
        self.close_popup();
        self.drop_peek();
        if self.reconcile_displays() {
            self.broadcast(&Event::ScreensChanged);
        }
    }

    fn watch_config(&mut self) {
        use notify::{RecursiveMode, Watcher};
        let pending = Rc::new(());
        let _ = pending;
        let watcher = notify::recommended_watcher(|res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            let relevant = ev.paths.iter().any(|p| {
                matches!(p.extension().and_then(|e| e.to_str()), Some("toml") | Some("lua"))
            });
            if relevant && !matches!(ev.kind, notify::EventKind::Access(_)) {
                runtime::post(|app| app.schedule_reload());
            }
        });
        match watcher {
            Ok(mut w) => {
                if w.watch(&config::config_dir(), RecursiveMode::Recursive).is_ok() {
                    self.watcher = Some(w);
                }
            }
            Err(e) => crate::log(&format!("can't watch the config: {e}")),
        }
    }

    fn schedule_reload(&mut self) {
        // Editors save in several steps: reload once, after the last
        static TICKET: AtomicU64 = AtomicU64::new(0);
        let ticket = TICKET.fetch_add(1, Ordering::Relaxed) + 1;
        runtime::after(0.3, move |app| {
            if TICKET.load(Ordering::Relaxed) == ticket {
                app.reload();
            }
        });
    }

    // ---- mouse ---------------------------------------------------------------------------------

    fn surface_mut(&mut self, id: SurfaceId) -> Option<&mut Surface> {
        match id {
            SurfaceId::Bar(d) => self.bars.iter_mut().find(|b| b.display.id == d).map(|b| &mut b.surface),
            SurfaceId::Popup => self.popup.as_mut().map(|p| &mut p.surface),
            SurfaceId::Peek => self.peek.as_mut().map(|p| &mut p.surface),
        }
    }

    fn mouse_display(&self) -> u32 {
        let p = NSEvent::mouseLocation();
        self.bars
            .iter()
            .find(|b| {
                let f = b.display.frame;
                p.x >= f.origin.x && p.x < f.origin.x + f.size.width && p.y >= f.origin.y && p.y <= f.origin.y + f.size.height
            })
            .or(self.bars.first())
            .map(|b| b.display.id)
            .unwrap_or(0)
    }

    fn on_mouse(&mut self, id: SurfaceId, kind: Mouse, p: CGPoint) {
        if std::env::var_os("RUSTYBAR_DEBUG").is_some() && !matches!(kind, Mouse::Moved) {
            crate::log(&format!("mouse {kind:?} on {id:?} at {:.0},{:.0}", p.x, p.y));
        }
        let display = match id {
            SurfaceId::Bar(d) => Some(d),
            SurfaceId::Popup => self.popup.as_ref().map(|p| p.display),
            SurfaceId::Peek => self.peek.as_ref().map(|p| p.display),
        };
        match (id, kind) {
            (SurfaceId::Bar(d), Mouse::Moved) => self.bar_hover(d, Some(p)),
            (SurfaceId::Bar(d), Mouse::Exited) => self.bar_hover(d, None),
            // Back in it in time: it stays
            (SurfaceId::Peek, Mouse::Moved) => {
                self.leave_ticket += 1;
                if let Some(peek) = &mut self.peek {
                    peek.leaving = false;
                }
            }
            (SurfaceId::Peek, Mouse::Exited) => self.left_peek(),
            _ => {}
        }
        let Some(surface) = self.surface_mut(id) else { return };
        // What a press lands on, topmost first, and whether it has an action
        if std::env::var_os("RUSTYBAR_DEBUG").is_some() && matches!(kind, Mouse::Down) {
            for h in surface.hits.iter().rev().filter(|h| crate::ui::contains(h.rect, p)) {
                let action = if h.action.is_some() { "" } else { ", no action" };
                crate::log(&format!("  on {:?} {}{action}", h.kind, h.path));
            }
        }
        match kind {
            Mouse::Moved => {
                if surface.dragging.is_none() {
                    // One that asks for no highlight is as if the mouse were on nothing
                    let hit = surface.hit(p, &[HitKind::Click]).and_then(|h| Some((h.path.clone(), h.hover?)));
                    match hit {
                        Some((path, hover)) => surface.set_hover(Some(path), Some(hover)),
                        None => surface.set_hover(None, None),
                    }
                    let slider = surface.hit(p, &[HitKind::Slider]).map(|h| h.path.clone());
                    surface.set_slider_hover(slider);
                }
            }
            Mouse::Exited => {
                surface.set_hover(None, None);
                surface.set_slider_hover(None);
            }
            Mouse::Down => {
                if let Some(h) = surface.hit(p, &[HitKind::Slider]).cloned() {
                    let v = crate::ui::slider_value(h.rect, p.x);
                    surface.drag_slider(&h.path, v);
                } else if let Some(h) = surface.hit(p, &[HitKind::Click]).cloned() {
                    surface.press(&h.path);
                    self.pressed = Some((id, h.path));
                } else if matches!(id, SurfaceId::Bar(_)) {
                    self.close_popup();
                }
            }
            Mouse::Dragged => {
                if let Some((path, _)) = surface.dragging.clone()
                    && let Some(h) = surface.hits.iter().find(|h| h.path == path).cloned() {
                        let v = crate::ui::slider_value(h.rect, p.x);
                        surface.drag_slider(&path, v);
                    }
            }
            Mouse::Up => {
                if let Some((path, v)) = surface.dragging.take() {
                    let action = surface.hits.iter().find(|h| h.path == path).and_then(|h| h.action.clone());
                    self.dirty = true;
                    if let Some(a) = action {
                        self.dispatch(a, Some(v), display);
                    }
                    return;
                }
                let pressed = self.pressed.take();
                let Some(surface) = self.surface_mut(id) else { return };
                let hit = surface.hit(p, &[HitKind::Click]).cloned();
                if let (Some((pid, ppath)), Some(h)) = (pressed, hit)
                    && pid == id && ppath == h.path
                        && let Some(a) = h.action {
                            self.dispatch(a, None, display);
                        }
            }
            Mouse::RightDown => {}
            Mouse::Scroll { dy, ended } => {
                let Some(h) = surface.hit(p, &[HitKind::Scroll]).cloned() else { return };
                let edge = surface.scroll_by(&h.path, dy, true);
                if ended {
                    surface.settle(&h.path);
                    self.dirty = true;
                }
                match (edge, &h.action) {
                    (Some(which), Some(Action::Widget { widget, name: _ })) => {
                        let key = format!("{}:{which}", h.path);
                        if self.edge_requested.as_deref() != Some(&key) {
                            self.edge_requested = Some(key);
                            let (widget, which) = (widget.clone(), which.to_string());
                            self.with_slot(&widget, |w, cx| w.action(&which, None, cx));
                            self.dirty = true;
                        }
                    }
                    (None, _) => self.edge_requested = None,
                    _ => {}
                }
            }
        }
    }

    // ---- popup ---------------------------------------------------------------------------------

    fn toggle_popup(&mut self, widget: &str, display: u32) {
        if self.popup.as_ref().is_some_and(|p| p.widget == widget) {
            self.close_popup();
        } else {
            self.open_popup(widget, display);
        }
    }

    pub fn open_popup(&mut self, widget: &str, display: u32) {
        self.close_popup();
        // The popup has all the peek had: the pill goes back, under it
        if self.peek.as_ref().is_some_and(|p| p.widget == widget) {
            self.collapse_peek();
        }
        let side = match more_side(widget) {
            Some(side) => side,
            None => match self.slots.iter().find(|s| s.name == widget) {
                Some(slot) => slot.side,
                None => return,
            },
        };
        let Some(bar) = self.bars.iter().find(|b| b.display.id == display).or(self.bars.first()) else { return };
        let Some(item) = bar.items.get(widget).copied() else { return };
        let d = bar.display.clone();
        let bar_frame = bar.window.panel.frame();
        // The pill in screen coordinates (Cocoa: y up)
        let anchor = CGRect::new(
            CGPoint::new(bar_frame.origin.x + item.origin.x, bar_frame.origin.y + bar_frame.size.height - item.origin.y - item.size.height),
            item.size,
        );
        let t = &self.theme;
        let backdrop = if t.glass {
            Backdrop::Glass
        } else if t.popup_blur {
            Backdrop::Blur
        } else {
            Backdrop::None
        };
        let mut surface = Surface::new(d.scale, t.highlight);
        surface.animate = false;
        // The pills of a `⋯` popup are on its glass already
        surface.glass_fill = Some(t.surface);
        let frame = CGRect::new(anchor.origin, CGSize::new(10.0, 10.0));
        let level = NSPopUpMenuWindowLevel;
        let mut window = Window::new(self.mtm, SurfaceId::Popup, frame, level, &surface.root, backdrop, t.popup_radius);
        window.set_dark(t.glass_dark);
        window.panel.setAlphaValue(0.0);
        self.popup = Some(Popup { widget: widget.to_string(), display: d.id, window, surface, anchor, side });
        let name = widget.to_string();
        self.with_slot(&name, |w, cx| w.event(&Event::PopupOpened, cx));
        if !self.render_popup(false) {
            self.popup = None;
            return;
        }
        let Some(popup) = &self.popup else { return };
        popup.window.show();
        let animate = self.animations();
        let panel = popup.window.panel.clone();
        let fade = RcBlock::new(move |ctx: NonNull<NSAnimationContext>| {
            unsafe { ctx.as_ref() }.setDuration(if animate { 0.16 } else { 0.0 });
            panel.animator().setAlphaValue(1.0);
        });
        NSAnimationContext::runAnimationGroup(&fade);
        if animate {
            let root = &popup.surface.root;
            for (key, from) in [("transform.scale", 0.96), ("transform.translation.y", -8.0)] {
                let anim = CASpringAnimation::animationWithKeyPath(Some(&objc2_foundation::NSString::from_str(key)));
                anim.setStiffness(380.0);
                anim.setDamping(26.0);
                unsafe {
                    anim.setFromValue(Some(&NSNumber::new_f64(from)));
                    anim.setToValue(Some(&NSNumber::new_f64(if key == "transform.scale" { 1.0 } else { 0.0 })));
                }
                anim.setDuration(anim.settlingDuration());
                root.addAnimation_forKey(&anim, Some(&objc2_foundation::NSString::from_str(key)));
            }
        }
        popup.surface.root.setOpacity(1.0);
        self.dirty = true;
    }

    /// Draws the popup and fits its window to it. False if the widget has no popup, or a `⋯` pill
    /// no widgets.
    fn render_popup(&mut self, animate: bool) -> bool {
        let Some(popup) = &self.popup else { return false };
        let (name, anchor, side) = (popup.widget.clone(), popup.anchor, popup.side);
        let Some(bar) = self.bars.iter().find(|b| b.display.id == popup.display) else { return false };
        let d = bar.display.clone();
        let content = if more_side(&name).is_some() {
            // The widgets of this side that aren't in the bar, in their full form: there's room
            let hidden: Vec<usize> = (0..self.slots.len())
                .filter(|&i| self.slots[i].side == side && bar.forms.get(&self.slots[i].name) == Some(&Form::Hidden))
                .collect();
            let items: Vec<Node> = hidden.into_iter().filter_map(|i| self.item(i, &d, Form::Full)).collect();
            let align = if side == Side::Left { Align::Start } else { Align::End };
            (!items.is_empty()).then(|| Node::column(items).padding(8.0, 8.0, 8.0, 8.0).gap(self.theme.item_gap).align(align))
        } else {
            let Some(slot) = self.slots.iter().find(|s| s.name == name) else { return false };
            // As the widget is in the bar
            let form = bar.forms.get(&name).copied().unwrap_or_else(|| self.config_form(&name, &d));
            let rx = Rx::new(&slot.name, &self.theme, &d, form, true);
            match &slot.failed {
                Some(err) => Some(rx.popup(260.0, vec![rx.section("Error", None), rx.detail(err)])),
                None => {
                    let widget = &slot.widget;
                    catch_unwind(AssertUnwindSafe(|| widget.popup(&rx))).ok().flatten()
                }
            }
        };
        let Some(content) = content else { return false };
        let t = &self.theme;
        let alpha = if t.popup_blur { t.popup_opacity } else { 1.0 };
        let mut tree = Node::column(vec![content]).key("popup").radius(t.popup_radius);
        if !t.glass {
            tree = tree.background(t.base.faded(alpha)).border(t.popup_border, t.surface);
        } else if let Some(veil) = t.popup_veil() {
            tree = tree.background(veil);
        }
        tree.normalize();
        let layout = Layout { scale: d.scale };
        let (mut w, mut h) = layout.intrinsic(&tree, None);
        // Never taller than the screen below the bar
        let max_h = (anchor.origin.y - d.frame.origin.y - 8.0).max(100.0);
        h = h.min(max_h);
        w = w.ceil();
        let laid = layout.place(&tree, 0.0, 0.0, w, h);
        let gap = self.config.bar.popup_gap;
        let margin = self.config.bar.margin;
        let a = anchor;
        let x = match side {
            Side::Left => a.origin.x,
            Side::Right => a.origin.x + a.size.width - w,
            Side::Center => a.origin.x + (a.size.width - w) / 2.0,
        };
        let min_x = d.frame.origin.x + margin;
        let max_x = d.frame.origin.x + d.frame.size.width - margin - w;
        let x = x.clamp(min_x, max_x.max(min_x));
        let top = a.origin.y - gap;
        let frame = CGRect::new(CGPoint::new(x, top - h), CGSize::new(w, h));
        let popup = self.popup.as_mut().unwrap();
        popup.window.set_frame(frame);
        popup.window.set_corner_radius(self.theme.popup_radius);
        popup.surface.animate = animate;
        popup.surface.motion = animate;
        popup.surface.render(&tree, &laid);
        true
    }

    pub fn close_popup(&mut self) {
        let Some(popup) = self.popup.take() else { return };
        let name = popup.widget.clone();
        self.with_slot(&name, |w, cx| w.event(&Event::PopupClosed, cx));
        let animate = self.animations();
        let panel = popup.window.panel.clone();
        let fade = RcBlock::new(move |ctx: NonNull<NSAnimationContext>| {
            unsafe { ctx.as_ref() }.setDuration(if animate { 0.12 } else { 0.0 });
            panel.animator().setAlphaValue(0.0);
        });
        NSAnimationContext::runAnimationGroup(&fade);
        self.closing.push(popup.window);
        runtime::after(0.2, |app| {
            if !app.closing.is_empty() {
                app.closing.remove(0);
            }
            // What the popup drew is off screen now: its bitmaps go, and so do the pages malloc
            // keeps after a free, or they would count in the footprint until something reused them
            if app.popup.is_none() && app.closing.is_empty() {
                crate::gfx::trim_cache(0);
                release_memory();
            }
        });
        self.dirty = true;
    }

    // ---- peek ----------------------------------------------------------------------------------

    /// The mouse moved on the bar of `display` to `p`, or left it (none). A pill whose widget has
    /// a peek grows once the mouse has rested on it a moment, or at once when it's going back; a
    /// grown one the mouse isn't on goes back in a moment (see `left_peek`), so that a mouse that
    /// slips out of it over the bar and back finds it as it was.
    fn bar_hover(&mut self, display: u32, p: Option<CGPoint>) {
        // The bar under a grown pill doesn't count
        if self.mouse_in_peek() {
            return;
        }
        if self.peek.as_ref().is_some_and(|peek| peek.open && !peek.leaving) {
            self.left_peek();
        }
        let pill = p.and_then(|p| {
            let bar = self.bars.iter().find(|b| b.display.id == display)?;
            let slot = self.slots.iter().find(|s| {
                bar.forms.get(&s.name).is_some_and(|f| *f != Form::Hidden) && bar.items.get(&s.name).is_some_and(|r| crate::ui::contains(*r, p))
            })?;
            Some((display, slot.name.clone()))
        });
        if pill == self.hovered_pill {
            return;
        }
        self.hovered_pill = pill.clone();
        self.peek_ticket += 1;
        let Some((display, name)) = pill else { return };
        // Back on one going back: the mouse wants it
        if self.peek.as_ref().is_some_and(|peek| !peek.open && peek.widget == name && peek.display == display) {
            self.open_peek(&name, display);
            return;
        }
        let ticket = self.peek_ticket;
        runtime::after(PEEK_DELAY, move |app| {
            if app.peek_ticket == ticket {
                app.open_peek(&name, display);
            }
        });
    }

    /// The mouse left the grown pill: it goes back, unless the mouse is in it again in a moment,
    /// or drags a slider of it.
    fn left_peek(&mut self) {
        self.leave_ticket += 1;
        let ticket = self.leave_ticket;
        if let Some(peek) = &mut self.peek {
            peek.leaving = true;
        }
        runtime::after(PEEK_LEAVE, move |app| {
            if app.leave_ticket != ticket {
                return;
            }
            if let Some(peek) = &mut app.peek {
                peek.leaving = false;
            }
            let dragging = app.peek.as_ref().is_some_and(|p| p.surface.dragging.is_some());
            if !dragging && !app.mouse_in_peek() {
                app.collapse_peek();
            }
        });
    }

    /// The mouse is on the grown pill.
    fn mouse_in_peek(&self) -> bool {
        self.peek.as_ref().is_some_and(|p| p.open && crate::ui::contains(p.frame, NSEvent::mouseLocation()))
    }

    /// `rustybar msg peek <widget>`: grows its pill on the screen of the mouse, or brings it back.
    /// One grown this way stays until the mouse leaves it, or moves on the bar.
    pub fn toggle_peek(&mut self, widget: &str) {
        if self.peek.as_ref().is_some_and(|p| p.widget == widget && p.open) {
            self.collapse_peek();
        } else {
            let display = self.mouse_display();
            self.open_peek(widget, display);
        }
    }

    /// Grows the pill of `widget` on `display` with its peek, if it has one and its popup isn't
    /// open: drawn first as the pill is, at once, then grown at the next render, animated, while
    /// the pill in the bar hides under it. One on its way back grows again.
    fn open_peek(&mut self, widget: &str, display: u32) {
        if self.popup.as_ref().is_some_and(|p| p.widget == widget) {
            return;
        }
        // One shrinking grows again from where it is; one already handed back to the pill (see
        // `collapse_peek`) starts again, as a new one
        if let Some(peek) = &mut self.peek
            && peek.widget == widget
            && peek.display == display
            && peek.hides_pill
        {
            if !peek.open {
                peek.open = true;
                peek.leaving = false;
                peek.grown_glass = true;
                peek.serial += 1;
                peek.window.set_ignores_mouse(false);
                self.hovered_pill = None;
                self.dirty = true;
                self.with_slot(widget, |w, cx| w.event(&Event::PeekOpened, cx));
            }
            return;
        }
        // Another pill's peek gives way only to one with something to show
        let Some((_, frame)) = self.peek_tree(widget, display, false) else { return };
        self.drop_peek();
        let level = self.bar_level();
        let Some(bar) = self.bars.iter_mut().find(|b| b.display.id == display) else { return };
        // The bar's highlight goes with its pill: the peek has its own
        bar.surface.set_hover(None, None);
        let mut surface = Surface::new(bar.display.scale, self.theme.highlight);
        surface.animate = false;
        let window = Window::new(self.mtm, SurfaceId::Peek, frame, level, &surface.root, Backdrop::None, 0.0);
        self.peek = Some(Peek { widget: widget.to_string(), display, window, surface, open: false, hides_pill: true, frame, serial: 0, leaving: false, grown_glass: true });
        if !self.render_peek(false) {
            self.peek = None;
            return;
        }
        let Some(peek) = &mut self.peek else { return };
        peek.window.show();
        peek.open = true;
        self.instant = true;
        self.hovered_pill = None;
        self.dirty = true;
        // The top under the mouse has the highlight the pill had
        let m = NSEvent::mouseLocation();
        self.on_mouse(SurfaceId::Peek, Mouse::Moved, CGPoint::new(m.x - frame.origin.x, frame.origin.y + frame.size.height - m.y));
        self.with_slot(widget, |w, cx| w.event(&Event::PeekOpened, cx));
    }

    /// The grown pill shrinks back to the pill; then the pill in the bar shows again under it,
    /// and then it goes. The mouse goes through it meanwhile.
    fn collapse_peek(&mut self) {
        let animate = self.animations();
        let Some(peek) = &mut self.peek else { return };
        if !peek.open {
            return;
        }
        peek.open = false;
        peek.grown_glass = false;
        peek.serial += 1;
        peek.window.set_ignores_mouse(true);
        peek.surface.set_hover(None, None);
        peek.surface.set_slider_hover(None);
        let (widget, serial) = (peek.widget.clone(), peek.serial);
        self.dirty = true;
        self.with_slot(&widget, |w, cx| w.event(&Event::PeekClosed, cx));
        runtime::after(if animate { PEEK_ANIMATION + 0.02 } else { 0.0 }, move |app| {
            let Some(peek) = app.peek.as_mut().filter(|p| !p.open && p.serial == serial) else { return };
            // The pill shows and the peek hides in the same render, so in the same frame (see
            // `render_peek`): never a frame without either, nor with both, as two glasses one
            // over the other have a brighter rim. Its window goes once that's on screen.
            peek.hides_pill = false;
            app.instant = true;
            app.dirty = true;
            runtime::after(0.1, move |app| {
                if app.peek.as_ref().is_some_and(|p| !p.open && p.serial == serial) {
                    app.peek = None;
                    // As after a popup (see `close_popup`)
                    if app.popup.is_none() && app.closing.is_empty() {
                        crate::gfx::trim_cache(0);
                        release_memory();
                    }
                }
            });
        });
    }

    /// The peek goes at once, without shrinking first: a reload, a change of screens, another
    /// pill's, or nothing to show anymore.
    fn drop_peek(&mut self) {
        let Some(peek) = self.peek.take() else { return };
        if peek.hides_pill {
            self.instant = true;
            self.dirty = true;
        }
        if peek.open {
            self.with_slot(&peek.widget, |w, cx| w.event(&Event::PeekClosed, cx));
        }
    }

    /// Draws the peek as it is now, grown or going back, and fits its window to it. False when
    /// it has nothing to show anymore.
    fn render_peek(&mut self, animate: bool) -> bool {
        let Some(peek) = &self.peek else { return false };
        let Some((mut tree, frame)) = self.peek_tree(&peek.widget, peek.display, peek.open) else { return false };
        // Handed back to the pill in the bar, which shows in this same render: hidden at once,
        // and its glass taken away, as a glass at alpha 0 still shows for a few frames
        let handed_back = !peek.open && !peek.hides_pill;
        if handed_back && let Some(card) = tree.children.first_mut() {
            card.style.opacity = 0.0;
            card.style.glass = false;
        }
        let animate = animate && !handed_back;
        let scale = self.bars.iter().find(|b| b.display.id == peek.display).map_or(2.0, |b| b.display.scale);
        let laid = Layout { scale }.place(&tree, 0.0, 0.0, frame.size.width, frame.size.height);
        let t = &self.theme;
        let Some(peek) = self.peek.as_mut() else { return false };
        if peek.frame != frame {
            peek.window.set_frame(frame);
            peek.frame = frame;
        }
        peek.surface.animate = animate;
        peek.surface.motion = animate;
        peek.surface.render(&tree, &laid);
        peek.window.set_dark(t.glass_dark);
        let mut glass = peek.surface.glass.clone();
        if peek.grown_glass {
            for g in &mut glass {
                g.rect = CGRect::new(CGPoint::new(0.0, 0.0), frame.size);
            }
        }
        peek.window.set_glass(&glass, t.clear_glass, animate && !peek.grown_glass);
        true
    }

    /// The peek of `widget` on `display`, and the frame of its window (Cocoa coordinates): the
    /// pill as the bar has it, where it is, grown down when `open` to what the widget's `peek`
    /// shows, and wider if that is, as its popup lines up (see `side` below). The
    /// pill's look (glass, background, border) goes to a card that grows and clips what's in
    /// it; its content stays where it is. The window has the grown size from the start, so that
    /// only the layers and the glass move. None when the widget has nothing to show, or its pill
    /// isn't in the bar.
    fn peek_tree(&self, widget: &str, display: u32, open: bool) -> Option<(Node, CGRect)> {
        let bar = self.bars.iter().find(|b| b.display.id == display)?;
        let form = bar.forms.get(widget).copied().filter(|f| *f != Form::Hidden)?;
        let item = bar.items.get(widget).copied()?;
        let pill = bar
            .peek_pill
            .clone()
            .filter(|n| n.key.as_deref() == Some(widget))
            .or_else(|| bar.tree.children.iter().flat_map(|g| &g.children).find(|n| n.key.as_deref() == Some(widget)).cloned())?;
        let slot = self.slots.iter().find(|s| s.name == widget && s.failed.is_none())?;
        if !slot.widget.ready() {
            return None;
        }
        let d = &bar.display;
        let t = &self.theme;
        let rx = Rx::new(&slot.name, t, d, form, false);
        let mut content = catch_unwind(AssertUnwindSafe(|| slot.widget.peek(&rx))).ok().flatten()?;
        content.normalize();
        // As its popup does; the center group beside the notch is at the inner end of a side, and
        // grows into the room it found there, not over that side's pills
        let side = match (slot.side, bar.center) {
            (Side::Center, Some((side, _))) => side,
            (side, _) => side,
        };

        let layout = Layout { scale: d.scale };
        let (pill_w, pill_h) = (item.size.width, item.size.height);
        let (content_w, content_h) = layout.intrinsic(&content, None);
        let (w, h) = (pill_w.max(content_w).ceil(), (pill_h + content_h).ceil());
        let bar_frame = bar.window.panel.frame();
        let pill_x = bar_frame.origin.x + item.origin.x;
        let top = bar_frame.origin.y + bar_frame.size.height - item.origin.y;
        let x = match side {
            Side::Left => pill_x,
            Side::Right => pill_x + pill_w - w,
            Side::Center => pill_x + ((pill_w - w) / 2.0).round(),
        };
        let margin = self.config.bar.margin;
        let (min_x, max_x) = (d.frame.origin.x + margin, d.frame.origin.x + d.frame.size.width - margin - w);
        let x = x.clamp(min_x, max_x.max(min_x));
        let frame = CGRect::new(CGPoint::new(x, top - h), CGSize::new(w, h));
        let px = pill_x - x;

        let look = pill.style.clone();
        let mut row = pill;
        row.key = Some("pill".into());
        row.style.glass = false;
        row.style.background = None;
        row.style.border = None;
        row.style.opacity = 1.0;
        row.style.width = Some(pill_w);
        row.style.height = Some(pill_h);
        let (card_x, card_w, card_h) = if open { (0.0, w, h) } else { (px, pill_w, pill_h) };
        let mut card = Node::default().key("card").size(card_w, card_h).at(card_x, 0.0).radius(look.radius).opacity(look.opacity).clip();
        // Grown, it stays the pill it was: the same glass and veil
        card.style.glass = look.glass;
        card.style.border = look.border;
        card.style.background = look.background;
        card.children.push(row.at(px - card_x, 0.0));
        if open {
            content.key = Some("peek".into());
            content.style.width = Some(w);
            card.children.push(content.at(0.0, pill_h));
        }
        let mut tree = Node::default().key("peek").size(w, h).push(card);
        tree.normalize();
        Some((tree, frame))
    }

    pub fn quit(&mut self) {
        self.cache.save();
        objc2_app_kit::NSApplication::sharedApplication(self.mtm).terminate(None);
    }

    /// For `rustybar msg query`: the widgets and their state.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        for s in &self.slots {
            let side = match s.side {
                Side::Left => "left",
                Side::Center => "center",
                Side::Right => "right",
            };
            let state = match (&s.failed, s.widget.ready()) {
                (Some(e), _) => format!("error: {e}"),
                (None, true) => "ready".into(),
                (None, false) => "loading".into(),
            };
            out.push_str(&format!("{:<16} {:<7} {}\n", s.name, side, state));
        }
        // Where each pill is on screen, top-left origin like CGEvent, for scripts and tests
        let main_h = self.bars.iter().find(|b| b.display.frame.origin.x == 0.0 && b.display.frame.origin.y == 0.0).map(|b| b.display.frame.size.height).unwrap_or(0.0);
        for b in &self.bars {
            let f = b.window.panel.frame();
            let mut items: Vec<_> = b.items.iter().collect();
            items.sort_by(|a, b| a.1.origin.x.total_cmp(&b.1.origin.x));
            for (k, r) in items {
                let x = f.origin.x + r.origin.x;
                let y = main_h - (f.origin.y + f.size.height) + r.origin.y;
                out.push_str(&format!("  {:<14} {:<16} {:.0},{:.0} {:.0}x{:.0}\n", b.display.name, k, x, y, r.size.width, r.size.height));
            }
            // The widgets that gave way, so that the sides don't overlap
            for (form, label) in [(Form::Slim, "slim"), (Form::Compact, "compact"), (Form::Hidden, "in ⋯")] {
                let names: Vec<&str> = self
                    .slots
                    .iter()
                    .filter(|s| b.forms.get(&s.name) == Some(&form) && !(form == Form::Hidden && s.side == Side::Center))
                    .map(|s| s.name.as_str())
                    .collect();
                if !names.is_empty() {
                    out.push_str(&format!("  {:<14} {:<16} {}\n", b.display.name, label, names.join(", ")));
                }
            }
            // Beside the notch, where the center group went
            if b.display.notch.is_some() && self.slots.iter().any(|s| s.side == Side::Center && b.forms.contains_key(&s.name)) {
                let place = match b.center {
                    Some((Side::Left, _)) => "on the left",
                    Some(_) => "on the right",
                    None => "hidden",
                };
                out.push_str(&format!("  {:<14} {:<16} {}\n", b.display.name, "center", place));
            }
        }
        if let Some(p) = &self.peek {
            out.push_str(&format!("peek: {} ({})\n", p.widget, if p.open { "grown" } else { "going back" }));
        }
        out.push_str(&format!("displays: {}\n", self.bars.iter().map(|b| format!("{} ({}x{}{})", b.display.name, b.display.frame.size.width, b.display.frame.size.height, if b.display.notch.is_some() { ", notch" } else { "" })).collect::<Vec<_>>().join(", ")));
        out
    }
}

/// The `⋯` pills: in place of the widgets of their side the bar has no room for, which are in their
/// popups. Not names a widget of `[layout]` would have.
const MORE_LEFT: &str = "⋯left";
const MORE_RIGHT: &str = "⋯right";

/// Where the center group goes on a screen with a notch: at the inner end of the left side, if it
/// fits there beside that side's widgets (`used`, in `room`), else of the right one; else the same
/// in its compact form (`compact_w`, none when it has no other); else nowhere. A better place than
/// the last one (`prev`) needs some room to spare, so that the group doesn't go back and forth when
/// its width or the sides' changes a little (a title, a number).
fn place_center(used: [f64; 2], room: [f64; 2], full_w: f64, compact_w: Option<f64>, gap: f64, prev: Option<(Side, Form)>) -> Option<(Side, Form)> {
    let mut places = vec![(Side::Left, Form::Full, full_w), (Side::Right, Form::Full, full_w)];
    if let Some(w) = compact_w {
        places.extend([(Side::Left, Form::Compact, w), (Side::Right, Form::Compact, w)]);
    }
    let last = prev.and_then(|p| places.iter().position(|&(side, form, _)| (side, form) == p)).unwrap_or(places.len());
    places
        .iter()
        .enumerate()
        .find(|&(k, &(side, _, w))| {
            let s = (side == Side::Right) as usize;
            let need = used[s] + if used[s] > 0.0 { gap } else { 0.0 } + w;
            need <= room[s] - if k < last { fit::SLACK } else { 0.0 }
        })
        .map(|(_, &(side, form, _))| (side, form))
}

/// The room beside the center group (see `App::widen`): `spare` when it's less than at the last
/// render (`prev`) or more by a few points, else `prev`.
fn steady_spare(prev: f64, spare: f64) -> f64 {
    const STEADY: f64 = 8.0;
    if (prev..prev + STEADY).contains(&spare) { prev } else { spare.max(0.0).floor() }
}

fn more_name(side: Side) -> &'static str {
    if side == Side::Left { MORE_LEFT } else { MORE_RIGHT }
}

fn more_side(name: &str) -> Option<Side> {
    match name {
        MORE_LEFT => Some(Side::Left),
        MORE_RIGHT => Some(Side::Right),
        _ => None,
    }
}

fn skeleton_pill(rx: &Rx) -> Node {
    let t = rx.theme;
    rx.pill(vec![Node::new(Kind::Skeleton).size(44.0, 10.0).radius(5.0).background(t.surface)])
}

fn error_pill(rx: &Rx, err: &str) -> Node {
    let t = rx.theme;
    let _ = err;
    rx.pill(vec![
        Node::symbol("exclamationmark.triangle.fill", t.icon_size, Weight::Semibold, t.red),
        Node::text(rx.name, t.font(), t.red),
    ])
    .click(rx.toggle_popup())
}

fn config_error_pill(t: &Theme, err: &str) -> Node {
    Node::row(vec![
        Node::symbol("exclamationmark.triangle.fill", t.icon_size, Weight::Semibold, t.base),
        Node::text(err, t.font(), t.base).max_width(600.0),
    ])
    .key("config-error")
    .height(t.pill_height)
    .padding(0.0, t.pill_padding, 0.0, t.pill_padding)
    .gap(6.0)
    .background(t.red)
    .radius(t.pill_radius)
    .click(Action::Open(config::config_path().display().to_string()))
}

/// Gives the free pages of every malloc zone back to macOS.
fn release_memory() {
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut libc::c_void, goal: usize) -> usize;
    }
    // A null zone is all of them, and a goal of 0 as much as they can
    unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
}

#[allow(dead_code)]
fn _strings() -> &'static objc2_foundation::NSString {
    ns_string!("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_room_beside_the_center_takes_a_side_that_shrinks_a_little_only_later() {
        // A side that grows takes the room at once, even by a point
        assert_eq!(steady_spare(100.0, 98.6), 98.0);
        // One that shrinks back a little leaves it as it was, until it's a few points
        assert_eq!(steady_spare(98.0, 100.0), 98.0);
        assert_eq!(steady_spare(98.0, 112.0), 112.0);
        // None at all, never less
        assert_eq!(steady_spare(0.0, -20.0), 0.0);
    }

    #[test]
    fn the_center_goes_left_then_right_then_compact_then_nowhere() {
        // 100 wide, 60 compact, with a gap of 8, beside sides of 300 and 200; from hidden, so with
        // room to spare
        let place = |room: [f64; 2]| place_center([300.0, 200.0], room.map(|r| r + fit::SLACK), 100.0, Some(60.0), 8.0, None);
        assert_eq!(place([408.0, 308.0]), Some((Side::Left, Form::Full)));
        assert_eq!(place([407.0, 308.0]), Some((Side::Right, Form::Full)));
        assert_eq!(place([407.0, 307.0]), Some((Side::Left, Form::Compact)));
        assert_eq!(place([367.0, 268.0]), Some((Side::Right, Form::Compact)));
        assert_eq!(place([367.0, 267.0]), None);
        // An empty side has no gap to leave
        assert_eq!(place_center([0.0, 0.0], [100.0, 0.0], 100.0, None, 8.0, Some((Side::Left, Form::Full))), Some((Side::Left, Form::Full)));
    }

    #[test]
    fn the_center_moves_back_only_with_room_to_spare() {
        let place = |left: f64, prev| place_center([300.0, 0.0], [left, 400.0], 100.0, None, 8.0, prev);
        // On the right since the left had no room: it stays there with just enough on the left
        assert_eq!(place(410.0, Some((Side::Right, Form::Full))), Some((Side::Right, Form::Full)));
        assert_eq!(place(408.0 + fit::SLACK, Some((Side::Right, Form::Full))), Some((Side::Left, Form::Full)));
        // On the left, it stays while it fits at all
        assert_eq!(place(408.0, Some((Side::Left, Form::Full))), Some((Side::Left, Form::Full)));
        // Hidden: back only with room to spare too
        let hidden = |left: f64| place_center([300.0, 300.0], [left, 400.0], 100.0, None, 8.0, None);
        assert_eq!(hidden(410.0), None);
        assert_eq!(hidden(408.0 + fit::SLACK), Some((Side::Left, Form::Full)));
    }
}
