//! AeroSpace: the workspaces with the icons of their windows, in the order they have on screen,
//! the focused workspace under a highlight that slides from one to the next, the focused window on
//! a darker tile, the one in fullscreen with the fullscreen symbol, and the binding mode while it
//! isn't main. A click on a number shows that workspace, on an icon focuses that window, on the
//! mode goes back to main. Compact, when the bar has no room for the rest, only the focused
//! workspace has its icons: the others are their number.
//!
//! RustyBar talks to AeroSpace's server socket, as its CLI does, so following it starts no
//! process: one connection receives the events (`subscribe --all`), and each change asks for the
//! windows again. If the protocol ever changes, the CLI does the same work.
//!
//! Options: `workspaces = ["1", "2", …]` shows a fixed list (by default the ones AeroSpace knows),
//! `hide_empty = false` keeps the empty ones.
//!
//! With made-up data (see `mock`) the workspaces and windows come from `aerospace.json` and the
//! clicks do nothing.

use std::collections::{HashMap, HashSet};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_graphics::{CGWindowListCopyWindowInfo, CGWindowListOption};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, ns_string};
use serde::Deserialize;

use super::{Cx, Event, Rx, Widget};
use crate::config::Options;
use crate::gfx::Weight;
use crate::runtime;
use crate::ui::{Justify, Node};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Win {
    workspace: String,
    id: u32,
    fullscreen: bool,
    bundle: String,
    app: String,
}

/// Made-up data (see `mock`).
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    workspaces: Vec<String>,
    windows: Vec<Win>,
    focused: String,
    focused_window: Option<u32>,
    mode: Option<String>,
}

/// What a refresh learns from AeroSpace.
struct Snapshot {
    windows: Vec<Win>,
    workspaces: Vec<String>,
    focused_ws: Option<String>,
    focused_win: Option<u32>,
    rank: HashMap<u32, usize>,
}

pub struct Aerospace {
    fixed: Vec<String>,
    hide_empty: bool,
    workspaces: Vec<String>,
    windows: Vec<Win>,
    focused_ws: String,
    focused_win: Option<u32>,
    mode: String,
    /// The apps with a badge in the Dock, from the notifications widget: a dot on their icons.
    badged: HashSet<String>,
    /// The last on-screen order of the windows, for the workspaces that are off screen now.
    rank: HashMap<u32, usize>,
    refreshing: bool,
    pending: bool,
    loaded: bool,
    mock: bool,
    /// The event connection, shut down when the widget goes so its thread ends.
    stream: Arc<Mutex<Option<UnixStream>>>,
}

impl Aerospace {
    pub fn new(options: &Options) -> Aerospace {
        let mock = crate::mock::load::<Mock>("aerospace");
        let mut w = Aerospace {
            fixed: options.strings("workspaces"),
            hide_empty: options.bool("hide_empty").unwrap_or(true),
            workspaces: Vec::new(),
            windows: Vec::new(),
            focused_ws: String::new(),
            focused_win: None,
            mode: "main".into(),
            badged: HashSet::new(),
            rank: HashMap::new(),
            refreshing: false,
            pending: false,
            loaded: false,
            mock: mock.is_some(),
            stream: Arc::new(Mutex::new(None)),
        };
        if let Some(m) = mock {
            (w.workspaces, w.windows, w.focused_ws, w.focused_win) = (m.workspaces, m.windows, m.focused, m.focused_window);
            if let Some(mode) = m.mode {
                w.mode = mode;
            }
            w.loaded = true;
        }
        w
    }

    /// Asks AeroSpace for the windows again, once at a time: changes during a refresh make one
    /// more after it.
    fn refresh(&mut self, cx: &mut Cx) {
        if self.mock {
            return;
        }
        if self.refreshing {
            self.pending = true;
            return;
        }
        self.refreshing = true;
        let (rank, fixed) = (self.rank.clone(), !self.fixed.is_empty());
        cx.background::<Aerospace, _>(
            move || fetch(rank, fixed),
            |w, snapshot, cx| {
                w.refreshing = false;
                if let Some(s) = snapshot {
                    w.windows = s.windows;
                    if !s.workspaces.is_empty() {
                        w.workspaces = s.workspaces;
                    }
                    if let Some(f) = s.focused_ws {
                        w.focused_ws = f;
                    }
                    w.focused_win = s.focused_win;
                    w.rank = s.rank;
                }
                w.loaded = true;
                cx.redraw();
                if std::mem::take(&mut w.pending) {
                    w.refresh(cx);
                }
            },
        );
    }

    fn on_event(&mut self, ev: serde_json::Value, cx: &mut Cx) {
        let field = |k: &str| ev.get(k).and_then(|v| v.as_str()).map(String::from);
        match ev.get("_event").and_then(|v| v.as_str()).unwrap_or("") {
            "mode-changed" => {
                if let Some(m) = field("mode") {
                    self.mode = m;
                    cx.redraw();
                }
                return;
            }
            // The highlight moves at once, the windows follow when AeroSpace has listed them
            "focus-changed" => {
                self.focused_win = ev.get("windowId").and_then(|v| v.as_u64()).map(|v| v as u32);
                if let Some(ws) = field("workspace") {
                    self.focused_ws = ws;
                }
                cx.redraw();
            }
            "focused-workspace-changed" => {
                if let Some(ws) = field("workspace") {
                    self.focused_ws = ws;
                    cx.redraw();
                }
            }
            _ => {}
        }
        self.refresh(cx);
    }

    fn visible(&self) -> Vec<&String> {
        let list = if self.fixed.is_empty() { &self.workspaces } else { &self.fixed };
        list.iter()
            .filter(|ws| !self.hide_empty || **ws == self.focused_ws || self.windows.iter().any(|w| &w.workspace == *ws))
            .collect()
    }

    fn window(&self, rx: &Rx, w: &Win, color: crate::gfx::Rgba) -> Node {
        let th = rx.theme;
        let mut children = Vec::new();
        if !w.bundle.is_empty() && w.bundle != "NULL-APP-BUNDLE-ID" {
            children.push(Node::app_icon(&w.bundle, 19.0).key("icon"));
        } else {
            let letter: String = w.app.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
            children.push(Node::row(vec![Node::text(letter, th.font_with(Weight::Bold, 13.0), color)]).width(19.0).justify(Justify::Center).key("icon"));
        }
        if w.fullscreen {
            children.push(Node::symbol("arrow.up.left.and.arrow.down.right", 11.0, Weight::Heavy, color).key("full"));
        }
        if self.badged.contains(&w.bundle) {
            children.push(Node::symbol("circle.fill", 7.0, Weight::Heavy, th.green).at(16.0, 1.0).key("dot"));
        }
        let mut tile = Node::row(children)
            .key(format!("w{}", w.id))
            .height(20.0)
            .padding(0.0, if w.fullscreen { 4.0 } else { 2.0 }, 0.0, 2.0)
            .gap(2.0)
            .radius(5.0)
            .click(rx.action(&format!("win:{}", w.id)));
        if Some(w.id) == self.focused_win {
            tile = tile.background(th.base.faded(0.35));
        }
        tile
    }
}

impl Widget for Aerospace {
    fn start(&mut self, cx: &mut Cx) {
        if self.mock {
            return;
        }
        let handle = cx.handle::<Aerospace>();
        let stream = self.stream.clone();
        runtime::spawn("aerospace-events", move || {
            let mut delay = 1.0;
            while handle.alive() {
                let h = handle.clone();
                let connected = client::subscribe(&stream, move |ev| {
                    h.send(move |w, cx| w.on_event(ev, cx));
                    h.alive()
                });
                if !handle.alive() {
                    break;
                }
                // AeroSpace quit, restarted or isn't installed: try again, less and less often
                handle.send(|w: &mut Aerospace, cx| {
                    w.loaded = true;
                    cx.redraw();
                });
                delay = if connected { 1.0 } else { f64::min(delay * 2.0, 30.0) };
                std::thread::sleep(Duration::from_secs_f64(delay));
            }
        });
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let groups: Vec<Node> = self
            .visible()
            .into_iter()
            .map(|ws| {
                let focused = *ws == self.focused_ws;
                let color = if focused { th.on_primary } else { th.text };
                let mut children = vec![Node::text(ws.as_str(), th.font(), color).key("n")];
                let wins: Vec<&Win> = self.windows.iter().filter(|w| &w.workspace == ws && (focused || !rx.compact)).collect();
                children.extend(wins.iter().map(|w| self.window(rx, w, color)));
                let right = if wins.is_empty() { 10.0 } else { 4.0 };
                let mut group = Node::row(children)
                    .key(format!("ws{ws}"))
                    .height(22.0)
                    .padding(0.0, right, 0.0, 10.0)
                    .gap(if wins.is_empty() { 0.0 } else { 4.0 })
                    .radius(7.0)
                    .click(rx.action(&format!("ws:{ws}")));
                if focused {
                    group = group.hover(th.on_primary.faded(0.12));
                }
                group
            })
            .collect();
        if groups.is_empty() {
            return None;
        }
        let selected = groups.iter().any(|g| g.key.as_deref() == Some(&format!("ws{}", self.focused_ws))).then(|| format!("ws{}", self.focused_ws));
        let spaces = rx.pill(groups).key("spaces").gap(0.0).padding(0.0, 3.0, 0.0, 3.0).selected(selected, th.primary);
        let mut row = vec![spaces];
        if self.mode != "main" && !self.mode.is_empty() {
            let crust = th.color("crust").unwrap_or(th.base);
            row.push(
                rx.pill(vec![
                    Node::symbol("wrench.and.screwdriver.fill", th.icon_size, Weight::Semibold, crust),
                    Node::text(&self.mode, th.font(), crust),
                ])
                .key("mode")
                .background(th.peach)
                .border(th.pill_border, th.peach)
                .click(rx.action("mode")),
            );
        }
        Some(Node::row(row).key(rx.name).gap(th.item_gap))
    }

    fn ready(&self) -> bool {
        self.loaded
    }

    fn action(&mut self, name: &str, _value: Option<f64>, _cx: &mut Cx) {
        if self.mock {
            return;
        }
        let args: Vec<String> = if let Some(ws) = name.strip_prefix("ws:") {
            vec!["workspace".into(), ws.into()]
        } else if let Some(id) = name.strip_prefix("win:") {
            vec!["focus".into(), "--window-id".into(), id.into()]
        } else if name == "mode" {
            vec!["mode".into(), "main".into()]
        } else {
            return;
        };
        runtime::spawn("aerospace-command", move || {
            let args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            if let Err(e) = client::run(&args) {
                crate::log(&format!("aerospace {}: {e}", args.join(" ")));
            }
        });
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        match event {
            Event::Wake | Event::ScreensChanged => self.refresh(cx),
            // `rustybar msg trigger aerospace_windows`, from bindings that move windows
            Event::Trigger { name, .. } if name == "aerospace_windows" => self.refresh(cx),
            Event::Trigger { name, args } if name == "badges" => {
                self.badged = args.get("bundles").map(|b| b.split(',').filter(|s| !s.is_empty()).map(String::from).collect()).unwrap_or_default();
                cx.redraw();
            }
            _ => {}
        }
    }

    crate::any_widget!();
}

impl Drop for Aerospace {
    fn drop(&mut self) {
        if let Some(s) = self.stream.lock().ok().and_then(|mut s| s.take()) {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Lists the windows and the focus, and puts the windows of each workspace in their order on
/// screen. On a worker thread.
fn fetch(mut rank: HashMap<u32, usize>, fixed: bool) -> Option<Snapshot> {
    let format = "%{workspace}|%{window-id}|%{window-is-fullscreen}|%{app-bundle-id}|%{app-name}";
    let list = client::run(&["list-windows", "--all", "--format", format]).ok()?;
    let windows: Vec<Win> = list
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(5, '|').collect();
            Some(Win {
                workspace: f.first()?.to_string(),
                id: f.get(1)?.parse().ok()?,
                fullscreen: f.get(2) == Some(&"true"),
                bundle: f.get(3).unwrap_or(&"").to_string(),
                app: f.get(4).unwrap_or(&"").to_string(),
            })
        })
        .collect();
    let focused_ws = client::run(&["list-workspaces", "--focused"]).ok().map(|s| s.trim().to_string());
    let focused_win = client::run(&["list-windows", "--focused", "--format", "%{window-id}"]).ok().and_then(|s| s.trim().parse().ok());
    let workspaces = if fixed {
        Vec::new()
    } else {
        client::run(&["list-workspaces", "--all"]).map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()).unwrap_or_default()
    };
    let windows = order(windows, &mut rank);
    Some(Snapshot { windows, workspaces, focused_ws, focused_win, rank })
}

/// The frames of all windows, by window server id (AeroSpace's window id is the same).
fn frames() -> HashMap<u32, (f64, f64, f64, f64)> {
    let mut out = HashMap::new();
    let Some(list) = CGWindowListCopyWindowInfo(CGWindowListOption::OptionAll, 0) else { return out };
    // CFArray of CFDictionary, toll-free bridged
    let list: &NSArray<NSDictionary<NSString, AnyObject>> = unsafe { &*(&*list as *const _ as *const NSArray<NSDictionary<NSString, AnyObject>>) };
    let num = |d: &NSDictionary<NSString, AnyObject>, k: &NSString| -> Option<f64> {
        d.objectForKey(k).and_then(|o| o.downcast::<NSNumber>().ok()).map(|n| n.doubleValue())
    };
    for d in list.iter() {
        let Some(id) = num(&d, ns_string!("kCGWindowNumber")) else { continue };
        let Some(b) = d.objectForKey(ns_string!("kCGWindowBounds")) else { continue };
        let Ok(b): Result<Retained<NSDictionary<NSString, AnyObject>>, _> = b.downcast::<NSDictionary>().map(|b| unsafe { Retained::cast_unchecked(b) }) else {
            continue;
        };
        if let (Some(x), Some(y), Some(w), Some(h)) = (num(&b, ns_string!("X")), num(&b, ns_string!("Y")), num(&b, ns_string!("Width")), num(&b, ns_string!("Height"))) {
            out.insert(id as u32, (x, y, x + w, y + h));
        }
    }
    out
}

/// Sorts the windows of each workspace by left edge, then top edge (a column goes from top to
/// bottom), then right and bottom edge (the windows of an accordion). AeroSpace hides the windows
/// of the workspaces off screen in a corner: those keep the last order seen.
fn order(windows: Vec<Win>, rank: &mut HashMap<u32, usize>) -> Vec<Win> {
    let mut frame = frames();
    // AeroSpace moves the windows after answering: wait until they stay put
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(20));
        let next = frames();
        let moved = windows.iter().any(|w| next.get(&w.id) != frame.get(&w.id));
        frame = next;
        if !moved {
            break;
        }
    }
    let displays = active_displays();
    let on_screen = |id: u32| {
        frame.get(&id).is_some_and(|&(x0, y0, x1, y1)| {
            displays.iter().any(|&(dx0, dy0, dx1, dy1)| x1.min(dx1) - x0.max(dx0) >= 10.0 && y1.min(dy1) - y0.max(dy0) >= 10.0)
        })
    };
    let mut groups: Vec<(String, Vec<Win>)> = Vec::new();
    for w in windows {
        match groups.iter_mut().find(|g| g.0 == w.workspace) {
            Some(g) => g.1.push(w),
            None => groups.push((w.workspace.clone(), vec![w])),
        }
    }
    let mut out = Vec::new();
    for (_, group) in groups {
        let all_on_screen = group.iter().all(|w| on_screen(w.id));
        let mut keyed: Vec<((f64, f64, f64, f64, usize, usize), Win)> = group
            .into_iter()
            .enumerate()
            .map(|(i, w)| {
                let f = if all_on_screen { frame[&w.id] } else { (0.0, 0.0, 0.0, 0.0) };
                ((f.0, f.1, f.2, f.3, rank.get(&w.id).copied().unwrap_or(usize::MAX), i), w)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        out.extend(keyed.into_iter().map(|(_, w)| w));
    }
    rank.clear();
    for (i, w) in out.iter().enumerate() {
        rank.insert(w.id, i);
    }
    out
}

fn active_displays() -> Vec<(f64, f64, f64, f64)> {
    use objc2_core_graphics::{CGDisplayBounds, CGGetActiveDisplayList};
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    unsafe { CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count) };
    ids[..count as usize]
        .iter()
        .map(|&id| {
            let b = CGDisplayBounds(id);
            (b.origin.x, b.origin.y, b.origin.x + b.size.width, b.origin.y + b.size.height)
        })
        .collect()
}

/// AeroSpace's server socket, spoken as its CLI does: the protocol version (a u32), then requests
/// and answers as JSON, each after its length (a u32, little endian).
pub(crate) mod client {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use crate::runtime;

    const PROTOCOL: u32 = 1;

    fn socket_path() -> String {
        let user = std::env::var("USER").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| objc2_foundation::NSUserName().to_string());
        format!("/tmp/bobko.aerospace-{user}.sock")
    }

    enum Connect {
        Ok(UnixStream),
        /// AeroSpace speaks another version: use its CLI.
        Mismatch,
        Down,
    }

    fn connect(timeout: Option<Duration>) -> Connect {
        let Ok(mut s) = UnixStream::connect(socket_path()) else { return Connect::Down };
        let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
        let mut v = [0u8; 4];
        if s.write_all(&PROTOCOL.to_le_bytes()).is_err() || s.read_exact(&mut v).is_err() {
            return Connect::Down;
        }
        if u32::from_le_bytes(v) != PROTOCOL {
            return Connect::Mismatch;
        }
        let _ = s.set_read_timeout(timeout);
        Connect::Ok(s)
    }

    fn send(s: &mut UnixStream, args: &[&str]) -> std::io::Result<()> {
        let body = serde_json::json!({ "command": "", "args": args, "stdin": "", "windowId": null, "workspace": null }).to_string();
        s.write_all(&(body.len() as u32).to_le_bytes())?;
        s.write_all(body.as_bytes())
    }

    fn receive(s: &mut UnixStream) -> std::io::Result<serde_json::Value> {
        let mut len = [0u8; 4];
        s.read_exact(&mut len)?;
        let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
        s.read_exact(&mut body)?;
        serde_json::from_slice(&body).map_err(std::io::Error::other)
    }

    /// Runs an AeroSpace command and returns what it prints.
    pub fn run(args: &[&str]) -> Result<String, String> {
        match connect(Some(Duration::from_secs(3))) {
            Connect::Ok(mut s) => {
                send(&mut s, args).map_err(|e| e.to_string())?;
                let answer = receive(&mut s).map_err(|e| e.to_string())?;
                let text = |k: &str| answer.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
                match answer.get("exitCode").and_then(|v| v.as_i64()) {
                    Some(0) => Ok(text("stdout")),
                    _ => Err(text("stderr").trim().to_string()),
                }
            }
            Connect::Mismatch => {
                let out = runtime::run("aerospace", args, None, Duration::from_secs(3));
                if out.ok() { Ok(out.stdout) } else { Err(out.stderr.trim().to_string()) }
            }
            Connect::Down => Err("AeroSpace isn't running".into()),
        }
    }

    /// Follows AeroSpace's events until the connection ends or `f` returns false. The stream is
    /// kept in `slot`, so the widget can end it. Returns whether it connected.
    pub fn subscribe(slot: &Arc<Mutex<Option<UnixStream>>>, mut f: impl FnMut(serde_json::Value) -> bool) -> bool {
        match connect(None) {
            Connect::Ok(mut s) => {
                if send(&mut s, &["subscribe", "--all"]).is_err() {
                    return false;
                }
                if let (Ok(mut g), Ok(c)) = (slot.lock(), s.try_clone()) {
                    *g = Some(c);
                }
                while let Ok(ev) = receive(&mut s) {
                    if !f(ev) {
                        break;
                    }
                }
                true
            }
            Connect::Mismatch => {
                // The CLI prints one event per line
                let Some(path) = runtime::which("aerospace") else { return false };
                let Ok(mut child) = std::process::Command::new(path)
                    .args(["subscribe", "--all"])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                else {
                    return false;
                };
                let mut connected = false;
                if let Some(out) = child.stdout.take() {
                    use std::io::BufRead;
                    for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                        connected = true;
                        if let Ok(ev) = serde_json::from_str(&line)
                            && !f(ev) {
                                break;
                            }
                    }
                }
                let _ = child.kill();
                let _ = child.wait();
                connected
            }
            Connect::Down => false,
        }
    }
}
