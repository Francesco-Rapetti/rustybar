//! The connection: in the bar the VPN, then the Wi-Fi network and its band, or Ethernet, or why
//! there's no connection (the icon yellow or red). The popup is a dashboard of tiles, the switches
//! and the networks on top and the metrics under them:
//!
//! - Wi-Fi, with ↻ for a new scan and its switch, then the nearby networks as tiles, the known
//!   ones first and the current one at the top, with band and security; a click joins one.
//! - The wired services, each with a switch.
//! - The connection, two tiles wide: the network, Ethernet, or why there's none, with the
//!   interface and its MTU; on Wi-Fi the signal in dBm and how good it is (Excellent from −55,
//!   Good from −67, Fair from −75, else Weak), on Ethernet the link speed.
//! - Download and upload: the speed now, the last 30 seconds as bars, and the total of the
//!   interface since it came up.
//! - On Wi-Fi: the link rate with the Wi-Fi generation, the channel with band and width, the
//!   signal-to-noise ratio with the noise, the security.
//! - Local IP (with the prefix), router (with its ping), public IP (with the latency to the
//!   internet), provider (with city and country), DNS, MAC address (private or not) and IPv6; a
//!   click copies an address, and the tile says Copied.
//! - The VPN, with its interface and address.
//!
//! The state comes from the System Configuration store, which tells when it changes, and from
//! CoreWLAN. Only while the popup is open, and in-process: every second the counters of the
//! interface (the routing sysctl) and the Wi-Fi link (CoreWLAN), every 5 seconds a ping to the
//! router (ICMP, without privileges). The public IP, its provider and the latency to the internet
//! (the TCP handshake of that request) come from one `curl` to ipinfo.io when the popup opens.
//! The network name comes from the last scan record the store keeps, as macOS gives it to CoreWLAN
//! only with the Location permission, which the list of nearby networks needs: RustyBar asks for
//! it the first time the popup opens.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAlert, NSApplication, NSImage, NSImageSymbolConfiguration, NSPasteboard, NSPasteboardTypeString, NSSecureTextField};
use objc2_core_location::CLLocationManager;
use objc2_core_wlan::{CWChannel, CWInterface, CWKeychainDomain, CWKeychainFindWiFiPassword, CWNetwork, CWSecurity, CWWiFiClient};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSKeyedUnarchiver, NSNumber, NSPoint, NSRect, NSSize, NSString, ns_string};
use serde::Deserialize;
use serde_json::Value;

use super::{Cx, Event, Handle, Rx, Widget};
use crate::config::Options;
use crate::gfx::{Rgba, Weight};
use crate::i18n::t;
use crate::iokit::{CFTypeRef, Dict, cf, owned};
use crate::ui::{Align, Justify, Kind, Node};

const TILE: f64 = 160.0;
const GAP: f64 = 8.0;
const WIDE: f64 = TILE * 2.0 + GAP;
/// Inside the padding of a tile.
const INNER: f64 = TILE - 20.0;
const HEADER: f64 = 56.0;
const TRAFFIC: f64 = 84.0;
const STAT: f64 = 54.0;
/// The VPN and the wired services.
const LINE: f64 = 50.0;
const NEARBY: f64 = 48.0;
/// Seconds of traffic in the graphs, a bar each.
const BARS: usize = 30;
const GRAPH: f64 = 24.0;
/// Below this the graphs don't grow: a quiet connection shows as quiet.
const FLOOR: f64 = 10_000.0;
/// Around the list of networks besides the tiles above it: the bar, the popup's padding, the
/// Wi-Fi title and the settings.
const AROUND: f64 = 150.0;

/// What the store says.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct State {
    /// None: the Mac has no Wi-Fi.
    wifi_device: Option<String>,
    wifi_power: bool,
    /// The interface of the connection, never a VPN tunnel.
    primary: Option<String>,
    ssid: Option<String>,
    /// "2.4", "5", "6"
    band: Option<String>,
    address: Option<String>,
    /// Of the subnet mask: 24 for 255.255.255.0.
    prefix: Option<u32>,
    router: Option<String>,
    /// The first global address of the connection.
    ipv6: Option<String>,
    ipv6_prefix: Option<u32>,
    /// The servers the Mac asks, a VPN's too.
    dns: Vec<String>,
    vpn: Option<String>,
    vpn_address: Option<String>,
    vpn_interface: Option<String>,
    wired: Vec<Wired>,
}

impl State {
    fn on_wifi(&self) -> bool {
        self.primary.is_some() && self.primary == self.wifi_device
    }

    /// What the public address is of: it changes with the connection.
    fn key(&self) -> String {
        format!("{}:{}:{}", self.primary.as_deref().unwrap_or(""), self.ssid.as_deref().unwrap_or(""), self.vpn.as_deref().unwrap_or(""))
    }
}

/// An Ethernet service: an adapter, or a phone shared over USB.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Wired {
    name: String,
    enabled: bool,
    connected: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Security {
    #[default]
    Open,
    Personal,
    /// Also wants a user name: System Settings asks for both.
    Enterprise,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Nearby {
    name: String,
    /// The bars of the Wi-Fi symbol in the macOS menu, 1 to 3.
    level: u8,
    security: Security,
    known: bool,
    /// Of the strongest access point: "2.4", "5", "6".
    band: String,
    /// "WPA3", "WPA2/3"…; empty for an open network.
    label: String,
}

/// The Wi-Fi link, from CoreWLAN.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Link {
    /// dBm
    rssi: i64,
    noise: i64,
    /// Mb/s
    rate: f64,
    channel: i64,
    band: String,
    /// MHz
    width: u32,
    /// The 802.11 amendment: "ax", "ac"…
    phy: String,
    /// A `CWSecurity`.
    security: i64,
}

/// The interface of the connection, from the routing sysctl.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Iface {
    mac: String,
    mtu: u32,
    /// Bits per second: what Ethernet negotiated.
    speed: u64,
}

/// The public side of the connection, from ipinfo.io.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Public {
    /// Empty when it couldn't be read.
    ip: String,
    provider: String,
    city: String,
    country: String,
    /// Milliseconds, the TCP handshake with ipinfo.io.
    latency: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Permission {
    Unknown,
    Asking,
    Allowed,
    Denied,
}

#[derive(Clone, Debug, PartialEq)]
enum Joined {
    Done,
    Failed,
    /// Not in range.
    Missing,
    Denied,
    Enterprise,
    /// Wrong (or no) password: ask for one.
    Password { wrong: bool },
}

/// Made-up data (see `mock`): a State, and what the popup reads.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    #[serde(flatten)]
    state: State,
    networks: Vec<Nearby>,
    public: Public,
    link: Link,
    interface: Iface,
    /// Milliseconds to the router.
    ping: f64,
    received: u64,
    sent: u64,
    /// Bytes per second down and up, one a second, over and over while the popup is open.
    traffic: Vec<(f64, f64)>,
}

pub struct Network {
    state: Option<State>,
    networks: Vec<Nearby>,
    permission: Permission,
    location: Option<Retained<CLLocationManager>>,
    /// The public side and the connection it's of.
    public: Option<(Public, String)>,
    fetching: bool,
    scanning: bool,
    /// The network being joined, and since when: one at a time, for at most 2 minutes.
    joining: Option<(String, f64)>,
    failed: Option<String>,
    store: Option<Store>,
    changes: u64,
    reading: bool,
    again: bool,
    // While the popup is open
    link: Option<Link>,
    iface: Option<Iface>,
    /// Milliseconds to the router; None while there's no answer.
    ping: Option<f64>,
    pinging: bool,
    /// Bumped when the popup opens or closes: the readings of an earlier one stop.
    ticking: u64,
    ticks: u64,
    /// The interface, its counters and when they were read, for the next speed.
    last: Option<(String, u64, u64, Instant)>,
    /// Received and sent since the interface came up.
    totals: Option<(u64, u64)>,
    /// Bytes per second down and up, oldest first, each with its number: the key of its bar.
    samples: VecDeque<(u64, f64, f64)>,
    counted: u64,
    /// The address just copied, for a moment.
    copied: Option<String>,
    copies: u64,
    mock: Option<Mock>,
}

fn now() -> f64 {
    objc2_foundation::NSDate::now().timeIntervalSince1970()
}

fn number(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if crate::i18n::italian() { s.replace('.', ",") } else { s }
}

/// `4,2 GB`, `610 MB`, `12 KB`: decimal units, as Activity Monitor.
fn bytes(b: f64) -> String {
    match b {
        1e9.. => format!("{} GB", number(b / 1e9, if b < 1e10 { 1 } else { 0 })),
        1e6.. => format!("{} MB", number(b / 1e6, if b < 1e7 { 1 } else { 0 })),
        1e3.. => format!("{} KB", number(b / 1e3, 0)),
        _ => format!("{b:.0} B"),
    }
}

/// `1 Gb/s`, `2,5 Gb/s`, `866 Mb/s`.
fn bits(b: f64) -> String {
    if b >= 1e9 { format!("{} Gb/s", number(b / 1e9, if (b / 1e8).round() % 10.0 == 0.0 { 0 } else { 1 })) } else { format!("{} Mb/s", number(b / 1e6, 0)) }
}

fn dbm(v: i64) -> String {
    format!("{}{} dBm", if v < 0 { "−" } else { "" }, v.abs())
}

fn ms(v: f64) -> String {
    if v < 1.0 { "< 1 ms".into() } else { format!("{v:.0} ms") }
}

fn band(b: &str) -> String {
    if crate::i18n::italian() { format!("{} GHz", b.replacen('.', ",", 1)) } else { format!("{b} GHz") }
}

/// How good a signal is, by its dBm.
fn quality(rssi: i64) -> (&'static str, u8) {
    match rssi {
        -55.. => (t("Ottimo", "Excellent"), 3),
        -67.. => (t("Buono", "Good"), 3),
        -75.. => (t("Discreto", "Fair"), 2),
        _ => (t("Debole", "Weak"), 1),
    }
}

/// The variable value of the `wifi` symbol for 1 to 3 bars.
fn wifi_symbol(level: u8) -> String {
    let value = match level {
        3.. => 1.0,
        2 => 0.5,
        _ => 0.1,
    };
    format!("wifi@{value}")
}

/// `Wi-Fi 6E` for 802.11ax on 6 GHz; the amendment for those before 802.11n.
fn generation(phy: &str, band: &str) -> String {
    match phy {
        "n" => "Wi-Fi 4".into(),
        "ac" => "Wi-Fi 5".into(),
        "ax" if band == "6" => "Wi-Fi 6E".into(),
        "ax" => "Wi-Fi 6".into(),
        "be" => "Wi-Fi 7".into(),
        "" => String::new(),
        other => format!("802.11{other}"),
    }
}

/// A `CWSecurity` as the protocol and the kind of it.
fn security_name(security: i64) -> (String, &'static str) {
    let (personal, enterprise, open) = (t("Personale", "Personal"), "Enterprise", t("Aperta", "Open"));
    let (name, kind) = match security {
        0 => (open, ""),
        1 => ("WEP", ""),
        2 => ("WPA", personal),
        3 => ("WPA/WPA2", personal),
        4 => ("WPA2", personal),
        5 => (personal, ""),
        6 => ("WEP", "802.1X"),
        7 => ("WPA", enterprise),
        8 => ("WPA/WPA2", enterprise),
        9 => ("WPA2", enterprise),
        10 => (enterprise, ""),
        11 => ("WPA3", personal),
        12 => ("WPA3", enterprise),
        13 => ("WPA2/WPA3", personal),
        14 | 15 => ("OWE", open),
        _ => ("—", ""),
    };
    (name.to_string(), kind)
}

/// Whether a MAC address is a private one: macOS makes them up locally administered.
fn private(mac: &str) -> bool {
    u8::from_str_radix(mac.get(..2).unwrap_or("0"), 16).is_ok_and(|b| b & 0x02 != 0)
}

/// A tile of the dashboard: a label with a note on its right, and the value under them.
fn stat(rx: &Rx, label: &str, note: Node, value: Node, width: f64) -> Node {
    let th = rx.theme;
    // The note takes what the label leaves
    let room = width - 20.0 - crate::gfx::measure_text(label, &th.small_font()).width - 6.0;
    Node::column(vec![
        Node::row(vec![Node::text(label, th.small_font(), th.subtext).key("label"), Node::spacer(), note.max_width(room).key("note")]).gap(6.0).height(14.0).key("top"),
        value.max_width(width - 20.0).key("value"),
    ])
    .gap(4.0)
    .padding(9.0, 10.0, 9.0, 10.0)
    .size(width, STAT)
    .radius(10.0)
    .background(th.tile(th.primary, false))
}

/// A tile two wide with an icon: the VPN, a wired service. `right` is as wide as `room` says.
fn line(rx: &Rx, symbol: &str, color: Rgba, title: &str, detail: Node, (right, room): (Node, f64), lit: bool) -> Node {
    let th = rx.theme;
    // The padding, the icon and the gaps
    let names = WIDE - 22.0 - 26.0 - 30.0 - room;
    let row = vec![
        Node::row(vec![Node::symbol(symbol, 15.0, Weight::Semibold, color)]).width(26.0).justify(Justify::Center).key("icon"),
        Node::column(vec![Node::text(title, th.font(), th.text).max_width(names).key("name"), detail.max_width(names).key("detail")]).gap(2.0).key("names"),
        Node::spacer(),
        right,
    ];
    Node::row(row).gap(10.0).padding(8.0, 12.0, 8.0, 10.0).size(WIDE, LINE).radius(10.0).background(th.tile(color, lit))
}

impl Network {
    pub fn new(_options: &Options) -> Network {
        let mock = crate::mock::load::<Mock>("network");
        Network {
            state: None,
            networks: mock.as_ref().map(|m| m.networks.clone()).unwrap_or_default(),
            permission: if mock.is_some() { Permission::Allowed } else { Permission::Unknown },
            location: None,
            public: None,
            fetching: false,
            scanning: false,
            joining: None,
            failed: None,
            store: None,
            changes: 0,
            reading: false,
            again: false,
            link: None,
            iface: None,
            ping: None,
            pinging: false,
            ticking: 0,
            ticks: 0,
            last: None,
            totals: None,
            samples: VecDeque::new(),
            counted: 0,
            copied: None,
            copies: 0,
            mock,
        }
    }

    fn read(&mut self, cx: &mut Cx) {
        if let Some(mock) = &self.mock {
            self.state = Some(mock.state.clone());
            cx.redraw();
            return;
        }
        if self.reading {
            self.again = true;
            return;
        }
        self.reading = true;
        cx.background::<Network, State>(read_state, |w, state, cx| {
            w.reading = false;
            if w.state.as_ref() != Some(&state) {
                // Joined: the network is now the current one
                if state.ssid.is_some() && w.joining.as_ref().map(|j| &j.0) == state.ssid.as_ref() {
                    w.joining = None;
                }
                let old = w.state.as_ref();
                if old.map(|s| &s.router) != Some(&state.router) {
                    w.ping = None;
                }
                if old.map(|s| &s.primary) != Some(&state.primary) {
                    w.link = None;
                    w.iface = None;
                }
                w.state = Some(state);
                cx.redraw();
            }
            if std::mem::take(&mut w.again) {
                w.read(cx);
            }
        });
    }

    /// The store sends bursts: one read for each.
    fn changed(&mut self, cx: &mut Cx) {
        self.changes += 1;
        let changes = self.changes;
        cx.after::<Network>(0.3, move |w, cx| {
            if w.changes == changes {
                w.read(cx);
            }
        });
    }

    fn fetch_public(&mut self, cx: &mut Cx) {
        if self.fetching {
            return;
        }
        if let Some(mock) = &self.mock {
            self.public = Some((mock.public.clone(), mock.state.key()));
            return;
        }
        let Some(key) = self.state.as_ref().map(State::key) else { return };
        self.fetching = true;
        // Through the VPN, its own
        cx.background::<Network, Public>(public, move |w, public, cx| {
            w.fetching = false;
            w.public = Some((public, key));
            cx.redraw();
        });
    }

    /// Every second while the popup is open: the speeds and the Wi-Fi link; every 5, the ping.
    fn tick(&mut self, ticking: u64, cx: &mut Cx) {
        if ticking != self.ticking || !cx.popup_open {
            return;
        }
        let next = move |cx: &mut Cx| cx.after::<Network>(1.0, move |w, cx| w.tick(ticking, cx));
        if let Some(mock) = &self.mock {
            let sample = mock.traffic.get(self.counted as usize % mock.traffic.len().max(1)).copied();
            let (received, sent) = self.totals.unwrap_or((mock.received, mock.sent));
            if let Some((down, up)) = sample {
                self.push(down, up);
                self.totals = Some((received + down as u64, sent + up as u64));
            }
            cx.redraw();
            next(cx);
            return;
        }
        let Some(s) = self.state.clone() else {
            next(cx);
            return;
        };
        if self.ticks.is_multiple_of(5) {
            self.ping_router(cx);
        }
        self.ticks += 1;
        let (name, wifi) = (s.primary.clone(), s.on_wifi());
        cx.background::<Network, (Option<(Iface, u64, u64)>, Option<Link>)>(
            move || (name.as_deref().and_then(counters), if wifi { link() } else { None }),
            move |w, (counted, link), cx| {
                if ticking != w.ticking {
                    return;
                }
                w.link = link;
                match (counted, &s.primary) {
                    (Some((iface, received, sent)), Some(name)) => {
                        w.count(name, received, sent);
                        w.iface = Some(iface);
                    }
                    _ => w.last = None,
                }
                cx.redraw();
                next(cx);
            },
        );
    }

    /// The speeds since the last counters, unless the interface changed or started again.
    fn count(&mut self, name: &str, received: u64, sent: u64) {
        let now = Instant::now();
        match &self.last {
            Some((last, r, s, at)) if last == name && received >= *r && sent >= *s => {
                let secs = now.duration_since(*at).as_secs_f64().max(0.2);
                self.push((received - r) as f64 / secs, (sent - s) as f64 / secs);
            }
            Some(_) => self.samples.clear(),
            None => {}
        }
        self.last = Some((name.to_string(), received, sent, now));
        self.totals = Some((received, sent));
    }

    fn push(&mut self, down: f64, up: f64) {
        self.counted += 1;
        self.samples.push_back((self.counted, down, up));
        while self.samples.len() > BARS {
            self.samples.pop_front();
        }
    }

    fn ping_router(&mut self, cx: &mut Cx) {
        let Some(router) = self.state.as_ref().and_then(|s| s.router.clone()) else { return };
        if self.pinging {
            return;
        }
        self.pinging = true;
        cx.background::<Network, Option<f64>>(
            move || ping(&router),
            |w, ms, cx| {
                w.pinging = false;
                if w.ping != ms {
                    w.ping = ms;
                    cx.redraw();
                }
            },
        );
    }

    fn copy(&mut self, what: &str, cx: &mut Cx) {
        let Some(s) = &self.state else { return };
        let text = match what {
            "address" => s.address.clone(),
            "router" => s.router.clone(),
            "public" => self.public.as_ref().map(|(p, _)| p.ip.clone()).filter(|ip| !ip.is_empty()),
            "dns" => s.dns.first().cloned(),
            "mac" => self.iface.as_ref().map(|i| i.mac.clone()).filter(|m| !m.is_empty()),
            "ipv6" => s.ipv6.clone(),
            "vpn" => s.vpn_address.clone(),
            _ => None,
        };
        let Some(text) = text else { return };
        if self.mock.is_none() {
            let pasteboard = NSPasteboard::generalPasteboard();
            pasteboard.clearContents();
            pasteboard.setString_forType(&NSString::from_str(&text), unsafe { NSPasteboardTypeString });
        }
        self.copied = Some(what.to_string());
        self.copies += 1;
        let copies = self.copies;
        cx.redraw();
        cx.after::<Network>(1.5, move |w, cx| {
            if w.copies == copies {
                w.copied = None;
                cx.redraw();
            }
        });
    }

    /// Location Services answers a permission only to an app, and RustyBar asks the first time
    /// the list is wanted. The answer takes a few seconds.
    fn ask_location(&mut self, cx: &mut Cx) {
        if self.mock.is_some() {
            return;
        }
        let manager = self.location.get_or_insert_with(|| unsafe { CLLocationManager::new() });
        match unsafe { manager.authorizationStatus() }.0 {
            0 => {
                if self.permission != Permission::Asking {
                    self.permission = Permission::Asking;
                    unsafe { manager.requestWhenInUseAuthorization() };
                    self.await_location(0, cx);
                }
            }
            1 | 2 => {
                if self.permission != Permission::Denied {
                    crate::log("location: no access (System Settings → Privacy & Security → Location Services)");
                }
                self.permission = Permission::Denied;
            }
            _ => {
                self.permission = Permission::Allowed;
                self.scan(cx);
            }
        }
    }

    fn await_location(&mut self, tries: u32, cx: &mut Cx) {
        let Some(manager) = &self.location else { return };
        let status = unsafe { manager.authorizationStatus() }.0;
        if status != 0 {
            self.permission = Permission::Unknown;
            self.ask_location(cx);
            cx.redraw();
            return;
        }
        // In case the prompt is only for an app in front
        if tries == 10
            && let Some(mtm) = MainThreadMarker::new()
        {
            #[allow(deprecated)]
            NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
            unsafe { manager.requestWhenInUseAuthorization() };
        }
        if tries < 60 {
            cx.after::<Network>(1.0, move |w, cx| w.await_location(tries + 1, cx));
        } else {
            self.permission = Permission::Unknown;
        }
    }

    /// The networks macOS saw last, at once, then a new scan, which takes a few seconds.
    fn scan(&mut self, cx: &mut Cx) {
        if self.scanning || self.permission != Permission::Allowed || !self.state.as_ref().is_some_and(|s| s.wifi_power) || self.mock.is_some() {
            return;
        }
        self.scanning = true;
        cx.background::<Network, Vec<Nearby>>(|| nearby(false), |w, networks, cx| {
            w.networks = networks;
            cx.redraw();
            cx.background::<Network, Vec<Nearby>>(
                || nearby(true),
                |w, networks, cx| {
                    w.scanning = false;
                    w.networks = networks;
                    cx.redraw();
                },
            );
        });
    }

    fn join(&mut self, name: String, cx: &mut Cx) {
        // One network at a time: the clicks meanwhile are ignored
        if self.joining.as_ref().is_some_and(|(_, since)| now() - since < 120.0) || self.mock.is_some() {
            return;
        }
        self.joining = Some((name.clone(), now()));
        self.failed = None;
        cx.redraw();
        let allowed = self.permission == Permission::Allowed;
        let target = name.clone();
        cx.background::<Network, Joined>(move || try_join(&target, allowed, None), move |w, joined, cx| w.joined(name, joined, cx));
    }

    fn joined(&mut self, name: String, joined: Joined, cx: &mut Cx) {
        match joined {
            Joined::Password { wrong } => {
                // A modal dialog: the bar waits while it's up
                let Some(password) = ask_password(&name, wrong) else {
                    self.joining = None;
                    cx.redraw();
                    return;
                };
                let target = name.clone();
                cx.background::<Network, Joined>(move || try_join(&target, true, Some(password)), move |w, joined, cx| w.joined(name, joined, cx));
                return;
            }
            Joined::Done => {}
            Joined::Enterprise => {
                cx.close_popup();
                crate::runtime::open("x-apple.systempreferences:com.apple.wifi-settings-extension");
            }
            Joined::Failed | Joined::Missing | Joined::Denied => self.failed = Some(name),
        }
        self.joining = None;
        self.read(cx);
        cx.redraw();
    }

    fn networksetup(&mut self, args: Vec<String>, cx: &mut Cx) {
        if self.mock.is_some() {
            return;
        }
        cx.background::<Network, ()>(
            move || {
                let _ = std::process::Command::new("/usr/sbin/networksetup").args(&args).status();
            },
            |w, _, cx| {
                w.read(cx);
                w.scan(cx);
            },
        );
    }

    // Drawing

    /// The connection, two tiles wide.
    fn header(&self, rx: &Rx, s: &State) -> Node {
        let th = rx.theme;
        let link = self.link.as_ref().filter(|_| s.on_wifi());
        let iface = self.iface.as_ref().filter(|_| s.primary.is_some());
        let (icon, color, title) = if s.on_wifi() {
            (wifi_symbol(link.map(|l| quality(l.rssi).1).unwrap_or(3)), th.primary, s.ssid.clone().unwrap_or_else(|| "Wi-Fi".into()))
        } else if s.primary.is_some() {
            ("cable.connector.horizontal".into(), th.primary, "Ethernet".into())
        } else if s.wifi_device.is_none() {
            ("network.slash".into(), th.red, "Offline".into())
        } else if s.wifi_power {
            ("wifi.exclamationmark".into(), th.yellow, t("Non connesso", "Not Connected").into())
        } else {
            ("wifi.slash".into(), th.red, t("Wi-Fi off", "Wi-Fi Off").into())
        };
        let detail = match (&s.primary, iface) {
            (Some(p), Some(i)) if i.mtu > 0 => format!("{p} · MTU {}", i.mtu),
            (Some(p), _) => p.clone(),
            (None, _) if s.wifi_device.is_none() => t("Nessuna connessione", "No connection").into(),
            (None, _) if s.wifi_power => t("Scegli una rete qui sotto", "Choose a network below").into(),
            (None, _) => t("Attiva il Wi-Fi qui sotto", "Turn on Wi-Fi below").into(),
        };
        let mut row = vec![
            Node::row(vec![Node::symbol(icon, 18.0, Weight::Semibold, color)]).width(26.0).justify(Justify::Center).key("icon"),
            Node::column(vec![Node::text(title, th.font(), th.text).max_width(WIDE - 150.0).key("name"), Node::text(detail, th.small_font(), th.subtext).max_width(WIDE - 150.0).key("detail")])
                .gap(2.0)
                .key("names"),
            Node::spacer(),
        ];
        let right = match (link, iface) {
            (Some(l), _) => {
                let (word, level) = quality(l.rssi);
                Some((dbm(l.rssi), word, match level {
                    3 => th.green,
                    2 => th.yellow,
                    _ => th.red,
                }))
            }
            (None, Some(i)) if i.speed > 0 && !s.on_wifi() => Some((bits(i.speed as f64), t("Velocità", "Speed"), th.subtext)),
            _ => None,
        };
        if let Some((value, label, color)) = right {
            row.push(
                Node::column(vec![Node::text(value, th.font_with(Weight::Medium, 15.0), th.text).key("value"), Node::text(label, th.small_font(), color).key("label")])
                    .gap(2.0)
                    .align(Align::End)
                    .key("right"),
            );
        }
        Node::row(row).gap(10.0).padding(10.0, 12.0, 10.0, 10.0).size(WIDE, HEADER).radius(10.0).background(th.tile(color, true)).key("connection")
    }

    /// Download or upload: the speed, the total and the last seconds as bars, the newest right.
    fn traffic(&self, rx: &Rx, down: bool) -> Node {
        let th = rx.theme;
        let color = if down { th.primary } else { th.green };
        let values: Vec<(u64, f64)> = self.samples.iter().map(|&(n, d, u)| (n, if down { d } else { u })).collect();
        let max = values.iter().map(|v| v.1).fold(FLOOR, f64::max);
        let width = (INNER - (BARS - 1) as f64 * 1.5) / BARS as f64;
        let graph = values.iter().map(|&(n, v)| Node::default().size(width, (2.0 + (GRAPH - 2.0) * v / max).round()).radius(1.0).background(color).key(format!("bar-{n}"))).collect();
        let total = self.totals.map(|(r, s)| bytes((if down { r } else { s }) as f64)).unwrap_or_default();
        let speed = values.last().map(|v| format!("{}/s", bytes(v.1))).unwrap_or_else(|| "…".into());
        Node::column(vec![
            Node::row(vec![
                Node::symbol(if down { "arrow.down" } else { "arrow.up" }, 10.0, Weight::Bold, color).key("icon"),
                Node::text(if down { "Download" } else { "Upload" }, th.small_font(), th.subtext).key("label"),
                Node::spacer(),
                Node::text(total, th.small_font(), th.subtext).key("total"),
            ])
            .gap(4.0)
            .height(14.0)
            .key("top"),
            Node::text(speed, th.font_with(Weight::Medium, 15.0), th.text).key("speed"),
            Node::row(graph).gap(1.5).justify(Justify::End).align(Align::End).height(GRAPH).key("graph"),
        ])
        .gap(4.0)
        .padding(9.0, 10.0, 9.0, 10.0)
        .size(TILE, TRAFFIC)
        .radius(10.0)
        .background(th.tile(color, false))
        .key(if down { "download" } else { "upload" })
    }

    /// Rate, channel, signal-to-noise and security of the Wi-Fi link.
    fn link_tiles(&self, rx: &Rx, l: &Link) -> Vec<Node> {
        let th = rx.theme;
        let value = |text: String, color: Rgba| Node::text(text, th.font_with(Weight::Medium, 14.0), color);
        let note = |text: String| Node::text(text, th.small_font(), th.subtext);
        let snr = l.rssi - l.noise;
        let mut channel = vec![band(&l.band)];
        if l.width > 0 {
            channel.push(format!("{} MHz", l.width));
        }
        let (protocol, kind) = security_name(l.security);
        vec![
            stat(rx, t("Velocità", "Link Rate"), note(generation(&l.phy, &l.band)), value(bits(l.rate * 1e6), th.text), TILE).key("rate"),
            stat(rx, t("Canale", "Channel"), note(channel.join(" · ")), value(l.channel.to_string(), th.text), TILE).key("channel"),
            stat(rx, "SNR", note(dbm(l.noise)), value(format!("{snr} dB"), th.level(-snr as f64, -25.0, -15.0, th.text)), TILE).key("snr"),
            stat(rx, t("Sicurezza", "Security"), note(kind.to_string()), value(protocol, th.text), TILE).key("security"),
        ]
    }

    /// The addresses, which a click copies; IPv6 two tiles wide, after the others.
    fn address_tiles(&self, rx: &Rx, s: &State) -> (Vec<Node>, Option<Node>) {
        let th = rx.theme;
        let value = |text: &str, color: Rgba| Node::text(text, th.font_with(Weight::Medium, 14.0), color);
        let note = |what: &str, text: String, color: Rgba| {
            if self.copied.as_deref() == Some(what) { Node::text(t("Copiato", "Copied"), th.small_font(), th.green) } else { Node::text(text, th.small_font(), color) }
        };
        let slow = |ms: f64, warn: f64| if ms >= warn { th.yellow } else { th.subtext };
        let copy = |tile: Node, what: &str| tile.click(rx.action(&format!("copy:{what}"))).key(what);
        let mut tiles = Vec::new();
        if let Some(address) = &s.address {
            let prefix = s.prefix.map(|p| format!("/{p}")).unwrap_or_default();
            tiles.push(copy(stat(rx, t("IP locale", "Local IP"), note("address", prefix, th.subtext), value(address, th.text), TILE), "address"));
        }
        if let Some(router) = &s.router {
            let ping = self.ping.map(|p| (ms(p), slow(p, 100.0))).unwrap_or((String::new(), th.subtext));
            tiles.push(copy(stat(rx, "Router", note("router", ping.0, ping.1), value(router, th.text), TILE), "router"));
        }
        let stale = self.public.as_ref().is_some_and(|(_, key)| *key != s.key());
        match self.public.as_ref().map(|(p, _)| p) {
            Some(p) if !p.ip.is_empty() => {
                let latency = p.latency.map(|l| (ms(l), slow(l, 150.0))).unwrap_or((String::new(), th.subtext));
                let color = if stale { th.overlay } else { th.text };
                tiles.push(copy(stat(rx, t("IP pubblico", "Public IP"), note("public", latency.0, latency.1), value(&p.ip, color), TILE), "public"));
                if !p.provider.is_empty() {
                    let place = [p.city.as_str(), p.country.as_str()].iter().filter(|x| !x.is_empty()).copied().collect::<Vec<_>>().join(", ");
                    tiles.push(stat(rx, "Provider", note("provider", place, th.subtext), value(&p.provider, color), TILE).key("provider"));
                }
            }
            Some(_) => tiles.push(stat(rx, t("IP pubblico", "Public IP"), Node::default(), value(t("Non disponibile", "Not Available"), th.subtext), TILE).key("public")),
            None => tiles.push(stat(rx, t("IP pubblico", "Public IP"), Node::default(), value("…", th.subtext), TILE).key("public")),
        }
        if let Some(first) = s.dns.first() {
            let more = if s.dns.len() > 1 { format!("+{}", s.dns.len() - 1) } else { String::new() };
            tiles.push(copy(stat(rx, "DNS", note("dns", more, th.subtext), value(first, th.text), TILE), "dns"));
        }
        if let Some(mac) = self.iface.as_ref().map(|i| &i.mac).filter(|m| !m.is_empty()) {
            let kind = if private(mac) { t("Privato", "Private") } else { "" };
            tiles.push(copy(stat(rx, t("Indirizzo MAC", "MAC Address"), note("mac", kind.to_string(), th.subtext), value(mac, th.text), TILE), "mac"));
        }
        let ipv6 = s.ipv6.as_ref().map(|a| {
            let prefix = s.ipv6_prefix.map(|p| format!("/{p}")).unwrap_or_default();
            copy(stat(rx, "IPv6", note("ipv6", prefix, th.subtext), value(a, th.text), WIDE), "ipv6")
        });
        (tiles, ipv6)
    }

    /// A nearby network: its signal, name and band with security, or what joining it is at.
    fn nearby_tile(&self, rx: &Rx, n: &Nearby, current: bool, joining: Option<&str>) -> Node {
        let th = rx.theme;
        let mut detail: Vec<String> = Vec::new();
        if !n.band.is_empty() {
            detail.push(band(&n.band));
        }
        detail.push(match n.security {
            Security::Open => t("Aperta", "Open").into(),
            Security::Enterprise => "Enterprise".into(),
            Security::Personal if n.label.is_empty() => t("Protetta", "Secured").into(),
            Security::Personal => n.label.clone(),
        });
        let (text, color) = if current {
            (t("Connessa", "Connected").to_string(), th.primary)
        } else if joining == Some(n.name.as_str()) {
            (t("Connessione…", "Connecting…").to_string(), th.yellow)
        } else if self.failed.as_deref() == Some(n.name.as_str()) {
            (t("Non riuscito", "Failed").to_string(), th.red)
        } else {
            (detail.join(" · "), th.subtext)
        };
        let names = TILE - 16.0 - 26.0 - 8.0;
        let tile = Node::row(vec![
            Node::row(vec![Node::symbol(wifi_symbol(n.level), 15.0, Weight::Semibold, if current { th.primary } else { th.text })]).width(26.0).justify(Justify::Center).key("icon"),
            Node::column(vec![Node::text(&n.name, th.font(), th.text).max_width(names).key("name"), Node::text(text, th.small_font(), color).max_width(names).key("detail")]).gap(2.0).key("names"),
        ])
        .gap(8.0)
        .padding(8.0, 8.0, 8.0, 8.0)
        .size(TILE, NEARBY)
        .radius(10.0)
        .background(th.tile(th.primary, current))
        .key(format!("net:{}", n.name));
        if current { tile } else { tile.click(rx.action(&format!("join:{}", n.name))) }
    }

    /// Wi-Fi with ↻ and its switch, then the networks to join.
    fn wifi(&self, rx: &Rx, s: &State, rows: &mut Vec<Node>, used: f64) {
        let th = rx.theme;
        let mut title = vec![Node::text("Wi-Fi", th.font(), th.text).key("name")];
        if s.wifi_power && self.scanning {
            title.push(Node::text(t("Ricerca…", "Searching…"), th.small_font(), th.subtext).key("note"));
        }
        title.push(Node::spacer());
        if s.wifi_power && self.permission == Permission::Allowed {
            let mut button = Node::row(vec![Node::symbol("arrow.clockwise", 10.0, Weight::Bold, if self.scanning { th.overlay } else { th.text })])
                .size(28.0, 22.0)
                .justify(Justify::Center)
                .radius(6.0)
                .background(th.surface.faded(if self.scanning { 0.35 } else { 0.7 }))
                .key("scan");
            if !self.scanning {
                button = button.click(rx.action("scan"));
            }
            title.push(button);
        }
        title.push(Node::new(Kind::Toggle { on: s.wifi_power, color: th.primary, track: th.surface }).click(rx.action("wifi")).key("toggle"));
        rows.push(Node::row(title).gap(6.0).height(26.0).padding(4.0, 4.0, 0.0, 10.0).key("wifi-title"));
        if !s.wifi_power {
            return;
        }
        let networks = if self.permission == Permission::Allowed { &self.networks[..] } else { &[] };
        if self.permission == Permission::Denied {
            rows.push(rx.row_button(vec![Node::text(t("Consenti la posizione per vedere le reti", "Allow Location to see the networks"), th.small_font(), th.primary)], vec![], rx.action("location")).key("location"));
            return;
        }
        if networks.is_empty() {
            rows.push(Node::row(vec![Node::text(t("Ricerca reti…", "Looking for networks…"), th.small_font(), th.subtext)]).height(20.0).padding(0.0, 0.0, 0.0, 10.0).key("searching"));
            return;
        }
        let joining = self.joining.as_ref().filter(|(_, since)| now() - since < 120.0).map(|(n, _)| n.as_str());
        // The current one first, as in the macOS menu
        let current = s.ssid.as_deref().filter(|_| s.on_wifi());
        let mut known: Vec<&Nearby> = networks.iter().filter(|n| n.known).collect();
        known.sort_by_key(|n| Some(n.name.as_str()) != current);
        let other: Vec<&Nearby> = networks.iter().filter(|n| !n.known).collect();
        let mut list = Vec::new();
        for (title, nets, key) in [(t("Reti conosciute", "Known Networks"), known, "known"), (t("Altre reti", "Other Networks"), other, "other")] {
            if nets.is_empty() {
                continue;
            }
            list.push(Node::row(vec![Node::text(title, th.small_font(), th.subtext)]).height(16.0).padding(0.0, 0.0, 0.0, 10.0).key(key));
            let tiles = nets.into_iter().take(10).map(|n| self.nearby_tile(rx, n, Some(n.name.as_str()) == current, joining)).collect();
            list.push(Node::row(tiles).columns(2).gap(GAP).key(format!("{key}-tiles")));
        }
        let height = (rx.display.frame.size.height - used - AROUND).max(NEARBY * 2.0 + GAP + 22.0);
        rows.push(Node::new(Kind::Scroll { max_height: height, start: None }).width(WIDE).key("networks").push(Node::column(list).gap(6.0)));
    }
}

impl Widget for Network {
    fn start(&mut self, cx: &mut Cx) {
        if self.mock.is_none() {
            self.store = Store::new(Box::new(cx.handle::<Network>()));
        }
        self.read(cx);
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let s = self.state.as_ref()?;
        let mut groups = Vec::new();
        if let Some(vpn) = &s.vpn {
            let mut children = vec![Node::symbol("lock.shield.fill", th.icon_size, Weight::Semibold, th.green).key("icon")];
            if !rx.compact {
                children.push(Node::text(vpn, th.font(), th.text).key("name"));
            }
            groups.push(Node::row(children).gap(4.0).key("vpn"));
        }
        let (icon, color, name) = if s.on_wifi() {
            ("wifi", th.text, s.ssid.clone().unwrap_or_else(|| "Wi-Fi".into()))
        } else if s.primary.is_some() {
            ("cable.connector.horizontal", th.text, "Ethernet".into())
        } else if s.wifi_device.is_none() {
            ("network.slash", th.red, "Offline".into())
        } else if s.wifi_power {
            ("wifi.exclamationmark", th.yellow, t("Non connesso", "Not Connected").into())
        } else {
            ("wifi.slash", th.red, t("Wi-Fi off", "Wi-Fi Off").into())
        };
        let mut children = vec![Node::symbol(icon, th.icon_size, Weight::Semibold, color).key("icon")];
        if !rx.compact {
            children.push(Node::text(name, th.font(), th.text).key("name"));
        }
        if s.on_wifi()
            && let Some(band) = &s.band
        {
            let band = if crate::i18n::italian() { band.replacen('.', ",", 1) } else { band.clone() };
            children.push(Node::text(format!("{band} GHz"), th.font_with(Weight::Bold, 11.0), th.subtext).key("band"));
        }
        groups.push(Node::row(children).gap(5.0).key("connection"));
        Some(rx.pill(groups).gap(16.0).click(rx.toggle_popup()))
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let s = self.state.as_ref()?;
        // The switches and the networks on top, the metrics under them
        let mut metrics = vec![self.header(rx, s)];
        let mut used = HEADER + GAP;
        let grid = |tiles: Vec<Node>, key: &str, used: &mut f64| {
            *used += tiles.len().div_ceil(2) as f64 * (STAT + GAP);
            Node::row(tiles).columns(2).gap(GAP).key(key)
        };
        if s.primary.is_some() {
            metrics.push(Node::row(vec![self.traffic(rx, true), self.traffic(rx, false)]).gap(GAP).key("traffic"));
            used += TRAFFIC + GAP;
            if let Some(l) = self.link.as_ref().filter(|_| s.on_wifi()) {
                metrics.push(grid(self.link_tiles(rx, l), "link", &mut used));
            }
            let (tiles, ipv6) = self.address_tiles(rx, s);
            metrics.push(grid(tiles, "addresses", &mut used));
            if let Some(ipv6) = ipv6 {
                metrics.push(ipv6);
                used += STAT + GAP;
            }
        }
        if let Some(vpn) = &s.vpn {
            let detail = if self.copied.as_deref() == Some("vpn") {
                Node::text(t("Indirizzo copiato", "Address copied"), th.small_font(), th.green)
            } else {
                let parts: Vec<&str> = [s.vpn_interface.as_deref(), s.vpn_address.as_deref()].into_iter().flatten().collect();
                Node::text(parts.join(" · "), th.small_font(), th.subtext)
            };
            let connected = t("Connessa", "Connected");
            let state = Node::text(connected, th.small_font(), th.green).key("state");
            let room = crate::gfx::measure_text(connected, &th.small_font()).width;
            metrics.push(line(rx, "lock.shield.fill", th.green, vpn, detail, (state, room), true).click(rx.action("copy:vpn")).key("vpn"));
            used += LINE + GAP;
        }
        let mut wired = Vec::new();
        for (i, w) in s.wired.iter().enumerate() {
            let state = if !w.enabled {
                t("Disattivata", "Off")
            } else if w.connected {
                t("Connessa", "Connected")
            } else {
                t("Non connessa", "Not Connected")
            };
            // USB adapters are named after their speed, phones after themselves
            let adapter = w.name.contains("Ethernet") || w.name.contains("LAN");
            let (title, details) = if adapter { ("Ethernet".to_string(), format!("{} · {state}", w.name)) } else { (w.name.clone(), state.to_string()) };
            let toggle = Node::new(Kind::Toggle { on: w.enabled, color: th.primary, track: th.surface }).click(rx.action(&format!("wired:{i}"))).key("toggle");
            let color = if w.connected { th.primary } else { th.subtext };
            wired.push(line(rx, "cable.connector.horizontal", color, &title, Node::text(details, th.small_font(), th.subtext), (toggle, 30.0), w.connected).key(format!("wired-{i}")));
            used += LINE + GAP;
        }
        let mut rows = Vec::new();
        if s.wifi_device.is_some() {
            self.wifi(rx, s, &mut rows, used);
        }
        rows.extend(wired);
        rows.extend(metrics);
        rows.push(rx.row_button(vec![Node::text(t("Impostazioni Rete…", "Network Settings…"), th.font(), th.primary)], vec![], rx.action("settings")).key("settings"));
        Some(rx.popup(WIDE, rows).gap(GAP).padding(8.0, 8.0, 6.0, 8.0))
    }

    fn ready(&self) -> bool {
        self.state.is_some()
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        let Some(s) = self.state.clone() else { return };
        match name {
            "wifi" => {
                if let Some(device) = &s.wifi_device {
                    let power = if s.wifi_power { "off" } else { "on" };
                    self.networksetup(vec!["-setairportpower".into(), device.clone(), power.into()], cx);
                }
            }
            "scan" => {
                self.scan(cx);
                cx.redraw();
            }
            "location" => {
                cx.close_popup();
                crate::runtime::open("x-apple.systempreferences:com.apple.preference.security?Privacy_LocationServices");
            }
            "settings" => {
                cx.close_popup();
                crate::runtime::open("x-apple.systempreferences:com.apple.Network-Settings.extension");
            }
            _ => {
                if let Some(network) = name.strip_prefix("join:") {
                    self.join(network.to_string(), cx);
                } else if let Some(what) = name.strip_prefix("copy:") {
                    self.copy(what, cx);
                } else if let Some(w) = name.strip_prefix("wired:").and_then(|i| i.parse::<usize>().ok()).and_then(|i| s.wired.get(i)) {
                    let enabled = if w.enabled { "off" } else { "on" };
                    self.networksetup(vec!["-setnetworkserviceenabled".into(), w.name.clone(), enabled.into()], cx);
                }
            }
        }
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        match event {
            Event::Wake => self.read(cx),
            Event::PopupOpened => {
                self.failed = None;
                self.read(cx);
                self.fetch_public(cx);
                if self.state.as_ref().is_some_and(|s| s.wifi_device.is_some() && s.wifi_power) {
                    self.ask_location(cx);
                }
                if let Some(mock) = &self.mock {
                    (self.link, self.iface, self.ping, self.totals) = (Some(mock.link.clone()), Some(mock.interface.clone()), Some(mock.ping), None);
                    // A full graph at once
                    for &(down, up) in &mock.traffic.clone() {
                        self.push(down, up);
                    }
                }
                self.ticking += 1;
                self.ticks = 0;
                self.tick(self.ticking, cx);
            }
            Event::PopupClosed => {
                self.ticking += 1;
                self.samples.clear();
                self.last = None;
                self.copied = None;
            }
            _ => {}
        }
    }

    crate::any_widget!();
}

// The System Configuration store

#[repr(C)]
struct StoreContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

type StoreCallback = extern "C" fn(CFTypeRef, CFTypeRef, *mut c_void);

#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    fn SCDynamicStoreCreate(allocator: CFTypeRef, name: CFTypeRef, callout: Option<StoreCallback>, context: *mut StoreContext) -> CFTypeRef;
    fn SCDynamicStoreCopyValue(store: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn SCDynamicStoreCopyMultiple(store: CFTypeRef, keys: CFTypeRef, patterns: CFTypeRef) -> CFTypeRef;
    fn SCDynamicStoreSetNotificationKeys(store: CFTypeRef, keys: CFTypeRef, patterns: CFTypeRef) -> u8;
    fn SCDynamicStoreSetDispatchQueue(store: CFTypeRef, queue: *const c_void) -> u8;
    fn SCNetworkInterfaceCopyAll() -> CFTypeRef;
    fn SCNetworkInterfaceGetBSDName(interface: CFTypeRef) -> CFTypeRef;
    fn SCNetworkInterfaceGetInterfaceType(interface: CFTypeRef) -> CFTypeRef;
    fn SCPreferencesCreate(allocator: CFTypeRef, name: CFTypeRef, id: CFTypeRef) -> CFTypeRef;
    fn SCNetworkSetCopyCurrent(prefs: CFTypeRef) -> CFTypeRef;
    fn SCNetworkSetGetServiceOrder(set: CFTypeRef) -> CFTypeRef;
    fn SCNetworkServiceCopyAll(prefs: CFTypeRef) -> CFTypeRef;
    fn SCNetworkServiceGetServiceID(service: CFTypeRef) -> CFTypeRef;
    fn SCNetworkServiceGetName(service: CFTypeRef) -> CFTypeRef;
    fn SCNetworkServiceGetEnabled(service: CFTypeRef) -> u8;
    fn SCNetworkServiceGetInterface(service: CFTypeRef) -> CFTypeRef;
}

unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
}

/// The store's notifications of what the bar shows, on the main queue; stopped when dropped.
struct Store {
    store: CFTypeRef,
    handle: *mut Handle<Network>,
}

extern "C" fn store_changed(_store: CFTypeRef, _keys: CFTypeRef, info: *mut c_void) {
    let handle = unsafe { &*(info as *const Handle<Network>) };
    handle.send(|w, cx| w.changed(cx));
}

impl Store {
    fn new(handle: Box<Handle<Network>>) -> Option<Store> {
        let handle = Box::into_raw(handle);
        let mut context = StoreContext { version: 0, info: handle as *mut c_void, retain: std::ptr::null(), release: std::ptr::null(), copy_description: std::ptr::null() };
        let store = unsafe { SCDynamicStoreCreate(std::ptr::null(), cf(ns_string!("rustybar")), Some(store_changed), &mut context) };
        if store.is_null() {
            drop(unsafe { Box::from_raw(handle) });
            return None;
        }
        let keys = NSArray::from_retained_slice(&[NSString::from_str("State:/Network/Global/IPv4"), NSString::from_str("Setup:/Network/Global/IPv4"), NSString::from_str("State:/Network/Global/DNS")]);
        let patterns = NSArray::from_retained_slice(&[
            NSString::from_str("State:/Network/Interface/[^/]+/IPv4"),
            NSString::from_str("State:/Network/Interface/[^/]+/AirPort"),
            NSString::from_str("State:/Network/Service/[^/]+/IPv4"),
            NSString::from_str("State:/Network/Service/[^/]+/IPv6"),
            NSString::from_str("Setup:/Network/Service/[^/]+"),
        ]);
        unsafe {
            SCDynamicStoreSetNotificationKeys(store, cf(&*keys), cf(&*patterns));
            SCDynamicStoreSetDispatchQueue(store, dispatch2::DispatchQueue::main() as *const _ as *const c_void);
        }
        Some(Store { store, handle })
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        unsafe {
            SCDynamicStoreSetDispatchQueue(self.store, std::ptr::null());
            CFRelease(self.store);
            drop(Box::from_raw(self.handle));
        }
    }
}

fn dict(store: CFTypeRef, key: &str) -> Option<Retained<Dict>> {
    unsafe { owned(SCDynamicStoreCopyValue(store, cf(&*NSString::from_str(key)))) }
}

fn as_dict(o: Retained<AnyObject>) -> Option<Retained<Dict>> {
    let d = o.downcast::<NSDictionary>().ok()?;
    // The store's keys are strings
    Some(unsafe { Retained::cast_unchecked(d) })
}

fn string_of(d: &Dict, key: &str) -> Option<String> {
    crate::iokit::string(d, &NSString::from_str(key))
}

/// An array of strings of a dictionary of the store.
fn strings(d: &Dict, key: &str) -> Vec<String> {
    let Some(list) = d.objectForKey(&NSString::from_str(key)).and_then(|o| o.downcast::<NSArray>().ok()) else { return Vec::new() };
    list.iter().filter_map(|o| o.downcast::<NSString>().ok().map(|s| s.to_string())).collect()
}

/// The first address of an IPv4 dictionary of the store.
fn first_address(d: &Dict) -> Option<String> {
    strings(d, "Addresses").into_iter().next()
}

/// 24 for 255.255.255.0.
fn prefix_of(mask: &str) -> Option<u32> {
    mask.parse::<std::net::Ipv4Addr>().ok().map(|m| u32::from(m).count_ones())
}

fn read_state() -> State {
    let mut s = State::default();
    let store = unsafe { SCDynamicStoreCreate(std::ptr::null(), cf(ns_string!("rustybar")), None, std::ptr::null_mut()) };
    if store.is_null() {
        return s;
    }
    // The network ports there are now: USB adapters come and go
    let mut hardware = Vec::new();
    if let Some(all) = unsafe { owned::<NSArray<AnyObject>>(SCNetworkInterfaceCopyAll()) } {
        for i in all.iter() {
            let Some(bsd) = (unsafe { crate::iokit::borrowed::<NSString>(SCNetworkInterfaceGetBSDName(cf(&*i))) }) else { continue };
            let kind = unsafe { crate::iokit::borrowed::<NSString>(SCNetworkInterfaceGetInterfaceType(cf(&*i))) };
            if kind.is_some_and(|k| k.to_string() == "IEEE80211") && s.wifi_device.is_none() {
                s.wifi_device = Some(bsd.to_string());
            }
            hardware.push(bsd.to_string());
        }
    }
    // The interfaces with a connection, and their address
    let mut addresses: HashMap<String, String> = HashMap::new();
    let patterns = NSArray::from_retained_slice(&[
        NSString::from_str("State:/Network/Interface/[^/]+/IPv4"),
        NSString::from_str("State:/Network/Service/[^/]+/IPv4"),
        NSString::from_str("State:/Network/Service/[^/]+/IPv6"),
    ]);
    let mut services: Vec<(String, Retained<Dict>)> = Vec::new();
    let mut services6: Vec<Retained<Dict>> = Vec::new();
    if let Some(found) = unsafe { owned::<NSDictionary<NSString, AnyObject>>(SCDynamicStoreCopyMultiple(store, std::ptr::null(), cf(&*patterns))) } {
        for key in found.allKeys().iter() {
            let Some(d) = found.objectForKey(&key).and_then(as_dict) else { continue };
            let key = key.to_string();
            let parts: Vec<&str> = key.split('/').collect();
            match parts.as_slice() {
                ["State:", "Network", "Interface", name, "IPv4"] => {
                    if let Some(a) = first_address(&d) {
                        addresses.insert(name.to_string(), a);
                    }
                }
                ["State:", "Network", "Service", id, "IPv4"] => services.push((id.to_string(), d)),
                ["State:", "Network", "Service", _, "IPv6"] => services6.push(d),
                _ => {}
            }
        }
    }
    let global = dict(store, "State:/Network/Global/IPv4");
    let global_primary = global.as_ref().and_then(|g| string_of(g, "PrimaryInterface"));

    // The services in the order of System Settings
    let mut names: HashMap<String, String> = HashMap::new();
    let mut ordered: Vec<(String, bool, Option<String>)> = Vec::new();
    unsafe {
        let prefs = SCPreferencesCreate(std::ptr::null(), cf(ns_string!("rustybar")), std::ptr::null());
        if !prefs.is_null() {
            if let Some(all) = owned::<NSArray<AnyObject>>(SCNetworkServiceCopyAll(prefs)) {
                let mut by_id = HashMap::new();
                for svc in all.iter() {
                    let id = crate::iokit::borrowed::<NSString>(SCNetworkServiceGetServiceID(cf(&*svc))).map(|s| s.to_string()).unwrap_or_default();
                    let name = crate::iokit::borrowed::<NSString>(SCNetworkServiceGetName(cf(&*svc))).map(|s| s.to_string()).unwrap_or_default();
                    let interface = SCNetworkServiceGetInterface(cf(&*svc));
                    let bsd = if interface.is_null() { None } else { crate::iokit::borrowed::<NSString>(SCNetworkInterfaceGetBSDName(interface)).map(|s| s.to_string()) };
                    names.insert(id.clone(), name.clone());
                    by_id.insert(id, (name, SCNetworkServiceGetEnabled(cf(&*svc)) != 0, bsd));
                }
                let set = SCNetworkSetCopyCurrent(prefs);
                if !set.is_null() {
                    if let Some(order) = crate::iokit::borrowed::<NSArray<NSString>>(SCNetworkSetGetServiceOrder(set)) {
                        ordered = order.iter().filter_map(|id| by_id.remove(&id.to_string())).collect();
                    }
                    CFRelease(set);
                }
            }
            CFRelease(prefs);
        }
    }

    // Never a VPN tunnel, which isn't a port
    let connected = |i: &String| addresses.contains_key(i);
    s.primary = global_primary
        .filter(|p| hardware.contains(p) && connected(p))
        .or_else(|| ordered.iter().filter_map(|(_, _, bsd)| bsd.clone()).find(|b| hardware.contains(b) && connected(b)));
    if let Some(primary) = &s.primary {
        s.address = addresses.get(primary).cloned();
        let service = services.iter().find(|(_, d)| string_of(d, "InterfaceName").as_ref() == Some(primary)).map(|(_, d)| d);
        s.prefix = service.and_then(|d| strings(d, "SubnetMasks").first().and_then(|m| prefix_of(m)));
        // Each interface has a default route of its own, also when a VPN takes the Mac's
        s.router = service.and_then(|d| string_of(d, "Router"));
        if s.router.is_none() && global.as_ref().and_then(|g| string_of(g, "PrimaryInterface")).as_ref() == Some(primary) {
            s.router = global.as_ref().and_then(|g| string_of(g, "Router"));
        }
        // Not a link-local address, which every interface has
        if let Some(d) = services6.iter().find(|d| string_of(d, "InterfaceName").as_ref() == Some(primary)) {
            let lengths = d.objectForKey(ns_string!("PrefixLength")).and_then(|o| o.downcast::<NSArray>().ok());
            if let Some((i, address)) = strings(d, "Addresses").into_iter().enumerate().find(|(_, a)| !a.to_ascii_lowercase().starts_with("fe80")) {
                s.ipv6 = Some(address);
                s.ipv6_prefix = lengths.and_then(|l| l.iter().nth(i)).and_then(|n| n.downcast::<NSNumber>().ok()).map(|n| n.unsignedIntValue());
            }
        }
    }
    s.dns = dict(store, "State:/Network/Global/DNS").map(|d| strings(&d, "ServerAddresses")).unwrap_or_default();
    // A VPN: a service with a tunnel, as those of System Settings and of VPN apps
    let tunnel = |name: &str| ["utun", "ipsec", "ppp", "tun", "tap"].iter().any(|p| name.strip_prefix(p).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())));
    if let Some((id, d)) = services.iter().find(|(_, d)| string_of(d, "InterfaceName").is_some_and(|i| tunnel(&i))) {
        s.vpn = Some(names.get(id).cloned().filter(|n| !n.is_empty()).unwrap_or_else(|| "VPN".into()));
        s.vpn_interface = string_of(d, "InterfaceName");
        s.vpn_address = first_address(d).or_else(|| s.vpn_interface.as_ref().and_then(|i| addresses.get(i).cloned()));
    }
    s.wired = ordered
        .iter()
        .filter(|(_, _, bsd)| bsd.as_ref().is_some_and(|b| Some(b) != s.wifi_device.as_ref() && !b.starts_with("bridge") && hardware.contains(b)))
        .take(3)
        .map(|(name, enabled, bsd)| Wired { name: name.clone(), enabled: *enabled, connected: bsd.as_ref().is_some_and(connected) })
        .collect();

    if let Some(device) = &s.wifi_device {
        s.wifi_power = unsafe { CWWiFiClient::sharedWiFiClient().interface() }.is_some_and(|i| unsafe { i.powerOn() });
        if addresses.contains_key(device)
            && let Some((ssid, band)) = dict(store, &format!("State:/Network/Interface/{device}/AirPort")).and_then(|d| scan_record(&d))
        {
            s.ssid = Some(ssid).filter(|x| !x.is_empty());
            s.band = band;
        }
    }
    unsafe { CFRelease(store) };
    s
}

/// The network name and band from the last scan record the store keeps: macOS redacts the name
/// everywhere else since 14.4, unless an app may use the location.
fn scan_record(airport: &Dict) -> Option<(String, Option<String>)> {
    let data = airport.objectForKey(ns_string!("CachedScanRecord"))?.downcast::<NSData>().ok()?;
    let unarchiver = unsafe { NSKeyedUnarchiver::initForReadingFromData_error(NSKeyedUnarchiver::alloc(), &data) }.ok()?;
    unsafe { unarchiver.setRequiresSecureCoding(false) };
    let root = unsafe { unarchiver.decodeObjectForKey(objc2_foundation::NSKeyedArchiveRootObjectKey) }?;
    unsafe { unarchiver.finishDecoding() };
    let record = as_dict(root)?;
    let ssid = string_of(&record, "SSID_STR").unwrap_or_default();
    let number = |k: &str| record.objectForKey(&NSString::from_str(k)).and_then(|o| o.downcast::<NSNumber>().ok()).map(|n| n.longLongValue()).unwrap_or(0);
    let (flags, channel) = (number("CHANNEL_FLAGS"), number("CHANNEL"));
    // As in the Apple80211 headers; the channel alone can't tell 6 GHz
    let band = if flags & 0x2000 != 0 {
        "6"
    } else if flags & 0x10 != 0 {
        "5"
    } else if flags & 0x8 != 0 {
        "2.4"
    } else if channel > 14 {
        "5"
    } else if channel > 0 {
        "2.4"
    } else {
        ""
    };
    Some((ssid, Some(band.to_string()).filter(|b| !b.is_empty())))
}

// While the popup is open: the counters, the link, the ping and the public side

/// The bytes in and out of `name` since it came up, with its hardware address, MTU and link
/// speed, from the routing sysctl: its counters have 64 bits, those of getifaddrs wrap at 4 GB.
fn counters(name: &str) -> Option<(Iface, u64, u64)> {
    let cname = std::ffi::CString::new(name).ok()?;
    let index = unsafe { libc::if_nametoindex(cname.as_ptr()) };
    if index == 0 {
        return None;
    }
    let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, index as libc::c_int];
    let mut len: libc::size_t = 0;
    let none = std::ptr::null_mut();
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 6, none, &mut len, none, 0) } != 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 6, buf.as_mut_ptr() as *mut c_void, &mut len, none, 0) } != 0 {
        return None;
    }
    let mut at = 0;
    while at + size_of::<libc::if_msghdr>() <= len {
        let header = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(at) as *const libc::if_msghdr) };
        if header.ifm_type as libc::c_int == libc::RTM_IFINFO2 && at + size_of::<libc::if_msghdr2>() <= len {
            let message = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(at) as *const libc::if_msghdr2) };
            let data = message.ifm_data;
            // The link-level address follows, after the interface's name
            let link = at + size_of::<libc::if_msghdr2>();
            let mut mac = String::new();
            if link + size_of::<libc::sockaddr_dl>() <= len {
                let dl = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(link) as *const libc::sockaddr_dl) };
                let start = link + std::mem::offset_of!(libc::sockaddr_dl, sdl_data) + dl.sdl_nlen as usize;
                if dl.sdl_alen == 6 && start + 6 <= len {
                    mac = buf[start..start + 6].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":");
                }
            }
            let (mtu, speed, received, sent) = (data.ifi_mtu, data.ifi_baudrate, data.ifi_ibytes, data.ifi_obytes);
            return Some((Iface { mac, mtu, speed }, received, sent));
        }
        if header.ifm_msglen == 0 {
            break;
        }
        at += header.ifm_msglen as usize;
    }
    None
}

/// The Wi-Fi link now; None when not associated. None of it needs the Location permission.
fn link() -> Option<Link> {
    let iface = interface()?;
    let rssi = unsafe { iface.rssiValue() } as i64;
    if rssi == 0 {
        return None;
    }
    let channel = unsafe { iface.wlanChannel() };
    let width = match channel.as_ref().map(|c| unsafe { c.channelWidth() }.0) {
        Some(1) => 20,
        Some(2) => 40,
        Some(3) => 80,
        Some(4) => 160,
        Some(5) => 320,
        _ => 0,
    };
    let phy = match unsafe { iface.activePHYMode() }.0 {
        1 => "a",
        2 => "b",
        3 => "g",
        4 => "n",
        5 => "ac",
        6 => "ax",
        7 => "be",
        _ => "",
    };
    Some(Link {
        rssi,
        noise: unsafe { iface.noiseMeasurement() } as i64,
        rate: unsafe { iface.transmitRate() },
        channel: channel.as_ref().map(|c| unsafe { c.channelNumber() } as i64).unwrap_or(0),
        band: band_of(channel.as_deref()),
        width,
        phy: phy.into(),
        security: unsafe { iface.security() }.0 as i64,
    })
}

fn band_of(channel: Option<&CWChannel>) -> String {
    match channel.map(|c| unsafe { c.channelBand() }.0) {
        Some(1) => "2.4",
        Some(2) => "5",
        Some(3) => "6",
        _ => "",
    }
    .into()
}

/// The internet checksum of ICMP.
fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = data.chunks(2).map(|c| u32::from(u16::from_be_bytes([c[0], c.get(1).copied().unwrap_or(0)]))).sum();
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// The round trip to `host` in milliseconds, the best of three ICMP echoes. A datagram socket
/// needs no privileges on macOS, and gives the reply back with its IP header.
fn ping(host: &str) -> Option<f64> {
    let address: std::net::Ipv4Addr = host.parse().ok()?;
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_ICMP) };
    if fd < 0 {
        return None;
    }
    let timeout = libc::timeval { tv_sec: 1, tv_usec: 0 };
    unsafe { libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVTIMEO, &timeout as *const _ as *const c_void, size_of::<libc::timeval>() as libc::socklen_t) };
    let mut to: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    to.sin_len = size_of::<libc::sockaddr_in>() as u8;
    to.sin_family = libc::AF_INET as libc::sa_family_t;
    to.sin_addr.s_addr = u32::from(address).to_be();
    let id = (std::process::id() as u16).to_be_bytes();
    let mut best: Option<f64> = None;
    for seq in 0..3u16 {
        let seq = seq.to_be_bytes();
        let mut packet = [8, 0, 0, 0, id[0], id[1], seq[0], seq[1], b'r', b'u', b's', b't', b'y', b'b', b'a', b'r'];
        let sum = checksum(&packet).to_be_bytes();
        packet[2..4].copy_from_slice(&sum);
        let start = Instant::now();
        let sent = unsafe { libc::sendto(fd, packet.as_ptr() as *const c_void, packet.len(), 0, &to as *const _ as *const libc::sockaddr, size_of::<libc::sockaddr_in>() as libc::socklen_t) };
        if sent < 0 {
            break;
        }
        // Other echoes may come too: only the reply to this one
        let mut reply = [0u8; 256];
        while start.elapsed() < Duration::from_secs(1) {
            let n = unsafe { libc::recv(fd, reply.as_mut_ptr() as *mut c_void, reply.len(), 0) };
            if n <= 0 {
                break;
            }
            let n = n as usize;
            let at = if reply[0] >> 4 == 4 { (reply[0] & 0xf) as usize * 4 } else { 0 };
            if n >= at + 8 && reply[at] == 0 && reply[at + 4..at + 6] == id && reply[at + 6..at + 8] == seq {
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                best = Some(best.map_or(ms, |b| b.min(ms)));
                break;
            }
        }
    }
    unsafe { libc::close(fd) };
    best
}

/// "AS3269 Example S.p.A." → "Example S.p.A.".
fn provider(org: &str) -> String {
    match org.split_once(' ') {
        Some((asn, rest)) if asn.starts_with("AS") && asn[2..].bytes().all(|b| b.is_ascii_digit()) => rest.to_string(),
        _ => org.to_string(),
    }
}

/// The public address with its provider and place from ipinfo.io, and the latency to it; from
/// ipify, without them, if ipinfo.io doesn't answer. Over IPv4, as the router and the local IP.
fn public() -> Public {
    let curl = |url: &str| {
        let out = std::process::Command::new("/usr/bin/curl").args(["-s", "-4", "--max-time", "5", "-w", "\n%{time_namelookup} %{time_connect}", url]).output();
        let out = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        let (body, times) = out.rsplit_once('\n').unwrap_or((&out, ""));
        let times: Vec<f64> = times.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        let latency = match times[..] {
            [lookup, connect] if connect > lookup => Some((connect - lookup) * 1000.0),
            _ => None,
        };
        (body.trim().to_string(), latency)
    };
    let valid = |ip: &str| ip.parse::<std::net::Ipv4Addr>().is_ok();
    let (body, latency) = curl("https://ipinfo.io/json");
    let info: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let field = |k: &str| info[k].as_str().unwrap_or_default().to_string();
    if valid(&field("ip")) {
        return Public { ip: field("ip"), provider: provider(&field("org")), city: field("city"), country: field("country"), latency };
    }
    let (ip, latency) = curl("https://api.ipify.org");
    if valid(&ip) { Public { ip, latency, ..Default::default() } } else { Public::default() }
}

// CoreWLAN: nearby networks and joining one (with the Location permission)

fn interface() -> Option<Retained<CWInterface>> {
    unsafe { CWWiFiClient::sharedWiFiClient().interface() }.filter(|i| unsafe { i.powerOn() })
}

fn security(n: &CWNetwork) -> Security {
    let any = |list: &[CWSecurity]| list.iter().any(|&s| unsafe { n.supportsSecurity(s) });
    if any(&[CWSecurity::None, CWSecurity::OWE, CWSecurity::OWETransition]) {
        Security::Open
    } else if any(&[CWSecurity::DynamicWEP, CWSecurity::WPAEnterprise, CWSecurity::WPAEnterpriseMixed, CWSecurity::WPA2Enterprise, CWSecurity::Enterprise, CWSecurity::WPA3Enterprise]) {
        Security::Enterprise
    } else {
        Security::Personal
    }
}

/// The newest protocol a network has: "WPA3", "WPA2/3" for one that takes both…
fn protocol(n: &CWNetwork) -> String {
    let any = |list: &[CWSecurity]| list.iter().any(|&s| unsafe { n.supportsSecurity(s) });
    if any(&[CWSecurity::WPA3Transition]) {
        "WPA2/3"
    } else if any(&[CWSecurity::WPA3Personal, CWSecurity::WPA3Enterprise]) {
        "WPA3"
    } else if any(&[CWSecurity::WPA2Personal, CWSecurity::WPA2Enterprise]) {
        "WPA2"
    } else if any(&[CWSecurity::WPAPersonal, CWSecurity::WPAPersonalMixed, CWSecurity::WPAEnterprise, CWSecurity::WPAEnterpriseMixed]) {
        "WPA"
    } else if any(&[CWSecurity::WEP, CWSecurity::DynamicWEP]) {
        "WEP"
    } else {
        ""
    }
    .into()
}

/// The networks around, by signal and then name; with `scan`, after looking again.
fn nearby(scan: bool) -> Vec<Nearby> {
    let Some(iface) = interface() else { return Vec::new() };
    let found = if scan { unsafe { iface.scanForNetworksWithSSID_error(None) }.ok() } else { unsafe { iface.cachedScanResults() } };
    let Some(found) = found else { return Vec::new() };
    let known: Vec<String> = unsafe { iface.configuration() }
        .map(|c| {
            let profiles = unsafe { c.networkProfiles() };
            (0..profiles.count()).filter_map(|i| unsafe { profiles.objectAtIndex(i).ssid() }.map(|s| s.to_string())).collect()
        })
        .unwrap_or_default();
    // A network has an access point for each band, and more in a mesh: the strongest
    let mut best: HashMap<String, (isize, Retained<CWNetwork>)> = HashMap::new();
    for n in found.iter() {
        let Some(name) = (unsafe { n.ssid() }).map(|s| s.to_string()).filter(|s| !s.is_empty()) else { continue };
        let rssi = unsafe { n.rssiValue() };
        if best.get(&name).is_none_or(|(r, _)| rssi > *r) {
            best.insert(name, (rssi, n));
        }
    }
    let mut list: Vec<Nearby> = best
        .into_iter()
        .map(|(name, (rssi, n))| Nearby {
            known: known.contains(&name),
            name: name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect(),
            level: if rssi >= -60 { 3 } else if rssi >= -72 { 2 } else { 1 },
            security: security(&n),
            band: band_of(unsafe { n.wlanChannel() }.as_deref()),
            label: protocol(&n),
        })
        .collect();
    list.sort_by(|a, b| b.level.cmp(&a.level).then_with(|| a.name.cmp(&b.name)));
    list
}

/// Joins a network: with the typed password, else with the one saved last time, else (for a
/// network of the Mac) with the System keychain's, which macOS gives only with an administrator's
/// name and password. CoreWLAN wants the password also for a saved network.
fn try_join(name: &str, allowed: bool, typed: Option<String>) -> Joined {
    if !allowed {
        return Joined::Denied;
    }
    let Some(iface) = interface() else { return Joined::Failed };
    let ns_name = NSString::from_str(name);
    let mut candidates: Vec<Retained<CWNetwork>> = unsafe { iface.cachedScanResults() }
        .map(|set| set.iter().filter(|n| unsafe { n.ssid() }.is_some_and(|s| s.to_string() == name)).collect())
        .unwrap_or_default();
    if candidates.is_empty() {
        candidates = unsafe { iface.scanForNetworksWithName_error(Some(&ns_name)) }.map(|set| set.iter().collect()).unwrap_or_default();
    }
    candidates.sort_by_key(|n| -unsafe { n.rssiValue() });
    let Some(network) = candidates.first() else { return Joined::Missing };
    let associate = |password: Option<&str>| unsafe { iface.associateToNetwork_password_error(network, password.map(NSString::from_str).as_deref()) }.is_ok();
    match security(network) {
        Security::Open => return if associate(None) { Joined::Done } else { Joined::Failed },
        Security::Enterprise => return Joined::Enterprise,
        Security::Personal => {}
    }
    if let Some(password) = typed {
        return if associate(Some(&password)) {
            save_password(name, &password);
            Joined::Done
        } else {
            Joined::Password { wrong: true }
        };
    }
    if let Some(password) = saved_password(name)
        && associate(Some(&password))
    {
        return Joined::Done;
    }
    let known = unsafe { iface.configuration() }.is_some_and(|c| {
        let profiles = unsafe { c.networkProfiles() };
        (0..profiles.count()).any(|i| unsafe { profiles.objectAtIndex(i).ssid() }.is_some_and(|s| s.to_string() == name))
    });
    if known {
        let mut password: *mut NSString = std::ptr::null_mut();
        let ssid = NSData::with_bytes(name.as_bytes());
        let status = unsafe { CWKeychainFindWiFiPassword(CWKeychainDomain::System, &ssid, &mut password) };
        if let Some(password) = (status == 0).then(|| unsafe { Retained::from_raw(password) }).flatten().map(|p| p.to_string())
            && associate(Some(&password))
        {
            save_password(name, &password);
            return Joined::Done;
        }
    }
    Joined::Password { wrong: false }
}

/// The passwords typed in the popup, in the login keychain; `sketchybar-wifi` is where SketchyBar
/// kept them.
fn saved_password(name: &str) -> Option<String> {
    ["rustybar-wifi", "sketchybar-wifi"].iter().find_map(|service| crate::secret::get(service, Some(name)))
}

fn save_password(name: &str, password: &str) {
    crate::secret::set("rustybar-wifi", name, password);
}

/// Asks for the password of a network, as macOS does; None if cancelled.
fn ask_password(name: &str, wrong: bool) -> Option<String> {
    let mtm = MainThreadMarker::new()?;
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(t(
        &format!("La rete Wi-Fi “{name}” richiede una password."),
        &format!("The Wi-Fi network “{name}” requires a password."),
    )));
    alert.setInformativeText(&NSString::from_str(if wrong { t("La password non è corretta: riprova.", "The password is incorrect: try again.") } else { "" }));
    if let Some(icon) = NSImage::imageWithSystemSymbolName_accessibilityDescription(ns_string!("wifi"), None) {
        let config = NSImageSymbolConfiguration::configurationWithPointSize_weight(48.0, 0.0);
        unsafe { alert.setIcon(icon.imageWithSymbolConfiguration(&config).as_deref()) };
    }
    alert.addButtonWithTitle(&NSString::from_str(t("Connetti", "Join")));
    alert.addButtonWithTitle(&NSString::from_str(t("Annulla", "Cancel")));
    let field = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(260.0, 24.0)));
    field.setPlaceholderString(Some(ns_string!("Password")));
    alert.setAccessoryView(Some(&field));
    alert.window().setInitialFirstResponder(Some(&field));
    // The first button
    (alert.runModal() == 1000).then(|| field.stringValue().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let s = State { primary: Some("en0".into()), wifi_device: Some("en0".into()), ssid: Some("Home".into()), ..Default::default() };
        assert!(s.on_wifi());
        assert_eq!(s.key(), "en0:Home:");
    }

    #[test]
    fn units() {
        assert_eq!(prefix_of("255.255.255.0"), Some(24));
        assert_eq!(prefix_of("255.255.240.0"), Some(20));
        assert_eq!(provider("AS64496 Example Broadband"), "Example Broadband");
        assert_eq!(provider("Example"), "Example");
        assert!(private("02:00:5e:00:53:24"));
        assert!(!private("00:00:5e:00:53:24"));
        assert_eq!(generation("ax", "6"), "Wi-Fi 6E");
        assert_eq!(generation("g", "2.4"), "802.11g");
        assert_eq!(dbm(-54), "−54 dBm");
    }

    #[test]
    fn ping_loopback() {
        assert!(ping("127.0.0.1").is_some_and(|ms| ms < 1000.0));
    }

    #[test]
    fn icmp_checksum() {
        // A packet with its checksum in it sums to zero
        let mut packet = [8u8, 0, 0, 0, 0x12, 0x34, 0, 1, b'r', b'u', b's', b't', b'y'];
        let sum = checksum(&packet).to_be_bytes();
        packet[2..4].copy_from_slice(&sum);
        assert_eq!(checksum(&packet), 0);
    }
}
