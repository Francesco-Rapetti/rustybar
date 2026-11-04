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
}

pub struct Popup {
    pub widget: String,
    pub display: u32,
    pub window: Window,
    pub surface: Surface,
    pub anchor: CGRect,
    pub side: Side,
}

pub struct App {
    pub mtm: MainThreadMarker,
    pub config: Config,
    pub theme: Theme,
    /// A config or script error, shown at the start of the bar until fixed.
    pub error: Option<String>,
    pub slots: Vec<Slot>,
    pub bars: Vec<Bar>,
    pub popup: Option<Popup>,
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
        let slot = &mut self.slots[i];
        let mut cx = Cx {
            name: &slot.name,
            generation: self.generation,
            popup_open,
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
        let popup_open = self.popup.as_ref().is_some_and(|p| p.widget == self.slots[i].name);
        let slot = &self.slots[i];
        let rx = Rx::new(&slot.name, &self.theme, d, form, popup_open);
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
                self.cache.put(&name, variant, &node);
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
    /// the inner end of their side.
    fn bar_tree(&mut self, b: usize) -> Node {
        let d = self.bars[b].display.clone();
        let height = self.config.bar.height;
        let width = d.frame.size.width;
        let margin = self.config.bar.margin;
        let gap = self.theme.item_gap;
        let layout = Layout { scale: d.scale };
        let mut forms: HashMap<String, Form> = HashMap::new();

        let mut center = Vec::new();
        for i in 0..self.slots.len() {
            let form = self.config_form(&self.slots[i].name, &d);
            if self.slots[i].side == Side::Center && form != Form::Hidden {
                forms.insert(self.slots[i].name.clone(), form);
                center.extend(self.item(i, &d, form));
            }
        }
        let center = Node::row(center).gap(gap).key("center");
        let (center_w, _) = layout.intrinsic(&center, None);

        // The room of the sides: up to the notch, or to the center group, with a gap; or all the
        // bar between the margins when there's nothing in the middle
        let mut middle = d.notch;
        if center_w > 0.0 {
            let x = ((width - center_w) / 2.0).round();
            middle = Some(middle.map_or((x, x + center_w), |(l, r)| (l.min(x), r.max(x + center_w))));
        }
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
            bar.surface.animate = animate && self.first_frame_logged;
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
            // The popups of the widgets in a `⋯` popup open from the `⋯` pill
            for slot in &self.slots {
                if bar.forms.get(&slot.name) == Some(&Form::Hidden)
                    && let Some(r) = bar.items.get(more_name(slot.side)).copied()
                {
                    bar.items.insert(slot.name.clone(), r);
                }
            }
        }
        // A `⋯` popup goes when its widgets are back in the bar
        if !self.render_popup(animate) && self.popup.as_ref().is_some_and(|p| more_side(&p.widget).is_some()) {
            self.close_popup();
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
                    self.bars.push(Bar { display: d, window, surface, items: HashMap::new(), fit: Vec::new(), forms: HashMap::new() });
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
        };
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
                    let hit = surface.hit(p, &[HitKind::Click]).map(|h| (h.path.clone(), h.hover));
                    match hit {
                        Some((path, hover)) => surface.set_hover(Some(path), hover),
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
                let names: Vec<&str> = self.slots.iter().filter(|s| b.forms.get(&s.name) == Some(&form)).map(|s| s.name.as_str()).collect();
                if !names.is_empty() {
                    out.push_str(&format!("  {:<14} {:<16} {}\n", b.display.name, label, names.join(", ")));
                }
            }
        }
        out.push_str(&format!("displays: {}\n", self.bars.iter().map(|b| format!("{} ({}x{}{})", b.display.name, b.display.frame.size.width, b.display.frame.size.height, if b.display.notch.is_some() { ", notch" } else { "" })).collect::<Vec<_>>().join(", ")));
        out
    }
}

/// The `⋯` pills: in place of the widgets of their side the bar has no room for, which are in their
/// popups. Not names a widget of `[layout]` would have.
const MORE_LEFT: &str = "⋯left";
const MORE_RIGHT: &str = "⋯right";

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
