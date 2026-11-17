//! The batteries of the mice, keyboards, trackpads and controllers, and of the other devices that
//! tell theirs (earbuds, headphones). The pill shows the connected ones that run low (`low`, 20%),
//! the kinds of `pinned` always (`["mouse", "keyboard", "controller"]`), or every connected one
//! with `always = true`, side by side with a thin line between them: its kind, a short name, its
//! level as a small bar and a number colored by how full it is, and a bolt while it charges.
//! Compact: the kinds, the bars and the bolts. The popup has a tile for each connected one, with its kind in
//! a round badge, its name, maker, link and state, the level big and as a bar (one for each half of
//! a split keyboard, each earbud and the case); then the others in short rows, and the Bluetooth
//! settings. As the `devices` widget of RustyBar for Windows.
//!
//! Two sources:
//! - macOS's own list, the accessories' power sources (`pmset -g accps`, the Batteries widget):
//!   what tells its battery over Bluetooth (Apple's, the Low Energy ones with the battery service,
//!   headsets over HFP, AirPods with each earbud and the case) and the game controllers macOS
//!   knows. powerd says when it changes (libnotify); a read takes ~0.1 ms, on the main thread.
//!   Over Bluetooth Low Energy a device sends only its level: it charges for us while that goes
//!   up step after step (`rising`).
//! - Receivers and cables (`hid`; `receivers = false` leaves them out): Razer, Logitech, ROG, the
//!   Steam Controller. A pass on a worker thread at the start, a second after one of their
//!   interfaces comes or goes (IOKit says), on wake, when the popup opens and every 5 minutes
//!   while there's one. One that sleeps (a keyboard left alone, a mouse that's off) stays in the
//!   pill, dimmed, with the level it had when it last answered since the bar started.
//!
//! A device seen on both, connected on one side only, shows once: the same maker and kind (a
//! keyboard paired over Bluetooth and on its receiver now). One of macOS's list that goes away
//! stays in the popup's others, with the level it had, until the bar starts again.
//!
//! With made-up data (see `mock`) the devices are those of `devices.json`.

use std::collections::HashMap;
use std::ffi::{c_char, c_int};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2_foundation::ns_string;
use serde::Deserialize;

use super::hid::{self, Found, Kind, Link, Maker};
use super::{Cx, Event, Rx, Widget};
use crate::config::Options;
use crate::gfx::{Rgba, Weight};
use crate::i18n::t;
use crate::theme::Theme;
use crate::ui::{Align, Justify, Kind as NodeKind, Node};

/// The receivers and cables are read again this often while there's one, in seconds; macOS's list
/// too, in case a change went unannounced.
const EVERY: f64 = 5.0 * 60.0;
/// IOKit tells a device's interfaces one by one: they're read this long after the last, in seconds.
const SETTLE: f64 = 1.0;
/// A device that doesn't say whether it charges, charging, goes up a step every few minutes (a
/// keyboard 1–3 points a minute, slower near full); a battery at rest comes back once, or slowly
/// over hours. So it charges while each step up comes this soon after the one before.
const STEADY: Duration = Duration::from_secs(15 * 60);

/// powerd's notifications: an accessory came or went, its charging state or its level changed;
/// and any power source changed (also every minute or so on a Mac with a battery).
const NOTIFICATIONS: [&std::ffi::CStr; 4] =
    [c"com.apple.system.accpowersources.attach", c"com.apple.system.accpowersources.source", c"com.apple.system.accpowersources.timeremaining", c"com.apple.system.powersources"];

// The popup: tiles as wide as the dashboards of the other widgets.
const WIDE: f64 = 328.0;
const INNER: f64 = WIDE - 24.0;
const GAP: f64 = 8.0;
const BADGE: f64 = 34.0;
/// A part's line (a half, an earbud): its label, a bar and the number.
const LABEL: f64 = 60.0;
const NUMBER: f64 = 40.0;

/// A part of a device with its own battery: a half of a split keyboard, an earbud, the case.
#[derive(Clone, Debug, PartialEq)]
struct Part {
    label: String,
    level: u8,
    /// It counts for the device's level (an earbud does, the case doesn't).
    main: bool,
}

/// A device with a battery, as the pill and the popup show it.
#[derive(Clone, Debug, PartialEq)]
struct Device {
    key: String,
    name: String,
    kind: Kind,
    icon: &'static str,
    maker: Maker,
    link: Link,
    /// The lowest of its main parts, or its own; None for one never read since the bar started.
    level: Option<u8>,
    /// More than one only for a device with parts.
    parts: Vec<Part>,
    charging: bool,
    /// It says whether it charges; one that doesn't (a keyboard over Bluetooth Low Energy sends
    /// only its level) charges for us while its level goes up step after step (`rising`).
    says_charging: bool,
    connected: bool,
    /// Behind a receiver or a cable, not answering now (asleep, off) but read since the bar
    /// started: the pill keeps it, dimmed, with that level, and the popup says when it was read.
    asleep: bool,
    read_at: Option<String>,
}

pub struct Devices {
    /// What the pill and the popup show, made of the three below.
    devices: Vec<Device>,
    /// macOS's list now, and what went away from it since the bar started.
    bluetooth: Vec<Device>,
    gone: Vec<Device>,
    wired: Vec<Found>,
    /// The last level of each device behind a receiver, by its key, with its other half and the
    /// time it was read, for while it sleeps.
    last: HashMap<String, (u8, Option<u8>, String)>,
    /// For the devices of macOS's list that don't say whether they charge, by key (`rising`).
    rises: HashMap<String, Rise>,
    /// Names of the user's for the pill, by the full name (lowercase): `names = { "Razer Naga V2
    /// Pro" = "Gioco" }`.
    names: HashMap<String, String>,
    low: u8,
    always: bool,
    pinned: Vec<Kind>,
    receivers: bool,
    /// The pass on a worker thread: running, another asked meanwhile, and whether there's any
    /// interface of the makers to read.
    reading: bool,
    again: bool,
    any_wired: bool,
    /// Bumped by each interface that comes or goes: the pass runs a second after the last.
    settle: u64,
    watch: Option<hid::Watch>,
    tokens: Vec<c_int>,
    mock: bool,
}

impl Devices {
    pub fn new(options: &Options) -> Devices {
        let names = options
            .0
            .get("names")
            .and_then(|v| v.as_table())
            .map(|t| t.iter().filter_map(|(k, v)| Some((k.to_lowercase(), v.as_str()?.trim().to_string()))).filter(|(_, v)| !v.is_empty()).collect())
            .unwrap_or_default();
        Devices {
            devices: Vec::new(),
            bluetooth: Vec::new(),
            gone: Vec::new(),
            wired: Vec::new(),
            last: HashMap::new(),
            rises: HashMap::new(),
            names,
            low: options.f64("low").unwrap_or(20.0).clamp(0.0, 100.0) as u8,
            always: options.bool("always").unwrap_or(false),
            pinned: options.strings("pinned").iter().filter_map(|k| kind_named(k)).collect(),
            receivers: options.bool("receivers").unwrap_or(true),
            reading: false,
            again: false,
            any_wired: false,
            settle: 0,
            watch: None,
            tokens: Vec::new(),
            mock: false,
        }
    }

    /// macOS's list read again; what went away from it stays among the others. True if it changed.
    fn read_accessories(&mut self) -> bool {
        let mut now = accessories();
        rising(&mut self.rises, &mut now, Instant::now());
        if now == self.bluetooth {
            return false;
        }
        let at = time_now();
        for old in self.bluetooth.iter().filter(|o| !now.iter().any(|n| n.key == o.key)) {
            let mut d = old.clone();
            d.connected = false;
            d.charging = false;
            d.read_at = Some(at.clone());
            self.gone.retain(|g| g.key != d.key);
            self.gone.push(d);
        }
        self.gone.retain(|g| !now.iter().any(|n| n.key == g.key));
        self.bluetooth = now;
        true
    }

    fn accessories_changed(&mut self, cx: &mut Cx) {
        if self.read_accessories() {
            self.rebuild(cx);
        }
    }

    /// A pass over the receivers and cables on a worker thread; one at a time, a second asked
    /// meanwhile runs after it.
    fn pass(&mut self, cx: &mut Cx) {
        if !self.receivers || self.mock {
            return;
        }
        if self.reading {
            self.again = true;
            return;
        }
        self.reading = true;
        let started = std::time::Instant::now();
        cx.background::<Devices, _>(hid::pass, move |w, (found, any), cx| {
            w.reading = false;
            w.any_wired = any;
            if found.len() != w.wired.len() {
                crate::log(&format!("devices: {} behind receivers or cables, read in {} ms", found.len(), started.elapsed().as_millis()));
            }
            let at = time_now();
            for f in &found {
                if let Some(level) = f.level {
                    w.last.insert(f.key.clone(), (level, f.second, at.clone()));
                }
            }
            w.wired = found;
            w.rebuild(cx);
            if std::mem::take(&mut w.again) {
                w.pass(cx);
            }
        });
    }

    /// One of the makers' interfaces came or went: a pass once they've all arrived.
    fn interfaces_changed(&mut self, cx: &mut Cx) {
        self.settle += 1;
        let settle = self.settle;
        cx.after::<Devices>(SETTLE, move |w, cx| {
            if w.settle == settle {
                w.pass(cx);
            }
        });
    }

    /// What the pill and the popup show: all the sources, each device once; drawn again if it
    /// changed.
    fn rebuild(&mut self, cx: &mut Cx) {
        let mut wired: Vec<Device> = self
            .wired
            .iter()
            .map(|f| {
                let last = self.last.get(&f.key);
                let (level, second) = match (f.level, last) {
                    (Some(l), _) => (Some(l), f.second),
                    (None, Some((l, s, _))) => (Some(*l), *s),
                    (None, None) => (None, None),
                };
                let parts = match (level, second) {
                    (Some(a), Some(b)) => [a, b].iter().enumerate().map(|(i, l)| Part { label: format!("{} {}", t("Metà", "Half"), i + 1), level: *l, main: true }).collect(),
                    _ => Vec::new(),
                };
                Device {
                    key: f.key.clone(),
                    name: f.name.clone(),
                    kind: f.kind,
                    icon: kind_icon(f.kind),
                    maker: f.maker,
                    link: f.link,
                    level: level.map(|l| second.map_or(l, |s| s.min(l))),
                    parts,
                    charging: f.charging && f.level.is_some(),
                    says_charging: true,
                    connected: f.level.is_some(),
                    asleep: f.level.is_none() && last.is_some(),
                    read_at: last.filter(|_| f.level.is_none()).map(|(_, _, at)| at.clone()),
                }
            })
            .collect();
        let mut bluetooth = self.bluetooth.clone();
        bluetooth.extend(self.gone.iter().cloned());
        let devices = combine(bluetooth, &mut wired);
        if devices != self.devices {
            self.devices = devices;
            cx.redraw();
        }
    }

    /// The connected devices the pill shows (and those asleep behind a receiver, dimmed): the kinds
    /// pinned, those that run low, or all with `always`.
    fn in_pill(&self) -> Vec<&Device> {
        self.devices.iter().filter(|d| (d.connected || d.asleep) && d.level.is_some_and(|l| self.always || self.pinned.contains(&d.kind) || l <= self.low)).collect()
    }

    /// A device's tile in the popup, tinted with its color when it runs low.
    fn tile(&self, rx: &Rx, d: &Device) -> Node {
        let th = rx.theme;
        let p = paint(th, d);
        let low = d.connected && d.level.is_some_and(|l| l <= self.low);
        let badge = Node::row(vec![Node::symbol(d.icon, 15.0, Weight::Semibold, if d.connected { p.fill } else { th.subtext }).key("symbol")])
            .size(BADGE, BADGE)
            .radius(BADGE / 2.0)
            .background(if d.connected { p.fill.faded(0.2) } else { th.text.faded(0.07) })
            .justify(Justify::Center)
            .align(Align::Center)
            .key("badge");
        // The level, big, with the bolt while it charges
        let big = th.font_with(Weight::Semibold, 20.0);
        let value = level_text(d);
        let bolt = 18.0;
        let names = INNER - BADGE - 20.0 - crate::gfx::measure_text(&value, &big).width - if d.charging { bolt } else { 0.0 };
        let mut top = vec![
            badge,
            Node::column(vec![
                Node::text(d.name.as_str(), th.font(), p.text).max_width(names).key("name"),
                Node::text(details(d), th.small_font(), th.subtext).max_width(names).key("details"),
            ])
            .gap(2.0)
            .align(Align::Start)
            .key("names"),
            Node::spacer(),
        ];
        if d.charging {
            top.push(Node::symbol("bolt.fill", 13.0, Weight::Bold, th.yellow).key("bolt"));
        }
        top.push(Node::text(value, big, p.level).key("level"));
        let top = Node::row(top).gap(10.0).width(INNER).align(Align::Center).key("top");
        // The bars: one, or one a part
        let bar = |level: Option<u8>, w: f64, color: Rgba| Node::new(NodeKind::Progress { value: level.unwrap_or(0) as f64 / 100.0, color, track: p.track }).size(w, 6.0);
        let bars = if d.parts.len() > 1 {
            let lines = d
                .parts
                .iter()
                .enumerate()
                .map(|(i, part)| {
                    let (fill, number) = if d.connected { level_colors(th, part.level) } else { (p.fill, th.subtext) };
                    Node::row(vec![
                        Node::text(part.label.as_str(), th.small_font(), th.subtext).max_width(LABEL).width(LABEL).key("label"),
                        bar(Some(part.level), INNER - LABEL - NUMBER, fill).key("bar"),
                        Node::row(vec![Node::text(format!("{}%", part.level), th.small_font(), number)]).width(NUMBER).justify(Justify::End).key("number"),
                    ])
                    .width(INNER)
                    .height(15.0)
                    .align(Align::Center)
                    .key(format!("part-{i}"))
                })
                .collect();
            Node::column(lines).gap(3.0).key("parts")
        } else {
            bar(d.level, INNER, p.fill).key("bar")
        };
        Node::column(vec![top, bars])
            .gap(10.0)
            .padding(10.0, 12.0, 12.0, 12.0)
            .width(WIDE)
            .radius(10.0)
            .background(th.tile(p.fill, low))
            .key(format!("tile-{}", d.key))
    }
}

impl Widget for Devices {
    fn start(&mut self, cx: &mut Cx) {
        if let Some(m) = crate::mock::load::<Mock>("devices") {
            self.mock = true;
            self.devices = m.devices.into_iter().map(MockDevice::device).collect();
            sort(&mut self.devices);
            return;
        }
        self.read_accessories();
        self.rebuild(cx);
        // powerd: an accessory came, went or changed
        for name in NOTIFICATIONS {
            let h = cx.handle::<Devices>();
            let block = RcBlock::new(move |_: c_int| h.send(|w: &mut Devices, cx| w.accessories_changed(cx)));
            let mut token: c_int = 0;
            if unsafe { notify_register_dispatch(name.as_ptr(), &mut token, dispatch2::DispatchQueue::main(), &block) } == 0 {
                self.tokens.push(token);
            }
        }
        if self.receivers {
            let h = cx.handle::<Devices>();
            self.watch = hid::Watch::new(move || h.send(|w: &mut Devices, cx| w.interfaces_changed(cx)));
            self.pass(cx);
        }
        cx.every::<Devices>(EVERY, |w, cx| {
            w.accessories_changed(cx);
            if w.any_wired {
                w.pass(cx);
            }
        });
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let shown = self.in_pill();
        if shown.is_empty() {
            return None;
        }
        // Each device: its kind, its short name, its level as a bar and a number, the bolt while it
        // charges; a thin line between them. Compact: the kinds, the bars and the bolts.
        let mut children = Vec::new();
        for (i, d) in shown.iter().enumerate() {
            let p = paint(th, d);
            let level = d.level.unwrap_or(0);
            if i > 0 {
                children.push(Node::default().size(1.0, 12.0).background(th.text.faded(0.15)).key(format!("rule-{i}")));
            }
            let mut item = vec![Node::symbol(d.icon, th.icon_size, Weight::Semibold, p.text).key("icon")];
            if !rx.compact {
                item.push(Node::text(short_name(&d.name, &self.names), th.small_font(), th.subtext).max_width(110.0).key("name"));
            }
            item.push(Node::new(NodeKind::Progress { value: level as f64 / 100.0, color: p.fill, track: p.track }).size(18.0, 6.0).key("gauge"));
            if !rx.compact {
                item.push(Node::text(format!("{level}%"), th.font(), p.level).key("level"));
            }
            if d.charging {
                item.push(Node::symbol("bolt.fill", 10.0, Weight::Bold, th.yellow).key("bolt"));
            }
            children.push(Node::row(item).gap(5.0).align(Align::Center).key(format!("device-{}", d.key)));
        }
        Some(rx.pill(children).gap(8.0).click(rx.toggle_popup()))
    }

    /// A tile for each device that's connected (or asleep behind its receiver); then the others in
    /// short rows, and the Bluetooth settings.
    fn popup(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let (tiles, others): (Vec<&Device>, Vec<&Device>) = self.devices.iter().partition(|d| d.connected || d.asleep);
        let connected = self.devices.iter().filter(|d| d.connected).count();
        let count = (connected > 0).then(|| {
            let text = if connected == 1 { t("1 collegato", "1 connected").to_string() } else { format!("{connected} {}", t("collegati", "connected")) };
            Node::text(text, th.small_font(), th.subtext).key("count")
        });
        let mut rows = vec![rx.section(t("Dispositivi", "Devices"), count).key("header")];
        if self.devices.is_empty() {
            rows.push(rx.detail(t("Nessun dispositivo con la batteria", "No device with a battery")).key("none"));
        }
        rows.extend(tiles.into_iter().map(|d| self.tile(rx, d)));
        if !others.is_empty() {
            rows.push(Node::text(t("Non collegati", "Not connected"), th.small_font(), th.subtext).padding(4.0, 8.0, 0.0, 8.0).key("others"));
            for d in others {
                rows.push(
                    Node::row(vec![
                        Node::row(vec![Node::symbol(d.icon, 12.0, Weight::Medium, th.subtext)]).width(20.0).justify(Justify::Center).key("icon"),
                        Node::text(d.name.as_str(), th.font_with(Weight::Regular, th.font_size), th.subtext).max_width(WIDE - 120.0).key("name"),
                        Node::spacer(),
                        Node::text(level_text(d), th.font_with(Weight::Regular, th.font_size), th.subtext).key("level"),
                    ])
                    .gap(8.0)
                    .pad(4.0, 8.0)
                    .height(26.0)
                    .width(WIDE)
                    .align(Align::Center)
                    .key(format!("other-{}", d.key)),
                );
            }
        }
        rows.push(rx.row_button(vec![Node::text(t("Impostazioni Bluetooth…", "Bluetooth Settings…"), th.font(), th.primary)], vec![], rx.action("settings")).key("settings"));
        Some(rx.popup(WIDE, rows).gap(GAP).padding(8.0, 8.0, 6.0, 8.0))
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        if name == "settings" {
            cx.close_popup();
            if !self.mock {
                crate::runtime::open("x-apple.systempreferences:com.apple.BluetoothSettings");
            }
        }
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        if self.mock {
            return;
        }
        match event {
            Event::Wake | Event::PopupOpened => {
                self.accessories_changed(cx);
                self.pass(cx);
            }
            _ => {}
        }
    }

    crate::any_widget!();
}

impl Drop for Devices {
    fn drop(&mut self) {
        for t in self.tokens.drain(..) {
            unsafe { notify_cancel(t) };
        }
    }
}

unsafe extern "C" {
    fn notify_register_dispatch(name: *const c_char, out_token: *mut c_int, queue: &dispatch2::DispatchQueue, handler: &block2::Block<dyn Fn(c_int)>) -> u32;
    fn notify_cancel(token: c_int) -> u32;
}

/// `14:05`, now.
fn time_now() -> String {
    let tm = crate::time::local(crate::time::now());
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

/// macOS's list of the accessories' batteries, as `pmset -g accps` shows it; the parts of one
/// device (AirPods: each earbud, the case) in one.
fn accessories() -> Vec<Device> {
    use crate::iokit::{number, string};
    let mut groups: Vec<(String, Vec<Source>)> = Vec::new();
    for d in crate::iokit::power_sources(true) {
        if number(&d, ns_string!("Is Present")) == Some(0) {
            continue;
        }
        let Some(current) = number(&d, ns_string!("Current Capacity")) else { continue };
        let max = number(&d, ns_string!("Max Capacity")).filter(|m| *m > 0).unwrap_or(100);
        let name = string(&d, ns_string!("Name")).unwrap_or_default();
        let part = string(&d, ns_string!("Part Identifier")).unwrap_or_default();
        let s = Source {
            name: name.clone(),
            category: string(&d, ns_string!("Accessory Category")).unwrap_or_default(),
            transport: string(&d, ns_string!("Transport Type")).unwrap_or_default(),
            vendor: number(&d, ns_string!("Vendor ID")).unwrap_or(0) as u16,
            level: (current * 100 / max).clamp(0, 100) as u8,
            // Without the key, a source on power charges: else we can't tell
            charging: number(&d, ns_string!("Is Charging"))
                .map(|c| c != 0)
                .or_else(|| (string(&d, ns_string!("Power Source State")).as_deref() == Some("AC Power")).then_some(true)),
            part: part.clone(),
        };
        // The parts of one device have its name; anything else is one by itself
        let key = match part.as_str() {
            "Left" | "Right" | "Case" => format!("parts:{name}"),
            _ => string(&d, ns_string!("Accessory Identifier")).unwrap_or(name),
        };
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, g)) => g.push(s),
            None => groups.push((key, vec![s])),
        }
    }
    let mut out: Vec<Device> = groups.into_iter().map(|(key, sources)| accessory(key, sources)).collect();
    sort(&mut out);
    out
}

/// How the level of a device that doesn't say whether it charges went lately.
#[derive(Clone, Copy)]
struct Rise {
    last: u8,
    /// Steps up since it last went down, each within `STEADY` of the one before.
    steps: u8,
    /// When the last step up came.
    at: Instant,
}

/// The devices that don't say whether they charge charge while their level goes up step after
/// step: from the second step up, each within `STEADY` of the one before, until one doesn't come
/// in time, the level goes down or it's full. One step isn't enough: a battery at rest comes back
/// (an AA one too, which never charges), and a device charged while away comes back higher once.
fn rising(rises: &mut HashMap<String, Rise>, devices: &mut [Device], now: Instant) {
    for d in devices.iter_mut().filter(|d| !d.says_charging) {
        let Some(level) = d.level else { continue };
        let r = rises.entry(d.key.clone()).or_insert(Rise { last: level, steps: 0, at: now });
        let late = now.duration_since(r.at) > STEADY;
        if level < r.last {
            r.steps = 0;
        } else if level > r.last {
            r.steps = if r.steps > 0 && late { 1 } else { r.steps.saturating_add(1) };
            r.at = now;
        }
        r.last = level;
        d.charging = level < 100 && r.steps >= 2 && now.duration_since(r.at) <= STEADY;
    }
}

/// One power source of macOS's list.
struct Source {
    name: String,
    category: String,
    transport: String,
    vendor: u16,
    level: u8,
    /// None: it doesn't say.
    charging: Option<bool>,
    part: String,
}

fn accessory(key: String, sources: Vec<Source>) -> Device {
    let first = &sources[0];
    let parts: Vec<Part> = if sources.len() > 1 || sources.iter().any(|s| matches!(s.part.as_str(), "Left" | "Right" | "Case")) {
        let mut parts: Vec<(usize, Part)> = sources
            .iter()
            .map(|s| {
                let (order, label, main) = match s.part.as_str() {
                    "Left" => (0, t("Sinistro", "Left"), true),
                    "Right" => (1, t("Destro", "Right"), true),
                    "Case" => (2, t("Custodia", "Case"), false),
                    _ => (3, "", true),
                };
                (order, Part { label: label.to_string(), level: s.level, main })
            })
            .collect();
        parts.sort_by_key(|(o, _)| *o);
        parts.into_iter().map(|(_, p)| p).collect()
    } else {
        Vec::new()
    };
    let level = parts.iter().filter(|p| p.main).map(|p| p.level).min().or_else(|| parts.iter().map(|p| p.level).min()).unwrap_or(first.level);
    let earbuds = parts.iter().any(|p| p.label == t("Sinistro", "Left") || p.label == t("Destro", "Right"));
    let transport = first.transport.to_lowercase();
    Device {
        key: format!("macos:{key}"),
        name: first.name.clone(),
        kind: kind_of(&first.category),
        icon: icon(&first.category, &first.name, hid::maker(first.vendor), earbuds),
        maker: hid::maker(first.vendor),
        link: if transport.contains("bluetooth") {
            Link::Bluetooth
        } else if transport.contains("usb") {
            Link::Cable
        } else {
            Link::Unknown
        },
        level: Some(level),
        charging: sources.iter().filter(|s| s.part != "Case").any(|s| s.charging == Some(true)),
        says_charging: sources.iter().any(|s| s.charging.is_some()),
        parts,
        connected: true,
        asleep: false,
        read_at: None,
    }
}

/// All the sources, each device once. A maker's devices of one kind are told apart only by that:
/// while some are connected the others go (a keyboard on its receiver hides its Bluetooth pairing,
/// and the other way round); with none connected one stays, the first with a level, the
/// receiver's (fresher) before Bluetooth's. A ROG receiver's device takes its Bluetooth name.
fn combine(bluetooth: Vec<Device>, wired: &mut [Device]) -> Vec<Device> {
    let same = |a: &Device, b: &Device| a.maker == b.maker && a.maker != Maker::Other && a.kind == b.kind && a.kind != Kind::Other;
    for w in wired.iter_mut().filter(|w| w.name.is_empty()) {
        w.name = match bluetooth.iter().find(|b| same(b, w)) {
            Some(b) => b.name.clone(),
            None if w.kind == Kind::Mouse => t("Mouse ROG", "ROG mouse").to_string(),
            None => t("Tastiera ROG", "ROG keyboard").to_string(),
        };
    }
    let mut all = bluetooth;
    all.extend(wired.iter().cloned());
    let keep: Vec<bool> = (0..all.len())
        .map(|i| {
            let d = &all[i];
            if d.maker == Maker::Other || d.kind == Kind::Other {
                return true;
            }
            let group: Vec<usize> = (0..all.len()).filter(|j| same(&all[*j], d)).collect();
            if group.iter().any(|j| all[*j].connected) {
                return d.connected;
            }
            let rank = |j: usize| (all[j].level.is_some(), all[j].link != Link::Bluetooth);
            let best = group.iter().copied().fold(group[0], |b, j| if rank(j) > rank(b) { j } else { b });
            best == i
        })
        .collect();
    let mut out: Vec<Device> = all.into_iter().zip(keep).filter(|(_, k)| *k).map(|(d, _)| d).collect();
    sort(&mut out);
    out
}

/// Connected first, then by name.
fn sort(devices: &mut [Device]) {
    devices.sort_by_key(|d| (!d.connected, !d.asleep, d.name.to_lowercase()));
}

/// A kind of `pinned`.
fn kind_named(name: &str) -> Option<Kind> {
    match name.to_lowercase().as_str() {
        "mouse" | "mice" | "trackpad" => Some(Kind::Mouse),
        "keyboard" | "keyboards" => Some(Kind::Keyboard),
        "controller" | "controllers" | "gamepad" => Some(Kind::Controller),
        "other" => Some(Kind::Other),
        _ => None,
    }
}

/// The kind of an accessory's category (`Keyboard`, `Trackpad`, `Headset`…), or of the made-up
/// data's.
fn kind_of(category: &str) -> Kind {
    let c = category.to_lowercase();
    if c.contains("keyboard") {
        Kind::Keyboard
    } else if c.contains("mouse") || c.contains("trackpad") || c.contains("pointer") {
        Kind::Mouse
    } else if c.contains("gam") || c.contains("joystick") || c.contains("controller") {
        Kind::Controller
    } else {
        Kind::Other
    }
}

fn kind_icon(kind: Kind) -> &'static str {
    match kind {
        Kind::Mouse => "computermouse.fill",
        Kind::Keyboard => "keyboard.fill",
        Kind::Controller => "gamecontroller.fill",
        Kind::Other => "battery.75percent",
    }
}

/// The SF Symbol of an accessory: Apple's Magic Mouse, Trackpad and Keyboard their own (by
/// Apple's vendor ID, or the name if it hasn't one; the last two drawn by `gfx`), glasses that play
/// sound (a `Headset` for macOS) glasses, headphones with two parts earbuds, else its category's.
fn icon(category: &str, name: &str, maker: Maker, earbuds: bool) -> &'static str {
    let (c, n) = (category.to_lowercase(), name.to_lowercase());
    let magic = |what: &str| maker == Maker::Apple || n.contains(&format!("magic {what}"));
    if c.contains("trackpad") {
        return if magic("trackpad") { "magictrackpad.fill" } else { "rectangle.and.hand.point.up.left.fill" };
    }
    if n.contains("glasses") || n.contains("ray-ban") {
        return "eyeglasses";
    }
    match kind_of(category) {
        Kind::Mouse if magic("mouse") => return "magicmouse.fill",
        Kind::Keyboard if magic("keyboard") => return "magickeyboard.fill",
        Kind::Other => {}
        kind => return kind_icon(kind),
    }
    if c.contains("head") || c.contains("earbud") || c.contains("audio") {
        if earbuds { "earbuds" } else { "headphones" }
    } else if c.contains("speaker") {
        "hifispeaker.fill"
    } else if c.contains("pencil") || c.contains("stylus") {
        "applepencil"
    } else if c.contains("watch") {
        "applewatch"
    } else if c.contains("phone") {
        "iphone"
    } else {
        "battery.75percent"
    }
}

/// The maker as the popup names it.
fn maker_name(maker: Maker) -> Option<&'static str> {
    match maker {
        Maker::Apple => Some("Apple"),
        Maker::Razer => Some("Razer"),
        Maker::Logitech => Some("Logitech"),
        Maker::Asus => Some("ROG"),
        Maker::Valve => Some("Valve"),
        Maker::Sony => Some("Sony"),
        Maker::Microsoft => Some("Xbox"),
        Maker::Other => None,
    }
}

/// The name in the pill: the user's (`names`, by the full name), or the model's without its owner,
/// its maker and the words every device has: `Razer Naga V2 Pro` → `Naga`, `ROG AZOTH` →
/// `Azoth`, `MX Master 4` → `MX Master 4`, `Steam Controller 2` → `Steam 2`, `Magic Trackpad di
/// Anna` → `Magic Trackpad`.
fn short_name(name: &str, names: &HashMap<String, String>) -> String {
    if let Some(own) = names.get(&name.to_lowercase()) {
        return own.clone();
    }
    // macOS names a device after its owner: `AirPods Pro di Anna`, `Anna's Magic Mouse`
    let mut name = name.trim();
    for sep in ["'s ", "’s "] {
        if let Some(i) = name.find(sep) {
            name = &name[i + sep.len()..];
        }
    }
    for sep in [" di ", " de ", " du ", " von "] {
        if let Some(i) = name.find(sep).filter(|i| *i > 0) {
            name = &name[..i];
        }
    }
    const MAKERS: [&str; 9] = ["razer", "rog", "asus", "logitech", "logi", "sony", "microsoft", "valve", "apple"];
    const GENERIC: [&str; 6] = ["controller", "wireless", "gaming", "mouse", "keyboard", "headset"];
    let mut words: Vec<&str> = name.split_whitespace().collect();
    if words.is_empty() {
        return name.to_string();
    }
    while words.len() > 1 && MAKERS.contains(&words[0].to_lowercase().as_str()) {
        words.remove(0);
    }
    // Apple's `Magic` needs what it is: `Magic Keyboard`, `Magic Mouse`
    let magic = words[0].eq_ignore_ascii_case("magic");
    let kept: Vec<&str> = words.iter().copied().filter(|w| !GENERIC.contains(&w.to_lowercase().as_str())).collect();
    if !kept.is_empty() && !magic {
        words = kept;
    }
    // A number at the end tells two of a kind apart (or is the model's: `MX Master 4`)
    let number = words.last().filter(|w| w.len() == 1 && w.chars().all(|c| c.is_ascii_digit())).copied();
    let mut short: Vec<&str> = vec![words[0]];
    // A short first word, or Apple's `Magic`, says little alone
    let says_little = words[0].chars().count() <= 3 || words[0].eq_ignore_ascii_case("magic");
    if says_little && words.len() > 1 && Some(words[1]) != number {
        short.push(words[1]);
    }
    if let Some(n) = number
        && words.len() > short.len()
    {
        short.push(n);
    }
    // `AZOTH` as `Azoth`: a name in capitals shouts in a pill
    short
        .iter()
        .map(|w| {
            if w.chars().count() > 3 && w.chars().all(|c| c.is_ascii_uppercase()) {
                let lower = w.to_lowercase();
                lower[..1].to_uppercase() + &lower[1..]
            } else {
                w.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The palette color of a level: green above half, yellow down to a quarter, peach (orange) down to
/// a tenth, red under it.
fn level_name(level: u8) -> &'static str {
    match level {
        0..=10 => "red",
        11..=25 => "peach",
        26..=50 => "yellow",
        _ => "green",
    }
}

/// The color of a level's bar, and of its number: the text's while it's green, the bar's from
/// yellow down.
fn level_colors(th: &Theme, level: u8) -> (Rgba, Rgba) {
    let fill = match level_name(level) {
        "red" => th.red,
        "peach" => th.peach,
        "yellow" => th.yellow,
        _ => th.green,
    };
    (fill, if level > 50 { th.text } else { fill })
}

/// A device's colors: the level's bar and its number (`level_colors`), the icon and name, and the
/// bar's track; all dimmed while it isn't connected (asleep behind its receiver).
struct Paint {
    fill: Rgba,
    level: Rgba,
    text: Rgba,
    track: Rgba,
}

fn paint(th: &Theme, d: &Device) -> Paint {
    if d.connected {
        let (fill, level) = level_colors(th, d.level.unwrap_or(0));
        Paint { fill, level, text: th.text, track: th.text.faded(0.18) }
    } else {
        Paint { fill: th.subtext.faded(0.75), level: th.subtext, text: th.subtext, track: th.subtext.faded(0.25) }
    }
}

/// Under its name in the popup: the maker, how it's connected, whether it charges or sleeps
/// (`Razer · cavo · in carica`, `Logitech · non risponde · letto alle 00:58`).
fn details(d: &Device) -> String {
    let mut parts: Vec<String> = maker_name(d.maker).map(str::to_string).into_iter().collect();
    if d.asleep {
        parts.push(t("non risponde", "not answering").into());
        if let Some(at) = &d.read_at {
            parts.push(format!("{} {at}", t("letto alle", "read at")));
        }
    } else if !d.connected {
        parts.push(t("non collegato", "not connected").into());
    } else {
        let link = match d.link {
            Link::Bluetooth => Some("Bluetooth"),
            Link::Receiver => Some(t("ricevitore", "receiver")),
            Link::Cable => Some(t("cavo", "cable")),
            Link::Unknown => None,
        };
        parts.extend(link.map(str::to_string));
        if d.charging {
            parts.push(t("in carica", "charging").into());
        }
    }
    let line = parts.join(" · ");
    let mut chars = line.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// Its level as a number, or `–` if never read.
fn level_text(d: &Device) -> String {
    d.level.map_or_else(|| "–".to_string(), |l| format!("{l}%"))
}

// Made-up data

#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    devices: Vec<MockDevice>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct MockDevice {
    name: String,
    /// `mouse`, `keyboard`, `controller`, `trackpad`, `headphones`…
    category: String,
    /// `bluetooth`, `receiver`, `cable`.
    link: String,
    /// None: never read since the bar started.
    battery: Option<u8>,
    /// The other half of a split keyboard.
    second: Option<u8>,
    /// Earbuds: each one and the case.
    left: Option<u8>,
    right: Option<u8>,
    case: Option<u8>,
    charging: bool,
    connected: bool,
    asleep: bool,
    read: Option<String>,
}

impl MockDevice {
    fn device(self) -> Device {
        let mut parts = Vec::new();
        if let (Some(a), Some(b)) = (self.battery, self.second) {
            parts = vec![Part { label: format!("{} 1", t("Metà", "Half")), level: a, main: true }, Part { label: format!("{} 2", t("Metà", "Half")), level: b, main: true }];
        }
        for (level, label, main) in [(self.left, t("Sinistro", "Left"), true), (self.right, t("Destro", "Right"), true), (self.case, t("Custodia", "Case"), false)] {
            if let Some(l) = level {
                parts.push(Part { label: label.to_string(), level: l.min(100), main });
            }
        }
        let level = parts.iter().filter(|p| p.main).map(|p| p.level).min().or(self.battery).map(|l| l.min(100));
        let earbuds = self.left.is_some() || self.right.is_some();
        Device {
            key: format!("mock:{}", self.name.to_lowercase()),
            kind: kind_of(&self.category),
            icon: icon(&self.category, &self.name, Maker::Other, earbuds),
            maker: Maker::Other,
            link: match self.link.as_str() {
                "receiver" => Link::Receiver,
                "cable" => Link::Cable,
                "bluetooth" | "" => Link::Bluetooth,
                _ => Link::Unknown,
            },
            level,
            parts,
            charging: self.charging,
            says_charging: true,
            connected: self.connected,
            asleep: self.asleep,
            read_at: self.read,
            name: self.name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, kind: Kind, maker: Maker, link: Link, level: u8, connected: bool) -> Device {
        Device { key: name.to_lowercase(), name: name.into(), kind, icon: kind_icon(kind), maker, link, level: Some(level), parts: Vec::new(), charging: false, says_charging: true, connected, asleep: false, read_at: None }
    }

    fn widget() -> Devices {
        let mut d = Devices::new(&Options::default());
        d.mock = true;
        d
    }

    #[test]
    fn kinds_of_devices() {
        let other = Maker::Other;
        assert_eq!(icon("Keyboard", "ROG AZOTH", Maker::Asus, false), "keyboard.fill");
        assert_eq!(icon("Mouse", "Glide Pro", other, false), "computermouse.fill");
        assert_eq!(icon("Trackpad", "Studio Trackpad", other, false), "rectangle.and.hand.point.up.left.fill");
        assert_eq!(icon("Headset", "Aurora Buds", other, false), "headphones");
        assert_eq!(icon("Headphone", "Aurora Buds", other, true), "earbuds");
        assert_eq!(icon("Game Controller", "Arcade Pad", other, false), "gamecontroller.fill");
        assert_eq!(icon("", "Tag", other, false), "battery.75percent");
        // Apple's, by its vendor ID even when renamed, or by the name
        assert_eq!(icon("Trackpad", "Magic Trackpad di Anna", Maker::Apple, false), "magictrackpad.fill");
        assert_eq!(icon("Trackpad", "Magic Trackpad", other, false), "magictrackpad.fill");
        assert_eq!(icon("Mouse", "Scrivania", Maker::Apple, false), "magicmouse.fill");
        assert_eq!(icon("Keyboard", "Magic Keyboard con Touch ID", other, false), "magickeyboard.fill");
        // Glasses that play sound are a headset for macOS
        assert_eq!(icon("Headset", "Nimbus Audio Glasses 2 A1B2", other, false), "eyeglasses");
        assert_eq!(kind_of("Trackpad"), Kind::Mouse);
        assert_eq!(kind_of("Game Controller"), Kind::Controller);
        assert_eq!(kind_named("Mouse"), Some(Kind::Mouse));
        assert_eq!(kind_named("toaster"), None);
    }

    #[test]
    fn a_level_that_goes_up_is_charging() {
        let start = Instant::now();
        let mut rises = HashMap::new();
        let mut read = |minute: u64, level: u8, says: bool| {
            let mut d = device("ROG AZOTH", Kind::Keyboard, Maker::Asus, Link::Bluetooth, level, true);
            d.says_charging = says;
            let mut now = [d];
            rising(&mut rises, &mut now, start + Duration::from_secs(minute * 60));
            now[0].charging
        };
        // A battery at rest comes back once, even by much: not charging
        assert_eq!([(0, 21), (1, 22), (2, 21), (3, 30), (4, 25)].map(|(m, l)| read(m, l, false)), [false; 5]);
        // Steps up a few minutes apart: from the second, until it's full
        assert_eq!([(5, 26), (7, 28), (20, 40), (30, 99), (31, 100)].map(|(m, l)| read(m, l, false)), [false, true, true, true, false]);
        // A step that doesn't come in time: no more, and the next one alone isn't enough
        assert_eq!([(40, 50), (42, 52), (44, 54), (70, 54), (71, 55), (73, 57)].map(|(m, l)| read(m, l, false)), [false, false, true, false, false, true]);
        // Slowly over hours, as a battery that rests: never
        assert_eq!([(80, 40), (200, 41), (320, 42), (440, 43)].map(|(m, l)| read(m, l, false)), [false; 4]);
        // One that says whether it charges keeps what it says
        assert!(!read(441, 100, true));
    }

    #[test]
    fn earbuds_and_their_case() {
        let s = |part: &str, level: u8, charging: bool| Source { name: "Buds".into(), category: "Headphone".into(), transport: "Bluetooth".into(), vendor: 0x004C, level, charging: Some(charging), part: part.into() };
        let d = accessory("parts:Buds".into(), vec![s("Case", 60, true), s("Right", 22, false), s("Left", 15, false)]);
        let labels: Vec<&str> = d.parts.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, [t("Sinistro", "Left"), t("Destro", "Right"), t("Custodia", "Case")]);
        // The lower earbud, not the case; the case charging isn't the earbuds
        assert_eq!((d.level, d.charging, d.icon, d.maker, d.link), (Some(15), false, "earbuds", Maker::Apple, Link::Bluetooth));
        let single = accessory("id".into(), vec![s("Single", 81, false)]);
        assert_eq!((single.level, single.parts.len(), single.icon), (Some(81), 0, "headphones"));
    }

    #[test]
    fn one_device_on_two_links() {
        // A keyboard paired over Bluetooth but on its receiver now, a mouse of another maker
        let bluetooth = vec![device("Tempo Keys", Kind::Keyboard, Maker::Razer, Link::Bluetooth, 80, false), device("Glide", Kind::Mouse, Maker::Logitech, Link::Bluetooth, 50, false)];
        let mut wired = vec![device("Tempo Keys 75%", Kind::Keyboard, Maker::Razer, Link::Unknown, 94, true)];
        let out = combine(bluetooth, &mut wired);
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Tempo Keys 75%", "Glide"]);
        // The receiver alone while the keyboard is on Bluetooth
        let bluetooth = vec![device("Tempo Keys", Kind::Keyboard, Maker::Razer, Link::Bluetooth, 80, true)];
        let mut wired = vec![device("Tempo Keys 75%", Kind::Keyboard, Maker::Razer, Link::Unknown, 94, false)];
        assert_eq!(combine(bluetooth, &mut wired).len(), 1);
        // Gone on both sides: the one with a level, the receiver's first
        let bluetooth = vec![device("Tempo Keys", Kind::Keyboard, Maker::Razer, Link::Bluetooth, 3, false)];
        let mut never = device("Tempo Keys 75%", Kind::Keyboard, Maker::Razer, Link::Receiver, 0, false);
        never.level = None;
        let out = combine(bluetooth.clone(), &mut [never]);
        assert_eq!((out.len(), out[0].link), (1, Link::Bluetooth));
        let out = combine(bluetooth, &mut [device("Tempo Keys 75%", Kind::Keyboard, Maker::Razer, Link::Receiver, 90, false)]);
        assert_eq!((out.len(), out[0].level), (1, Some(90)));
        // Two connected of a kind both stay; so do two of another maker
        let mut pads = [device("Pad 1", Kind::Controller, Maker::Valve, Link::Receiver, 50, true), device("Pad 2", Kind::Controller, Maker::Valve, Link::Receiver, 60, true)];
        assert_eq!(combine(Vec::new(), &mut pads).len(), 2);
        let buds = vec![device("Buds", Kind::Other, Maker::Apple, Link::Bluetooth, 50, true), device("Old buds", Kind::Other, Maker::Apple, Link::Bluetooth, 70, false)];
        assert_eq!(combine(buds, &mut []).len(), 2);
    }

    #[test]
    fn a_rog_receiver_takes_the_bluetooth_name() {
        let bluetooth = vec![device("Split 75", Kind::Keyboard, Maker::Asus, Link::Bluetooth, 40, false)];
        let mut wired = vec![device("", Kind::Keyboard, Maker::Asus, Link::Receiver, 53, true), device("", Kind::Mouse, Maker::Asus, Link::Receiver, 70, true)];
        let out = combine(bluetooth, &mut wired);
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|d| d.name == "Split 75" && d.connected));
        assert!(out.iter().any(|d| d.kind == Kind::Mouse && !d.name.is_empty()));
    }

    #[test]
    fn what_the_pill_shows() {
        let mut d = widget();
        d.pinned = vec![Kind::Mouse, Kind::Keyboard];
        let mut split = device("Split", Kind::Keyboard, Maker::Asus, Link::Receiver, 48, true);
        split.parts = vec![Part { label: "1".into(), level: 53, main: true }, Part { label: "2".into(), level: 48, main: true }];
        let mut never = device("Never", Kind::Mouse, Maker::Logitech, Link::Receiver, 0, false);
        never.level = None;
        d.devices = vec![device("Mouse", Kind::Mouse, Maker::Razer, Link::Cable, 100, true), split, device("Speaker", Kind::Other, Maker::Other, Link::Bluetooth, 64, true), device("Buds", Kind::Other, Maker::Other, Link::Bluetooth, 15, true), never];
        let names: Vec<&str> = d.in_pill().iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Mouse", "Split", "Buds"]);
        assert_eq!(level_text(&d.devices[1]), "48%");
        assert_eq!(level_text(&d.devices[4]), "–");
        d.always = true;
        assert_eq!(d.in_pill().len(), 4);
    }

    #[test]
    fn a_keyboard_that_sleeps_stays_dimmed() {
        let mut d = widget();
        d.pinned = vec![Kind::Keyboard];
        let mut keys = Found { key: "razer:keys".into(), name: "Keys".into(), kind: Kind::Keyboard, maker: Maker::Razer, link: Link::Unknown, level: None, second: None, charging: false };
        let mut cx_timers = Vec::new();
        let mut cx = Cx { name: "devices", generation: 0, popup_open: false, peek_open: false, timers: &mut cx_timers, effects: Vec::new() };
        // Never read since the bar started: not in the pill
        d.wired = vec![keys.clone()];
        d.rebuild(&mut cx);
        assert!(d.in_pill().is_empty());
        // Read once, then asleep: in the pill with that level, dimmed, and "not answering"
        d.last.insert(keys.key.clone(), (64, None, "00:58".into()));
        d.rebuild(&mut cx);
        let shown = d.in_pill();
        assert_eq!((shown.len(), shown[0].level, shown[0].asleep, shown[0].connected), (1, Some(64), true, false));
        assert_eq!(details(shown[0]), format!("Razer · {} · {} 00:58", t("non risponde", "not answering"), t("letto alle", "read at")));
        // Awake again
        keys.level = Some(60);
        d.wired = vec![keys];
        d.rebuild(&mut cx);
        assert!(d.devices[0].connected && !d.devices[0].asleep);
    }

    #[test]
    fn what_a_tile_says() {
        let mut d = device("Mouse", Kind::Mouse, Maker::Razer, Link::Cable, 100, true);
        d.charging = true;
        assert_eq!(details(&d), format!("Razer · {} · {}", t("cavo", "cable"), t("in carica", "charging")));
        let buds = device("Buds", Kind::Other, Maker::Other, Link::Bluetooth, 64, true);
        assert_eq!(details(&buds), "Bluetooth");
        let gone = device("Pad", Kind::Controller, Maker::Other, Link::Bluetooth, 64, false);
        assert_eq!(details(&gone), capitalized(t("non collegato", "not connected")));
    }

    fn capitalized(s: &str) -> String {
        let mut c = s.chars();
        c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
    }

    #[test]
    fn colors_of_levels() {
        // A mouse at 25% is nearly flat: orange, not green
        let names: Vec<&str> = [100, 51, 50, 26, 25, 11, 10, 0].into_iter().map(level_name).collect();
        assert_eq!(names, ["green", "green", "yellow", "yellow", "peach", "peach", "red", "red"]);
    }

    #[test]
    fn short_names() {
        let none = HashMap::new();
        assert_eq!(short_name("Razer Naga V2 Pro", &none), "Naga");
        assert_eq!(short_name("Razer Huntsman V3 Pro 75%", &none), "Huntsman");
        assert_eq!(short_name("ROG AZOTH", &none), "Azoth");
        assert_eq!(short_name("MX Master 4", &none), "MX Master 4");
        assert_eq!(short_name("Steam Controller", &none), "Steam");
        assert_eq!(short_name("Steam Controller 2", &none), "Steam 2");
        assert_eq!(short_name("DualSense", &none), "DualSense");
        assert_eq!(short_name("S24 Ultra", &none), "S24 Ultra");
        assert_eq!(short_name("Magic Trackpad di Anna", &none), "Magic Trackpad");
        assert_eq!(short_name("Anna's Magic Keyboard", &none), "Magic Keyboard");
        assert_eq!(short_name("AirPods Pro de Ana", &none), "AirPods");
        assert_eq!(short_name("Nimbus Audio Glasses 2 A1B2", &none), "Nimbus");
        let own = HashMap::from([("razer naga v2 pro".to_string(), "Gioco".to_string())]);
        assert_eq!(short_name("Razer Naga V2 Pro", &own), "Gioco");
    }

    #[test]
    fn made_up_devices() {
        let m: Mock = serde_json::from_str(&std::fs::read_to_string("examples/mock/devices.json").unwrap()).unwrap();
        let devices: Vec<Device> = m.devices.into_iter().map(MockDevice::device).collect();
        assert!(devices.len() >= 6);
        assert!(devices.iter().any(|d| d.parts.len() == 3 && d.icon == "earbuds"));
        assert!(devices.iter().any(|d| d.asleep && d.read_at.is_some()));
        assert!(devices.iter().any(|d| !d.connected && !d.asleep && d.level.is_none()));
    }
}
