//! Widgets written in Lua (`~/.config/rustybar/widgets/*.lua`). Lua 5.4 runs inside RustyBar, on
//! the main thread, with a time limit on every call; anything slow (commands, HTTP) runs in the
//! background and calls back.
//!
//! ```lua
//! local rb = require("rustybar")
//! rb.widget {
//!   name = "hello",
//!   every = 60,                                  -- seconds between update() calls
//!   update = function(self) self.n = (self.n or 0) + 1 end,
//!   bar = function(self, ctx)
//!     return rb.ui.pill { rb.ui.icon { "hand.wave" }, rb.ui.text { "Ciao " .. (self.n or 0) },
//!                         on_click = "popup" }
//!   end,
//!   popup = function(self, ctx)
//!     return rb.ui.column { rb.ui.button { rb.ui.text { "Reset" }, on_click = function() self.n = 0 end } }
//!   end,
//! }
//! ```

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{Function, HookTriggers, Lua, RegistryKey, Table, Value, VmState};

use crate::config::{self, Config};
use crate::gfx::{FontSpec, Rgba, Weight};
use crate::runtime;
use crate::theme::Theme;
use crate::ui::{Action, Align, Along, Dir, Justify, Kind, Node, Trace};
use crate::widgets::{Cx, Event, Rx, Widget};

const PRELUDE: &str = include_str!("prelude.lua");
/// The most a single call into Lua may take before it's stopped.
const BUDGET: Duration = Duration::from_millis(250);

struct Inner {
    lua: Lua,
    theme: RefCell<Theme>,
    /// The table passed to rb.widget, by name.
    defs: RefCell<HashMap<String, RegistryKey>>,
    /// The functions of the nodes drawn in the current frame, by widget.
    actions: RefCell<HashMap<String, Vec<RegistryKey>>>,
    /// Callbacks of rb.exec, by id.
    callbacks: RefCell<HashMap<u64, RegistryKey>>,
    next_callback: Cell<u64>,
    deadline: Rc<Cell<Option<Instant>>>,
}

#[derive(Clone)]
pub struct Host(Rc<Inner>);

impl Host {
    /// Loads every widget file. Returns the host and the names of the widgets defined.
    pub fn load(config: &Config, theme: &Theme) -> Result<(Host, Vec<String>), String> {
        let _ = config;
        let lua = Lua::new();
        let deadline: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
        let d = deadline.clone();
        let _ = d;
        let host = Host(Rc::new(Inner {
            lua,
            theme: RefCell::new(theme.clone()),
            defs: RefCell::new(HashMap::new()),
            actions: RefCell::new(HashMap::new()),
            callbacks: RefCell::new(HashMap::new()),
            next_callback: Cell::new(1),
            deadline,
        }));
        host.install().map_err(|e| format!("Lua: {e}"))?;
        let dir = config::config_dir().join("widgets");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "lua")).collect())
            .unwrap_or_default();
        files.sort();
        for file in files {
            let code = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
            let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            host.guard(|| host.0.lua.load(&code).set_name(format!("@widgets/{name}")).exec())
                .map_err(|e| format!("widgets/{name}: {}", short_error(&e)))?;
        }
        let names = host.names();
        Ok((host, names))
    }

    /// The names of the widgets the files define.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.0.defs.borrow().keys().cloned().collect();
        names.sort();
        names
    }

    fn install(&self) -> mlua::Result<()> {
        let lua = &self.0.lua;
        let g = lua.globals();
        // Nothing that blocks the bar or ends it
        let os: Table = g.get("os")?;
        os.set("execute", Value::Nil)?;
        os.set("exit", Value::Nil)?;
        let io: Table = g.get("io")?;
        io.set("popen", Value::Nil)?;

        let deadline = self.0.deadline.clone();
        lua.set_hook(HookTriggers::new().every_nth_instruction(20_000), move |_, _| {
            match deadline.get() {
                Some(d) if Instant::now() > d => Err(mlua::Error::runtime("took too long (more than 250 ms)")),
                _ => Ok(VmState::Continue),
            }
        })?;

        let rb = lua.create_table()?;
        let weak = Rc::downgrade(&self.0);
        rb.set(
            "widget",
            lua.create_function(move |_, def: Table| {
                let name: String = def.get("name").map_err(|_| mlua::Error::runtime("rb.widget needs a name"))?;
                if let Some(inner) = weak.upgrade() {
                    let key = inner.lua.create_registry_value(def)?;
                    inner.defs.borrow_mut().insert(name, key);
                }
                Ok(())
            })?,
        )?;
        rb.set("redraw", lua.create_function(|_, ()| {
            runtime::after(0.0, |app| app.mark_dirty());
            Ok(())
        })?)?;
        rb.set("close_popup", lua.create_function(|_, ()| {
            runtime::after(0.0, |app| app.close_popup());
            Ok(())
        })?)?;
        rb.set("open", lua.create_function(|_, target: String| {
            runtime::open(&target);
            Ok(())
        })?)?;
        rb.set("shell", lua.create_function(|_, cmd: String| {
            runtime::shell(&cmd);
            Ok(())
        })?)?;
        // rb.trigger("name", { key = "value" }): an event for every widget, as `rustybar msg trigger`
        rb.set("trigger", lua.create_function(|_, (name, args): (String, Option<Table>)| {
            let mut map = std::collections::HashMap::new();
            if let Some(args) = args {
                for pair in args.pairs::<String, Value>() {
                    let (k, v) = pair?;
                    let v = match v {
                        Value::String(s) => s.to_str()?.to_string(),
                        Value::Integer(i) => i.to_string(),
                        Value::Number(n) => n.to_string(),
                        Value::Boolean(b) => b.to_string(),
                        _ => continue,
                    };
                    map.insert(k, v);
                }
            }
            runtime::after(0.0, move |app| app.broadcast(&crate::widgets::Event::Trigger { name, args: map }));
            Ok(())
        })?)?;
        let weak = Rc::downgrade(&self.0);
        rb.set(
            "exec",
            lua.create_function(move |_, (cmd, cb): (String, Option<Function>)| {
                let Some(inner) = weak.upgrade() else { return Ok(()) };
                let id = inner.next_callback.get();
                inner.next_callback.set(id + 1);
                if let Some(cb) = cb {
                    let key = inner.lua.create_registry_value(cb)?;
                    inner.callbacks.borrow_mut().insert(id, key);
                }
                runtime::spawn("lua-exec", move || {
                    let out = runtime::run("/bin/sh", &["-c", &cmd], None, Duration::from_secs(30));
                    runtime::post(move |app| {
                        if let Some(host) = app.lua.clone() {
                            if let Err(e) = host.callback(id, out) {
                                crate::log(&format!("rb.exec callback: {e}"));
                            }
                            app.mark_dirty();
                        }
                    });
                });
                Ok(())
            })?,
        )?;
        let weak = Rc::downgrade(&self.0);
        rb.set(
            "color",
            lua.create_function(move |_, name: String| {
                let Some(inner) = weak.upgrade() else { return Ok(None) };
                Ok(inner.theme.borrow().color(&name).map(hex))
            })?,
        )?;
        // rb.home: the states of Home Assistant that the `home` widget follows, and its services
        let home = lua.create_table()?;
        home.set("state", lua.create_function(|lua, id: String| match crate::widgets::home::state(&id) {
            Some(v) => json_to_lua(lua, &v),
            None => Ok(Value::Nil),
        })?)?;
        home.set("online", lua.create_function(|_, ()| Ok(crate::widgets::home::online()))?)?;
        home.set("call", lua.create_function(|_, (domain, service, data): (String, String, Option<Value>)| {
            let data = data.map(|d| lua_to_json(&d)).filter(|d| d.is_object()).unwrap_or_else(|| serde_json::json!({}));
            crate::widgets::home::call(&domain, &service, data);
            Ok(())
        })?)?;
        rb.set("home", home)?;
        g.set("rb", rb.clone())?;
        // require("rustybar") gives the same table
        let loaded: Table = lua.globals().get::<Table>("package")?.get("loaded")?;
        loaded.set("rustybar", rb)?;
        lua.load(PRELUDE).set_name("@prelude").exec()?;
        self.set_theme(&self.0.theme.borrow().clone())?;
        Ok(())
    }

    /// A new theme, for `rb.theme` and the colors the widgets name.
    pub fn retheme(&self, theme: &Theme) {
        *self.0.theme.borrow_mut() = theme.clone();
        if let Err(e) = self.set_theme(theme) {
            crate::log(&format!("lua: {e}"));
        }
    }

    /// `rb.theme`: the palette as hex strings and the sizes.
    fn set_theme(&self, theme: &Theme) -> mlua::Result<()> {
        let lua = &self.0.lua;
        let t = lua.create_table()?;
        for (k, v) in &theme.palette {
            t.set(k.as_str(), hex(*v))?;
        }
        t.set("font_size", theme.font_size)?;
        t.set("small_size", theme.small_size)?;
        t.set("icon_size", theme.icon_size)?;
        t.set("pill_height", theme.pill_height)?;
        let rb: Table = lua.globals().get("rb")?;
        rb.set("theme", t)?;
        Ok(())
    }

    /// Runs `f` with the time limit armed.
    fn guard<R>(&self, f: impl FnOnce() -> mlua::Result<R>) -> mlua::Result<R> {
        let outer = self.0.deadline.get();
        if outer.is_none() {
            self.0.deadline.set(Some(Instant::now() + BUDGET));
        }
        let r = f();
        if outer.is_none() {
            self.0.deadline.set(None);
        }
        r
    }

    fn def(&self, widget: &str) -> Option<Table> {
        let defs = self.0.defs.borrow();
        defs.get(widget).and_then(|k| self.0.lua.registry_value(k).ok())
    }

    /// Calls `def[method](def, args…)` if the widget has that method.
    fn call(&self, widget: &str, method: &str, args: impl mlua::IntoLuaMulti) -> mlua::Result<Value> {
        let Some(def) = self.def(widget) else { return Ok(Value::Nil) };
        match def.get::<Value>(method)? {
            Value::Function(f) => self.guard(|| f.call((def, args))),
            _ => Ok(Value::Nil),
        }
    }

    fn number(&self, widget: &str, field: &str) -> Option<f64> {
        self.def(widget).and_then(|d| d.get::<Option<f64>>(field).ok().flatten())
    }

    fn events(&self, widget: &str) -> Vec<String> {
        self.def(widget).and_then(|d| d.get::<Option<Vec<String>>>("on").ok().flatten()).unwrap_or_default()
    }

    /// Forgets the functions of the previous frame: the nodes drawn now register theirs again.
    pub fn begin_frame(&self) {
        let mut actions = self.0.actions.borrow_mut();
        for (_, keys) in actions.drain() {
            for k in keys {
                let _ = self.0.lua.remove_registry_value(k);
            }
        }
    }

    pub fn call_action(&self, widget: &str, index: usize, value: Option<f64>) -> Result<(), String> {
        let f: Option<Function> = {
            let actions = self.0.actions.borrow();
            actions.get(widget).and_then(|v| v.get(index)).and_then(|k| self.0.lua.registry_value(k).ok())
        };
        match f {
            Some(f) => self.guard(|| f.call::<()>(value)).map_err(|e| short_error(&e)),
            None => Ok(()),
        }
    }

    fn callback(&self, id: u64, out: runtime::Output) -> Result<(), String> {
        let key = self.0.callbacks.borrow_mut().remove(&id);
        let Some(key) = key else { return Ok(()) };
        let f: Function = self.0.lua.registry_value(&key).map_err(|e| e.to_string())?;
        let _ = self.0.lua.remove_registry_value(key);
        self.guard(|| f.call::<()>((out.stdout, out.status.unwrap_or(-1), out.stderr))).map_err(|e| short_error(&e))
    }

    /// Calls `def.bar` or `def.popup` and turns the table it returns into nodes.
    fn render(&self, widget: &str, method: &str, rx: &Rx) -> Result<Option<Node>, String> {
        let lua = &self.0.lua;
        let ctx = (|| -> mlua::Result<Table> {
            let ctx = lua.create_table()?;
            ctx.set("compact", rx.compact)?;
            ctx.set("spare", rx.spare)?;
            ctx.set("popup_open", rx.popup_open)?;
            ctx.set("notch", rx.display.notch.is_some())?;
            ctx.set("large", rx.display.large)?;
            ctx.set("width", rx.display.frame.size.width)?;
            Ok(ctx)
        })()
        .map_err(|e| e.to_string())?;
        let value = self.call(widget, method, ctx).map_err(|e| short_error(&e))?;
        let Value::Table(t) = value else { return Ok(None) };
        let theme = self.0.theme.borrow();
        let mut conv = Convert { host: self, widget, theme: &theme, rx };
        conv.node(&t).map(Some).map_err(|e| format!("{method}(): {e}"))
    }
}

/// Turns the tables made by `rb.ui.*` into nodes.
struct Convert<'a> {
    host: &'a Host,
    widget: &'a str,
    theme: &'a Theme,
    rx: &'a Rx<'a>,
}

impl Convert<'_> {
    fn color(&self, t: &Table, field: &str) -> Option<Rgba> {
        t.get::<Option<String>>(field).ok().flatten().and_then(|s| self.theme.color(&s))
    }

    fn num(&self, t: &Table, field: &str) -> Option<f64> {
        t.get::<Option<f64>>(field).ok().flatten()
    }

    fn string(&self, t: &Table, field: &str) -> Option<String> {
        t.get::<Option<String>>(field).ok().flatten()
    }

    fn action(&self, t: &Table, field: &str) -> mlua::Result<Option<Action>> {
        Ok(match t.get::<Value>(field)? {
            Value::Function(f) => {
                let key = self.host.0.lua.create_registry_value(f)?;
                let mut actions = self.host.0.actions.borrow_mut();
                let list = actions.entry(self.widget.to_string()).or_default();
                list.push(key);
                Some(Action::Lua { widget: self.widget.to_string(), index: list.len() - 1 })
            }
            Value::String(s) => {
                let s = s.to_str()?.to_string();
                Some(match s.as_str() {
                    "popup" => Action::Popup { widget: self.widget.to_string() },
                    _ if s.contains("://") => Action::Open(s),
                    _ => Action::Shell(s),
                })
            }
            _ => None,
        })
    }

    fn node(&mut self, t: &Table) -> mlua::Result<Node> {
        let kind: String = t.get::<Option<String>>("_t")?.unwrap_or_else(|| "row".into());
        let theme = self.theme;
        let first: Option<String> = t.get::<Value>(1).ok().and_then(|v| match v {
            Value::String(s) => s.to_str().ok().map(|s| s.to_string()),
            Value::Number(n) => Some(n.to_string()),
            Value::Integer(n) => Some(n.to_string()),
            _ => None,
        });
        let weight = self.string(t, "weight").map(|w| Weight::parse(&w));
        let mut node = match kind.as_str() {
            "text" => {
                let size = self.num(t, "size").unwrap_or(theme.font_size);
                let font = FontSpec::new(
                    &self.string(t, "font").unwrap_or_else(|| theme.font.clone()),
                    weight.unwrap_or(Weight::Bold),
                    size,
                );
                Node::text(first.unwrap_or_default(), font, self.color(t, "color").unwrap_or(theme.text))
            }
            "icon" => Node::symbol(
                first.unwrap_or_else(|| "questionmark".into()),
                self.num(t, "size").unwrap_or(theme.icon_size),
                weight.unwrap_or(Weight::Semibold),
                self.color(t, "color").unwrap_or(theme.text),
            ),
            "app" => Node::app_icon(first.unwrap_or_default(), self.num(t, "size").unwrap_or(18.0)),
            "image" => Node::image(
                first.unwrap_or_default(),
                self.num(t, "w").unwrap_or(16.0),
                self.num(t, "h").unwrap_or(16.0),
                self.color(t, "color"),
            ),
            "slider" => Node::new(Kind::Slider {
                value: self.num(t, "value").unwrap_or(0.0),
                color: self.color(t, "color").unwrap_or(theme.primary),
                track: theme.surface,
                // The percentage inside, unless `percent = false`
                font: t.get::<Option<bool>>("percent")?.unwrap_or(true).then(|| FontSpec::new(&theme.font, Weight::Bold, 11.0)),
                // Steps, each with the label shown inside instead of the percentage
                labels: t.get::<Option<Vec<String>>>("labels")?.unwrap_or_default(),
            })
            .grow(),
            "progress" => Node::new(Kind::Progress {
                value: self.num(t, "value").unwrap_or(0.0),
                color: self.color(t, "color").unwrap_or(theme.primary),
                track: theme.surface,
            }),
            "toggle" => Node::new(Kind::Toggle {
                on: t.get::<Option<bool>>("on")?.unwrap_or(false),
                color: self.color(t, "color").unwrap_or(theme.primary),
                track: theme.surface,
            }),
            "scroll" => Node::new(Kind::Scroll {
                max_height: self.num(t, "max_height").unwrap_or(300.0),
                start: self.string(t, "start"),
            }),
            "skeleton" => Node::new(Kind::Skeleton).background(theme.surface).radius(4.0),
            "spacer" => Node::spacer(),
            "divider" => Node::default().height(1.0).background(theme.surface),
            "column" => Node::column(vec![]),
            "pill" => self.rx.pill(vec![]),
            "button" => Node::row(vec![]).gap(8.0).pad(4.0, 8.0).radius(theme.row_radius),
            "grid" => Node::row(vec![]).columns(self.num(t, "columns").unwrap_or(7.0) as usize),
            _ => Node::row(vec![]),
        };
        let s = &mut node.style;
        if let Some(v) = self.num(t, "gap") {
            s.gap = v;
        }
        match t.get::<Value>("pad")? {
            Value::Number(n) => s.padding = [n; 4],
            Value::Integer(n) => s.padding = [n as f64; 4],
            Value::Table(p) => {
                let v: Vec<f64> = p.sequence_values::<f64>().flatten().collect();
                s.padding = match v.as_slice() {
                    [a] => [*a; 4],
                    [v, h] => [*v, *h, *v, *h],
                    [t, r, b, l] => [*t, *r, *b, *l],
                    _ => s.padding,
                };
            }
            _ => {}
        }
        if let Some(v) = self.num(t, "width") {
            s.width = Some(v);
        }
        if let Some(v) = self.num(t, "height") {
            s.height = Some(v);
        }
        if let Some(v) = self.num(t, "min_width") {
            s.min_width = Some(v);
        }
        if let Some(v) = self.num(t, "max_width") {
            s.max_width = Some(v);
        }
        if let Some(v) = self.num(t, "grow") {
            s.grow = v;
        }
        if t.get::<Option<bool>>("marquee").ok().flatten() == Some(true) {
            s.marquee = true;
        }
        // A line along the edge as far as `value`, going on by `rate` a second from now
        if let Ok(Some(tr)) = t.get::<Option<Table>>("trace") {
            s.trace = Some(Trace {
                value: self.num(&tr, "value").unwrap_or(0.0).clamp(0.0, 1.0),
                at: crate::time::now(),
                rate: self.num(&tr, "rate").unwrap_or(0.0).max(0.0),
                along: if self.string(&tr, "along").as_deref() == Some("around") { Along::Around } else { Along::Bottom },
                width: self.num(&tr, "width").unwrap_or(2.0),
                color: self.color(&tr, "color").unwrap_or(theme.primary),
                track: self.color(&tr, "track"),
            });
        }
        if let Some(v) = self.num(t, "radius") {
            s.radius = v;
        }
        if let Some(v) = self.num(t, "opacity") {
            s.opacity = v;
        }
        if let Some(c) = self.color(t, "background").or_else(|| self.color(t, "bg")) {
            s.background = Some(c);
        }
        if let Some(w) = self.num(t, "border") {
            s.border = Some((w, self.color(t, "border_color").unwrap_or(theme.surface)));
        }
        if let Some(a) = self.string(t, "align") {
            s.align = match a.as_str() {
                "start" => Align::Start,
                "end" => Align::End,
                "stretch" => Align::Stretch,
                _ => Align::Center,
            };
        }
        if let Some(j) = self.string(t, "justify") {
            s.justify = match j.as_str() {
                "center" => Justify::Center,
                "end" => Justify::End,
                "between" => Justify::Between,
                _ => Justify::Start,
            };
        }
        if let Some(d) = self.string(t, "dir") {
            s.dir = if d == "column" { Dir::Column } else { Dir::Row };
        }
        if let Some(k) = self.string(t, "key") {
            node.key = Some(k);
        }
        if let Some(c) = self.color(t, "hover") {
            node.hover = Some(c);
        }
        if let Some(sel) = self.string(t, "selected") {
            node.selected = Some(sel);
            node.selected_color = Some(self.color(t, "selected_color").unwrap_or(theme.primary));
        }
        node.on_click = match kind.as_str() {
            "slider" | "toggle" => self.action(t, "on_change")?,
            "scroll" => self.action(t, "on_edge")?,
            _ => self.action(t, "on_click")?,
        };
        if kind == "toggle" {
            // A toggle is clicked as a whole
            if let Some(Action::Lua { .. }) = &node.on_click {
                node.hover = Some(Rgba::TRANSPARENT);
            }
        }
        if !matches!(kind.as_str(), "text" | "icon" | "app" | "image") {
            for v in t.sequence_values::<Value>() {
                if let Value::Table(child) = v? {
                    node.children.push(self.node(&child)?);
                }
            }
        }
        Ok(node)
    }
}

fn hex(c: Rgba) -> String {
    let b = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    if c.3 >= 1.0 {
        format!("#{:02x}{:02x}{:02x}", b(c.0), b(c.1), b(c.2))
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", b(c.0), b(c.1), b(c.2), b(c.3))
    }
}

fn short_error(e: &mlua::Error) -> String {
    let s = e.to_string();
    // The first line has the file, line and message; the traceback is for the log
    let first = s.lines().next().unwrap_or(&s).to_string();
    crate::log(&s);
    first
}

/// A widget defined in Lua, as the app sees it.
pub struct LuaWidget {
    name: String,
}

impl LuaWidget {
    pub fn new(name: &str) -> LuaWidget {
        LuaWidget { name: name.to_string() }
    }

    fn host(&self) -> Option<Host> {
        let mut host = None;
        // The app owns the host; widgets reach it through the app while it's borrowed by the
        // caller, so it's cloned out when the slot is created (see App::create_widgets)
        HOST.with(|h| host = h.borrow().clone());
        host
    }
}

thread_local! {
    /// The current host, for the Lua widgets to draw with.
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

pub fn set_current(host: Option<Host>) {
    HOST.with(|h| *h.borrow_mut() = host);
}

impl Widget for LuaWidget {
    fn start(&mut self, cx: &mut Cx) {
        let Some(host) = self.host() else { return };
        if let Err(e) = host.call(&self.name, "init", ()) {
            crate::log(&format!("{}: init: {}", self.name, short_error(&e)));
        }
        if let Some(every) = host.number(&self.name, "every") {
            let name = self.name.clone();
            cx.every::<LuaWidget>(every.max(0.5), move |w, cx| {
                if let Some(host) = w.host()
                    && let Err(e) = host.call(&name, "update", ()) {
                        crate::log(&format!("{name}: update: {}", short_error(&e)));
                    }
                cx.redraw();
            });
            // And once now
            if let Err(e) = host.call(&self.name, "update", ()) {
                crate::log(&format!("{}: update: {}", self.name, short_error(&e)));
            }
        }
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let host = self.host()?;
        match host.render(&self.name, "bar", rx) {
            Ok(n) => n,
            Err(e) => {
                crate::log(&format!("{}: {e}", self.name));
                Some(rx.pill(vec![Node::text("⚠︎ Lua", rx.theme.font(), rx.theme.red)]).click(rx.toggle_popup()))
            }
        }
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let host = self.host()?;
        match host.render(&self.name, "popup", rx) {
            Ok(n) => n,
            Err(e) => Some(rx.popup(260.0, vec![rx.section("Lua", None), rx.detail(&e)])),
        }
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        let Some(host) = self.host() else { return };
        let name = match event {
            Event::Wake => "wake".to_string(),
            Event::ScreensChanged => "screens".to_string(),
            Event::PopupOpened => "popup_opened".to_string(),
            Event::PopupClosed => "popup_closed".to_string(),
            Event::PeekOpened => "peek_opened".to_string(),
            Event::PeekClosed => "peek_closed".to_string(),
            Event::Trigger { name, .. } => name.clone(),
        };
        let wanted = host.events(&self.name);
        if !wanted.is_empty() && !wanted.contains(&name) && !name.starts_with("popup_") {
            return;
        }
        let args: HashMap<String, String> = match event {
            Event::Trigger { args, .. } => args.clone(),
            _ => HashMap::new(),
        };
        if let Err(e) = host.call(&self.name, "event", (name, args)) {
            crate::log(&format!("{}: event: {}", self.name, short_error(&e)));
        }
        cx.redraw();
    }

    crate::any_widget!();
}

fn json_to_lua(lua: &Lua, v: &serde_json::Value) -> mlua::Result<Value> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => Value::Nil,
        J::Bool(b) => Value::Boolean(*b),
        J::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Number(n.as_f64().unwrap_or(0.0)),
        },
        J::String(s) => Value::String(lua.create_string(s)?),
        J::Array(a) => {
            let t = lua.create_table()?;
            for (i, x) in a.iter().enumerate() {
                t.set(i + 1, json_to_lua(lua, x)?)?;
            }
            Value::Table(t)
        }
        J::Object(o) => {
            let t = lua.create_table()?;
            for (k, x) in o {
                t.set(k.as_str(), json_to_lua(lua, x)?)?;
            }
            Value::Table(t)
        }
    })
}

/// A table with items 1… is an array, any other an object.
fn lua_to_json(v: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Value::Boolean(b) => J::Bool(*b),
        Value::Integer(i) => J::from(*i),
        Value::Number(n) => serde_json::Number::from_f64(*n).map(J::Number).unwrap_or(J::Null),
        Value::String(s) => J::String(s.to_string_lossy()),
        Value::Table(t) if t.raw_len() > 0 => J::Array(t.sequence_values::<Value>().flatten().map(|x| lua_to_json(&x)).collect()),
        Value::Table(t) => J::Object(t.pairs::<String, Value>().flatten().map(|(k, x)| (k, lua_to_json(&x))).collect()),
        _ => J::Null,
    }
}
