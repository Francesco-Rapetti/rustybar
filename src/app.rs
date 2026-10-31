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
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAnimationContext, NSApplicationDidChangeScreenParametersNotification,
    NSEvent, NSEventMask, NSMainMenuWindowLevel, NSPopUpMenuWindowLevel, NSStatusWindowLevel,
    NSWorkspace, NSWorkspaceDidWakeNotification,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSNumber, NSObjectProtocol, ns_string};
use objc2_quartz_core::{CAMediaTiming, CASpringAnimation};

use crate::cache::Cache;
use crate::config::{self, Config, Options};
use crate::display::{self, DisplayInfo};
use crate::gfx::Weight;
use crate::runtime::{self, Timer};
use crate::surface::{Mouse, SurfaceId, Window};
use crate::theme::Theme;
use crate::ui::{Action, HitKind, Kind, Layout, Node, Surface};
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
    monitor: Option<Retained<AnyObject>>,
    watcher: Option<notify::RecommendedWatcher>,
    closing: Vec<Window>,
    started: Instant,
    first_frame_logged: bool,
}

static GEN: AtomicU64 = AtomicU64::new(1);

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
                    None => crate::log(&format!("unknown widget {name} (type {kind})")),
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
        }
        self.reconcile_displays();
        self.create_widgets();
        crate::log("reloaded");
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

    fn compact(&self, name: &str, d: &DisplayInfo) -> bool {
        (d.notch.is_some() && self.config.displays.compact_on_notch)
            || self.config.displays.always_compact.iter().any(|n| n == name)
    }

    /// The node of a widget in the bar of `d`: its own, its last known one (dimmed) until it's
    /// ready, a skeleton the very first time, or its error.
    fn item(&mut self, i: usize, d: &DisplayInfo) -> Option<Node> {
        let compact = self.compact(&self.slots[i].name, d);
        let popup_open = self.popup.as_ref().is_some_and(|p| p.widget == self.slots[i].name);
        let slot = &self.slots[i];
        if d.notch.is_some() && self.config.displays.hide_on_notch.contains(&slot.name) {
            return None;
        }
        let rx = Rx { name: &slot.name, theme: &self.theme, display: d, compact, popup_open };
        if let Some(err) = &slot.failed {
            return Some(error_pill(&rx, err));
        }
        let variant = if compact { "compact" } else { "full" };
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

    fn bar_tree(&mut self, d: &DisplayInfo) -> Node {
        let height = self.config.bar.height;
        let width = d.frame.size.width;
        let margin = self.config.bar.margin;
        let layout = Layout { scale: d.scale };
        let mut groups: Vec<(Side, Node)> = Vec::new();
        for side in [Side::Left, Side::Center, Side::Right] {
            let mut children: Vec<Node> = Vec::new();
            if side == Side::Left
                && let Some(err) = self.error.clone() {
                    children.push(config_error_pill(&self.theme, &err));
                }
            for i in 0..self.slots.len() {
                if self.slots[i].side == side
                    && let Some(n) = self.item(i, d) {
                        children.push(n);
                    }
            }
            let key = match side {
                Side::Left => "left",
                Side::Center => "center",
                Side::Right => "right",
            };
            groups.push((side, Node::row(children).gap(self.theme.item_gap).key(key)));
        }
        let mut root = Node::default().size(width, height).key("bar");
        for (side, group) in groups {
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

    pub fn render(&mut self) {
        self.dirty = false;
        if let Some(lua) = &self.lua {
            lua.begin_frame();
        }
        let animate = self.animations();
        for b in 0..self.bars.len() {
            let d = self.bars[b].display.clone();
            let mut tree = self.bar_tree(&d);
            tree.normalize();
            let laid = Layout { scale: d.scale }.run(&tree, Some(d.frame.size.width));
            let bar = &mut self.bars[b];
            bar.surface.animate = animate && self.first_frame_logged;
            bar.surface.render(&tree, &laid);
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
        }
        self.render_popup(animate);
        crate::gfx::trim_cache(256);
        if !self.first_frame_logged && !self.bars.is_empty() {
            self.first_frame_logged = true;
            crate::log(&format!("first frame in {} ms", self.started.elapsed().as_millis()));
        }
        self.cache.save_soon();
    }

    // ---- displays ------------------------------------------------------------------------------

    fn bar_level(&self) -> isize {
        if self.config.bar.topmost { NSStatusWindowLevel } else { NSMainMenuWindowLevel - 1 }
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
                    let window = Window::new(self.mtm, SurfaceId::Bar(d.id), frame, level, &surface.root, None);
                    window.show();
                    self.bars.push(Bar { display: d, window, surface, items: HashMap::new() });
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
                }
            }
            Mouse::Exited => surface.set_hover(None, None),
            Mouse::Down => {
                if let Some(h) = surface.hit(p, &[HitKind::Slider]).cloned() {
                    let v = ((p.x - h.rect.origin.x) / h.rect.size.width).clamp(0.0, 1.0);
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
                        let v = ((p.x - h.rect.origin.x) / h.rect.size.width).clamp(0.0, 1.0);
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
        let Some(slot) = self.slots.iter().find(|s| s.name == widget) else { return };
        let side = slot.side;
        let Some(bar) = self.bars.iter().find(|b| b.display.id == display).or(self.bars.first()) else { return };
        let Some(item) = bar.items.get(widget).copied() else { return };
        let d = bar.display.clone();
        let bar_frame = bar.window.panel.frame();
        // The pill in screen coordinates (Cocoa: y up)
        let anchor = CGRect::new(
            CGPoint::new(bar_frame.origin.x + item.origin.x, bar_frame.origin.y + bar_frame.size.height - item.origin.y - item.size.height),
            item.size,
        );
        let blur = self.theme.popup_blur.then_some(self.theme.popup_radius);
        let mut surface = Surface::new(d.scale, self.theme.highlight);
        surface.animate = false;
        let frame = CGRect::new(anchor.origin, CGSize::new(10.0, 10.0));
        let level = NSPopUpMenuWindowLevel;
        let window = Window::new(self.mtm, SurfaceId::Popup, frame, level, &surface.root, blur);
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

    /// Draws the popup and fits its window to it. False if the widget has no popup.
    fn render_popup(&mut self, animate: bool) -> bool {
        let Some(popup) = &self.popup else { return false };
        let name = popup.widget.clone();
        let Some(slot) = self.slots.iter().find(|s| s.name == name) else { return false };
        let Some(bar) = self.bars.iter().find(|b| b.display.id == popup.display) else { return false };
        let d = bar.display.clone();
        let compact = self.compact(&name, &d);
        let rx = Rx { name: &slot.name, theme: &self.theme, display: &d, compact, popup_open: true };
        let content = match &slot.failed {
            Some(err) => Some(rx.popup(260.0, vec![rx.section("Error", None), rx.detail(err)])),
            None => {
                let widget = &slot.widget;
                catch_unwind(AssertUnwindSafe(|| widget.popup(&rx))).ok().flatten()
            }
        };
        let Some(content) = content else { return false };
        let t = &self.theme;
        let alpha = if t.popup_blur { t.popup_opacity } else { 1.0 };
        let mut tree = Node::column(vec![content])
            .key("popup")
            .background(t.base.with_alpha(alpha))
            .border(t.popup_border, t.surface)
            .radius(t.popup_radius);
        tree.normalize();
        let layout = Layout { scale: d.scale };
        let (mut w, mut h) = layout.intrinsic(&tree, None);
        // Never taller than the screen below the bar
        let max_h = (popup.anchor.origin.y - d.frame.origin.y - 8.0).max(100.0);
        h = h.min(max_h);
        w = w.ceil();
        let laid = layout.place(&tree, 0.0, 0.0, w, h);
        let gap = self.config.bar.popup_gap;
        let margin = self.config.bar.margin;
        let a = popup.anchor;
        let x = match popup.side {
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
        }
        out.push_str(&format!("displays: {}\n", self.bars.iter().map(|b| format!("{} ({}x{}{})", b.display.name, b.display.frame.size.width, b.display.frame.size.height, if b.display.notch.is_some() { ", notch" } else { "" })).collect::<Vec<_>>().join(", ")));
        out
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
