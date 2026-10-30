//! The connection: in the bar the VPN, then the Wi-Fi network and its band, or Ethernet, or why
//! there's no connection (the icon yellow or red). In the popup the addresses, the router and the
//! VPN, the wired services and the Wi-Fi to turn on and off, and the nearby networks to join.
//!
//! Everything comes from the System Configuration store, which tells when it changes, and from
//! CoreWLAN; nothing is polled. The network name comes from the last scan record the store keeps,
//! as macOS gives it to CoreWLAN only with the Location permission, which the list of nearby
//! networks needs: RustyBar asks for it the first time the popup opens.

use std::collections::HashMap;
use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAlert, NSApplication, NSImage, NSImageSymbolConfiguration, NSSecureTextField};
use objc2_core_location::CLLocationManager;
use objc2_core_wlan::{CWInterface, CWKeychainDomain, CWKeychainFindWiFiPassword, CWNetwork, CWSecurity, CWWiFiClient};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSKeyedUnarchiver, NSNumber, NSPoint, NSRect, NSSize, NSString, ns_string};
use serde::Deserialize;

use super::{Cx, Event, Handle, Rx, Widget};
use crate::config::Options;
use crate::gfx::Weight;
use crate::i18n::t;
use crate::iokit::{CFTypeRef, Dict, cf, owned};
use crate::ui::{Justify, Node};

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
    router: Option<String>,
    vpn: Option<String>,
    vpn_address: Option<String>,
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

/// Made-up data (see `mock`): a State, plus `networks` and `public_ip`.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    #[serde(flatten)]
    state: State,
    networks: Vec<Nearby>,
    public_ip: String,
}

pub struct Network {
    state: Option<State>,
    networks: Vec<Nearby>,
    permission: Permission,
    location: Option<Retained<CLLocationManager>>,
    /// The public address and the connection it's of.
    public_ip: Option<(String, String)>,
    fetching: bool,
    scanning: bool,
    /// The network being joined, and since when: one at a time, for at most 2 minutes.
    joining: Option<(String, f64)>,
    failed: Option<String>,
    store: Option<Store>,
    changes: u64,
    reading: bool,
    again: bool,
    mock: Option<Mock>,
}

fn now() -> f64 {
    objc2_foundation::NSDate::now().timeIntervalSince1970()
}

/// The names of nearby networks, cut with … after 32 characters.
fn cut(name: &str) -> String {
    if name.chars().count() > 32 { name.chars().take(31).collect::<String>() + "…" } else { name.to_string() }
}

impl Network {
    pub fn new(_options: &Options) -> Network {
        let mock = crate::mock::load::<Mock>("network");
        Network {
            state: None,
            networks: mock.as_ref().map(|m| m.networks.clone()).unwrap_or_default(),
            permission: if mock.is_some() { Permission::Allowed } else { Permission::Unknown },
            location: None,
            public_ip: None,
            fetching: false,
            scanning: false,
            joining: None,
            failed: None,
            store: None,
            changes: 0,
            reading: false,
            again: false,
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

    fn fetch_public_ip(&mut self, cx: &mut Cx) {
        if self.fetching {
            return;
        }
        if let Some(mock) = &self.mock {
            self.public_ip = Some((mock.public_ip.clone(), mock.state.key()));
            return;
        }
        let Some(key) = self.state.as_ref().map(State::key) else { return };
        self.fetching = true;
        // Through the VPN, its own
        cx.background::<Network, String>(
            || {
                let out = std::process::Command::new("/usr/bin/curl").args(["-s", "--max-time", "5", "https://api.ipify.org"]).output();
                let ip = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
                let valid = ip.split('.').count() == 4 && ip.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
                if valid { ip } else { String::new() }
            },
            move |w, ip, cx| {
                w.fetching = false;
                w.public_ip = Some((ip, key));
                cx.redraw();
            },
        );
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
        if self.scanning || self.permission != Permission::Allowed || !self.state.as_ref().is_some_and(|s| s.wifi_power) {
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
        let bold = th.font();
        let small = th.font_with(Weight::Medium, 11.0);
        let joining = self.joining.as_ref().filter(|(_, since)| now() - since < 120.0).map(|(n, _)| n.as_str());
        let (none, unavailable, off) = (t("Nessuno", "None"), t("Non disponibile", "Not Available"), t("Non attiva", "Off"));
        let (turn_on, turn_off, connecting, failed) = (t("Attiva", "Turn On"), t("Disattiva", "Turn Off"), t("Connessione…", "Connecting…"), t("Non riuscito", "Failed"));
        // The right column, as wide as its widest text, so that they line up
        let public = match &self.public_ip {
            None => "…".to_string(),
            Some((ip, _)) if ip.is_empty() => unavailable.to_string(),
            Some((ip, _)) => ip.clone(),
        };
        let texts = [s.address.as_deref().unwrap_or(none), &public, unavailable, s.router.as_deref().unwrap_or(""), s.vpn.as_deref().unwrap_or(off), s.vpn_address.as_deref().unwrap_or(""), turn_on, turn_off, connecting, failed];
        let right = texts.iter().map(|x| crate::gfx::measure_text(x, &bold).width).fold(0.0, f64::max);
        let column = |node: Node| Node::row(vec![node]).min_width(right).justify(Justify::End).key("right");
        let info = |key: &str, title: &str, value: Node| {
            Node::row(vec![Node::text(title, bold.clone(), th.text), Node::spacer(), column(value)]).gap(16.0).padding(0.0, 8.0, 0.0, 8.0).height(24.0).key(key)
        };
        let button = |key: &str, left: Node, value: Option<Node>, action: &str| {
            let mut children = vec![left, Node::spacer()];
            children.extend(value.map(column));
            Node::row(children).gap(16.0).padding(0.0, 8.0, 0.0, 8.0).height(24.0).radius(th.row_radius).click(rx.action(action)).key(key)
        };
        let space = |key: &str, h: f64| Node::default().height(h).key(key);
        let label = |key: &str, text: &str, h: f64| Node::row(vec![Node::text(text, small.clone(), th.subtext)]).padding(0.0, 8.0, 0.0, 8.0).height(h).key(key);

        let mut rows = vec![info("address", t("Indirizzo IP", "IP Address"), Node::text(s.address.as_deref().unwrap_or(none), bold.clone(), th.subtext))];
        // The public address read last, dimmed when the connection changed since then
        let stale = self.public_ip.as_ref().is_some_and(|(_, key)| *key != s.key());
        rows.push(info("public", t("IP pubblico", "Public IP"), Node::text(public, bold.clone(), if stale { th.overlay } else { th.subtext })));
        if let Some(router) = &s.router {
            rows.push(info("router", "Router", Node::text(router, bold.clone(), th.subtext)));
        }
        rows.push(info("vpn", "VPN", match &s.vpn {
            Some(name) => Node::text(name, bold.clone(), th.green),
            None => Node::text(off, bold.clone(), th.subtext),
        }));
        if let Some(address) = &s.vpn_address {
            rows.push(info("vpn-address", t("Indirizzo VPN", "VPN Address"), Node::text(address, bold.clone(), th.subtext)));
        }

        for (i, w) in s.wired.iter().enumerate() {
            let state = if !w.enabled {
                t("disattivata", "off")
            } else if w.connected {
                t("connessa", "connected")
            } else {
                t("non connessa", "not connected")
            };
            // USB adapters are named after their speed, phones after themselves
            let adapter = w.name.contains("Ethernet") || w.name.contains("LAN");
            let (title, details) = if adapter { ("Ethernet".to_string(), format!("{} · {state}", w.name)) } else { (w.name.clone(), state.to_string()) };
            rows.push(space(&format!("wired-gap-{i}"), 10.0));
            rows.push(button(
                &format!("wired-{i}"),
                Node::text(title, bold.clone(), th.text),
                Some(Node::text(if w.enabled { turn_off } else { turn_on }, bold.clone(), th.primary)),
                &format!("wired:{i}"),
            ));
            rows.push(Node::row(vec![Node::text(details, small.clone(), th.subtext)]).padding(0.0, 8.0, 0.0, 8.0).height(16.0).key(format!("wired-details-{i}")));
        }

        if s.wifi_device.is_some() {
            rows.push(space("wifi-gap", 10.0));
            rows.push(button("wifi", Node::text("Wi-Fi", bold.clone(), th.text), Some(Node::text(if s.wifi_power { turn_off } else { turn_on }, bold.clone(), th.primary)), "wifi"));
            if s.wifi_power {
                let networks = if self.permission == Permission::Allowed { &self.networks[..] } else { &[] };
                if self.permission == Permission::Denied {
                    rows.push(button("location", Node::text(t("Consenti la posizione per vedere le reti", "Allow Location to see the networks"), small.clone(), th.primary), None, "location"));
                } else if networks.is_empty() {
                    rows.push(label("searching", t("Ricerca reti…", "Looking for networks…"), 20.0));
                }
                // The current one first, as in the macOS menu
                let current = s.ssid.as_deref();
                let mut known: Vec<&Nearby> = networks.iter().filter(|n| n.known).collect();
                known.sort_by_key(|n| Some(n.name.as_str()) != current);
                let other: Vec<&Nearby> = networks.iter().filter(|n| !n.known).collect();
                for (title, list, key) in [(t("Reti conosciute", "Known Networks"), known, "known"), (t("Altre reti", "Other Networks"), other, "other")] {
                    if list.is_empty() {
                        continue;
                    }
                    rows.push(label(key, title, 20.0));
                    for n in list.into_iter().take(10) {
                        let right = if joining == Some(n.name.as_str()) {
                            Node::text(connecting, bold.clone(), th.yellow)
                        } else if self.failed.as_deref() == Some(n.name.as_str()) {
                            Node::text(failed, bold.clone(), th.red)
                        } else if n.security != Security::Open {
                            Node::symbol("lock.fill", 11.0, Weight::Semibold, th.subtext)
                        } else {
                            Node::default()
                        };
                        let level = match n.level {
                            3.. => 1.0,
                            2 => 0.5,
                            _ => 0.1,
                        };
                        let mut row = Node::row(vec![
                            Node::symbol(format!("wifi@{level}"), 14.0, Weight::Semibold, th.text).key("signal"),
                            Node::text(cut(&n.name), bold.clone(), th.text).key("name"),
                            Node::spacer(),
                            column(right),
                        ])
                        .gap(8.0)
                        .padding(0.0, 8.0, 0.0, 8.0)
                        .height(24.0)
                        .radius(th.row_radius)
                        .key(format!("net:{}", n.name));
                        if Some(n.name.as_str()) == current {
                            row = row.background(th.surface);
                        } else {
                            row = row.click(rx.action(&format!("join:{}", n.name)));
                        }
                        rows.push(row);
                    }
                }
            }
        }
        rows.push(space("settings-gap", 10.0));
        rows.push(button("settings", Node::text(t("Impostazioni Rete…", "Network Settings…"), bold.clone(), th.primary), None, "settings"));
        rows.push(space("bottom", 4.0));
        Some(rx.popup(260.0, rows).padding(4.0, 4.0, 0.0, 4.0).gap(0.0))
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
                self.fetch_public_ip(cx);
                if self.state.as_ref().is_some_and(|s| s.wifi_device.is_some() && s.wifi_power) {
                    self.ask_location(cx);
                }
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
        let keys = NSArray::from_retained_slice(&[NSString::from_str("State:/Network/Global/IPv4"), NSString::from_str("Setup:/Network/Global/IPv4")]);
        let patterns = NSArray::from_retained_slice(&[
            NSString::from_str("State:/Network/Interface/[^/]+/IPv4"),
            NSString::from_str("State:/Network/Interface/[^/]+/AirPort"),
            NSString::from_str("State:/Network/Service/[^/]+/IPv4"),
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

/// The first address of an IPv4 dictionary of the store.
fn first_address(d: &Dict) -> Option<String> {
    let addresses = d.objectForKey(ns_string!("Addresses"))?.downcast::<NSArray>().ok()?;
    addresses.iter().next()?.downcast::<NSString>().ok().map(|s| s.to_string())
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
    let patterns = NSArray::from_retained_slice(&[NSString::from_str("State:/Network/Interface/[^/]+/IPv4"), NSString::from_str("State:/Network/Service/[^/]+/IPv4")]);
    let mut services: Vec<(String, Retained<Dict>)> = Vec::new();
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
        // Each interface has a default route of its own, also when a VPN takes the Mac's
        s.router = services.iter().find(|(_, d)| string_of(d, "InterfaceName").as_ref() == Some(primary)).and_then(|(_, d)| string_of(d, "Router"));
        if s.router.is_none() && global.as_ref().and_then(|g| string_of(g, "PrimaryInterface")).as_ref() == Some(primary) {
            s.router = global.as_ref().and_then(|g| string_of(g, "Router"));
        }
    }
    // A VPN: a service with a tunnel, as those of System Settings and of VPN apps
    let tunnel = |name: &str| ["utun", "ipsec", "ppp", "tun", "tap"].iter().any(|p| name.strip_prefix(p).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())));
    if let Some((id, d)) = services.iter().find(|(_, d)| string_of(d, "InterfaceName").is_some_and(|i| tunnel(&i))) {
        s.vpn = Some(names.get(id).cloned().filter(|n| !n.is_empty()).unwrap_or_else(|| "VPN".into()));
        s.vpn_address = first_address(d).or_else(|| string_of(d, "InterfaceName").and_then(|i| addresses.get(&i).cloned()));
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
    let mut best: HashMap<String, (isize, Security)> = HashMap::new();
    for n in found.iter() {
        let Some(name) = (unsafe { n.ssid() }).map(|s| s.to_string()).filter(|s| !s.is_empty()) else { continue };
        let rssi = unsafe { n.rssiValue() };
        if best.get(&name).is_none_or(|(r, _)| rssi > *r) {
            best.insert(name, (rssi, security(&n)));
        }
    }
    let mut list: Vec<Nearby> = best
        .into_iter()
        .map(|(name, (rssi, security))| Nearby {
            known: known.contains(&name),
            name: name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect(),
            level: if rssi >= -60 { 3 } else if rssi >= -72 { 2 } else { 1 },
            security,
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
        assert_eq!(cut("short"), "short");
        assert_eq!(cut(&"a".repeat(40)).chars().count(), 32);
        let s = State { primary: Some("en0".into()), wifi_device: Some("en0".into()), ssid: Some("Home".into()), ..Default::default() };
        assert!(s.on_wifi());
        assert_eq!(s.key(), "en0:Home:");
    }
}
