//! The sound: in the bar the output and the input in use, with the battery of Bluetooth ones;
//! the Mac's own speakers and microphone, or the two halves of a headset, show as one. In the
//! popup the volume of each, the devices to switch to, and the paired Bluetooth headphones and
//! speakers to connect or disconnect.
//!
//! CoreAudio tells when devices come and go and when their volume changes. Batteries come from
//! IOKit and, for AirPods, from `system_profiler`, which also lists the paired devices: it runs
//! when the popup opens and, while a Bluetooth device is in use, every 2 minutes.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_foundation::NSString;
use serde::Deserialize;

use super::{Cx, Event, Handle, Rx, Widget};
use crate::config::Options;
use crate::gfx::Weight;
use crate::i18n::t;
use crate::ui::{Justify, Node};

#[derive(Clone, Copy, PartialEq, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Transport {
    Builtin,
    Bluetooth,
    Display,
    Airplay,
    /// Aggregate, multi-output and virtual devices.
    Virtual,
    /// USB, Thunderbolt…
    #[default]
    External,
}

#[derive(Clone, Copy, PartialEq, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Type {
    Headphones,
    Headset,
    Speaker,
    Microphone,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Device {
    #[serde(skip)]
    object: u32,
    uid: String,
    name: String,
    transport: Transport,
    #[serde(rename = "type")]
    kind: Type,
    /// From IOKit, as `pmset -g accps` shows it.
    battery: Option<u8>,
    default: bool,
}

impl Device {
    /// The same for both halves of a device: Bluetooth headsets are two, `<address>:output` and
    /// `<address>:input`, USB ones one.
    fn id(&self) -> String {
        self.uid.trim_end_matches(":output").trim_end_matches(":input").to_uppercase()
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Volume {
    /// No device.
    #[default]
    None,
    /// Set on the device only: displays, headsets with a knob.
    Fixed,
    Level(u8),
}

/// What CoreAudio says.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Snapshot {
    outputs: Vec<Device>,
    inputs: Vec<Device>,
    output_volume: Volume,
    input_volume: Volume,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Paired {
    name: String,
    /// As CoreAudio writes it: AA-BB-CC-DD-EE-FF.
    address: String,
    #[serde(rename = "type")]
    kind: Type,
}

/// What `system_profiler` says.
#[derive(Clone, Debug, Default)]
struct Bluetooth {
    paired: Vec<Paired>,
    /// By address: the lower of the two earbuds, or the main battery.
    batteries: HashMap<String, u8>,
}

/// Made-up devices (see `mock`): a Snapshot, plus `bluetooth`, the paired devices.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    #[serde(flatten)]
    snapshot: Snapshot,
    bluetooth: Vec<Paired>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Outcome {
    Done,
    Failed,
    /// No Bluetooth permission.
    Denied,
}

#[derive(Clone, Copy, PartialEq)]
enum Badges {
    Speaker,
    Microphone,
    Both,
}

impl Badges {
    fn symbols(self) -> &'static str {
        match self {
            Badges::Speaker => "speaker.wave.2.fill",
            Badges::Microphone => "mic.fill",
            // The waves would run into the microphone
            Badges::Both => "speaker.fill+mic.fill",
        }
    }
}

pub struct Audio {
    devices: Option<Snapshot>,
    bluetooth: Bluetooth,
    mock: Option<Mock>,
    /// Bluetooth actions under way, by address: true to connect.
    busy: HashMap<String, bool>,
    /// The last Bluetooth action of a device that didn't work, until the popup is drawn anew.
    failed: HashMap<String, Outcome>,
    /// CoreAudio sends bursts (switching devices, holding a volume key): one read for each.
    changes: u64,
    reading: bool,
    again: bool,
    profiling: bool,
}

impl Audio {
    pub fn new(_options: &Options) -> Audio {
        Audio {
            devices: None,
            bluetooth: Bluetooth::default(),
            mock: crate::mock::load::<Mock>("audio"),
            busy: HashMap::new(),
            failed: HashMap::new(),
            changes: 0,
            reading: false,
            again: false,
            profiling: false,
        }
    }

    /// Reads the devices again, in the background; one read at a time.
    fn read(&mut self, cx: &mut Cx) {
        if self.reading {
            self.again = true;
            return;
        }
        if let Some(mock) = &self.mock {
            self.devices = Some(mock.snapshot.clone());
            self.bluetooth.paired = mock.bluetooth.clone();
            cx.redraw();
            return;
        }
        self.reading = true;
        cx.background::<Audio, Snapshot>(read_devices, |w, snapshot, cx| {
            w.reading = false;
            let before = w.devices.as_ref().map(bluetooth_ids);
            let after = bluetooth_ids(&snapshot);
            w.devices = Some(snapshot);
            // A Bluetooth device came: its battery, if it's AirPods
            if before.is_some_and(|b| b != after) && !after.is_empty() {
                w.profile(cx);
            }
            if std::mem::take(&mut w.again) {
                w.read(cx);
            }
            cx.redraw();
        });
    }

    fn changed(&mut self, cx: &mut Cx) {
        self.changes += 1;
        let changes = self.changes;
        cx.after::<Audio>(0.2, move |w, cx| {
            if w.changes == changes {
                w.read(cx);
            }
        });
    }

    /// Reads the paired Bluetooth devices and their batteries again.
    fn profile(&mut self, cx: &mut Cx) {
        if self.profiling || self.mock.is_some() {
            return;
        }
        self.profiling = true;
        cx.background::<Audio, Option<Bluetooth>>(read_bluetooth, |w, bluetooth, cx| {
            w.profiling = false;
            if let Some(b) = bluetooth {
                w.bluetooth = b;
                cx.redraw();
            }
        });
    }

    fn battery(&self, d: &Device) -> Option<u8> {
        if d.transport != Transport::Bluetooth {
            return d.battery;
        }
        self.bluetooth.batteries.get(&d.id()).copied().or(d.battery)
    }

    /// Whether the audio of a paired device is there: CoreAudio drops it at once, while
    /// `system_profiler` keeps a device among the connected ones for a while.
    fn connected(&self, address: &str) -> bool {
        self.devices.as_ref().is_some_and(|s| s.outputs.iter().chain(&s.inputs).any(|d| d.transport == Transport::Bluetooth && d.id() == address))
    }

    fn bluetooth_action(&mut self, address: String, cx: &mut Cx) {
        if self.busy.contains_key(&address) || self.mock.is_some() {
            return;
        }
        let connect = !self.connected(&address);
        self.busy.insert(address.clone(), connect);
        self.failed.remove(&address);
        cx.redraw();
        let target = address.clone();
        cx.background::<Audio, Outcome>(
            move || connect_bluetooth(&target, connect),
            move |w, outcome, cx| match outcome {
                // Connected once its audio is there, up to 10 s later
                Outcome::Done => w.await_bluetooth(address, connect, 50, cx),
                _ => {
                    w.busy.remove(&address);
                    w.failed.insert(address, outcome);
                    cx.redraw();
                }
            },
        );
    }

    fn await_bluetooth(&mut self, address: String, connect: bool, tries: u32, cx: &mut Cx) {
        if self.connected(&address) == connect || tries == 0 {
            self.busy.remove(&address);
            cx.redraw();
            return;
        }
        cx.after::<Audio>(0.2, move |w, cx| w.await_bluetooth(address, connect, tries - 1, cx));
    }

    fn device_row(&self, rx: &Rx, d: &Device, output: bool, action: String, right: f64) -> Node {
        let th = rx.theme;
        let badges = if output { Badges::Speaker } else { Badges::Microphone };
        let battery = self.battery(d).map(|b| battery_node(rx, b, true)).unwrap_or_default();
        let mut row = Node::row(vec![
            icon_column(rx, &icon(output, badges, d.transport, d.kind, &d.name)),
            Node::text(&d.name, th.font(), th.text).key("name"),
            Node::spacer(),
            Node::row(vec![battery]).min_width(right).justify(Justify::End).key("right"),
        ])
        .gap(8.0)
        .padding(0.0, 8.0, 0.0, 8.0)
        .height(26.0)
        .radius(th.row_radius)
        .key(format!("{}:{}", if output { "out" } else { "in" }, d.uid));
        if d.default {
            row = row.background(th.surface);
        } else {
            row = row.click(rx.action(&action));
        }
        row
    }
}

fn bluetooth_ids(s: &Snapshot) -> Vec<String> {
    s.outputs.iter().chain(&s.inputs).filter(|d| d.transport == Transport::Bluetooth).map(Device::id).collect()
}

/// `Altoparlanti MacBook Pro` and `Microfono MacBook Pro` → `MacBook Pro`: the words they end
/// with, or else start with.
fn common_name(a: &str, b: &str) -> Option<String> {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.split_whitespace().collect(), b.split_whitespace().collect());
    let end = a.iter().rev().zip(b.iter().rev()).take_while(|(x, y)| x == y).count();
    if end > 0 {
        return Some(a[a.len() - end..].join(" "));
    }
    let start = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    (start > 0).then(|| a[..start].join(" "))
}

/// The SF Symbol of a device: the Mac, a display or glasses with badges for what they do (a
/// speaker, a microphone), else the device itself.
fn icon(output: bool, badges: Badges, transport: Transport, kind: Type, name: &str) -> String {
    let jack = matches!(kind, Type::Headphones | Type::Headset);
    let glasses = name.contains("glasses") || name.contains("Glasses");
    let device = match transport {
        Transport::Builtin if jack => None,
        Transport::Builtin if name.contains("MacBook") => Some("laptopcomputer"),
        Transport::Builtin => Some("desktopcomputer"),
        Transport::Display => Some("display"),
        _ if glasses => Some("eyeglasses"),
        _ => None,
    };
    if let Some(device) = device {
        return format!("{device}+{}", badges.symbols());
    }
    let symbol = if name.contains("AirPods Max") {
        "airpods.max"
    } else if name.contains("AirPods Pro") {
        "airpods.pro"
    } else if name.contains("AirPods") {
        "airpods"
    } else if name.contains("Buds") {
        "headphones"
    } else if transport == Transport::Virtual {
        "waveform"
    } else if output {
        match (transport, kind) {
            (_, Type::Headphones | Type::Headset) => "headphones",
            (Transport::Bluetooth, Type::Speaker) => "hifispeaker",
            // Bluetooth devices that don't say they are speakers are nearly always headphones
            (Transport::Bluetooth, _) => "headphones",
            (Transport::Airplay, _) => "airplayaudio",
            _ => "hifispeaker",
        }
    } else if kind == Type::Headset || transport == Transport::Bluetooth {
        "headset"
    } else {
        "music.mic"
    };
    symbol.to_string()
}

fn icon_column(rx: &Rx, symbol: &str) -> Node {
    Node::row(vec![Node::symbol(symbol, 14.0, Weight::Semibold, rx.theme.text)]).width(31.0).justify(Justify::Center).key("icon")
}

fn battery_symbol(level: u8) -> &'static str {
    match level {
        88.. => "battery.100",
        63.. => "battery.75",
        38.. => "battery.50",
        13.. => "battery.25",
        _ => "battery.0",
    }
}

/// The battery glyph and percentage; red at 20% or less, as in the battery menu of macOS: the
/// glyph only in the bar, all of it in the popup.
fn battery_node(rx: &Rx, level: u8, popup: bool) -> Node {
    let th = rx.theme;
    let low = level <= 20;
    let glyph = if low { th.red } else { th.subtext };
    let text = if low && popup { th.red } else { th.subtext };
    Node::row(vec![
        Node::symbol(battery_symbol(level), if popup { 11.0 } else { 12.0 }, Weight::Semibold, glyph),
        Node::text(format!("{level}%"), th.font_with(Weight::Bold, 11.0), text),
    ])
    .gap(4.0)
    .key("battery")
}

impl Widget for Audio {
    fn start(&mut self, cx: &mut Cx) {
        if self.mock.is_none() {
            watch(cx.handle::<Audio>());
        }
        self.read(cx);
        // The batteries of Bluetooth devices: nothing tells when they change
        cx.every::<Audio>(120.0, |w, cx| {
            if w.devices.as_ref().is_some_and(|s| !bluetooth_ids(s).is_empty()) {
                w.read(cx);
                w.profile(cx);
            }
        });
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let s = self.devices.as_ref()?;
        let output = s.outputs.iter().find(|d| d.default);
        let mut input = s.inputs.iter().find(|d| d.default);
        let mut out_name = output.map(|d| d.name.clone());
        let mut badges = Badges::Speaker;
        if let (Some(o), Some(i)) = (output, input) {
            // One device: a headset, or the Mac's speakers and microphone
            let merged = if o.id() == i.id() {
                Some(o.name.clone())
            } else if o.transport == Transport::Builtin && i.transport == Transport::Builtin {
                common_name(&o.name, &i.name)
            } else {
                None
            };
            if merged.is_some() {
                out_name = merged;
                badges = Badges::Both;
                input = None;
            }
        }
        let group = |d: &Device, name: Option<String>, output: bool, badges: Badges, key: &str| {
            let mut children = vec![Node::symbol(icon(output, badges, d.transport, d.kind, &d.name), th.icon_size, Weight::Semibold, th.text).key("icon")];
            if !rx.compact
                && let Some(name) = name
            {
                children.push(Node::text(name, th.font(), th.text).key("name"));
            }
            if let Some(b) = self.battery(d) {
                children.push(battery_node(rx, b, false));
            }
            Node::row(children).gap(5.0).key(key)
        };
        let mut groups = Vec::new();
        if let Some(o) = output {
            groups.push(group(o, out_name, true, badges, "output"));
        }
        if let Some(i) = input {
            groups.push(group(i, Some(i.name.clone()), false, Badges::Microphone, "input"));
        }
        if groups.is_empty() {
            return None;
        }
        Some(rx.pill(groups).gap(14.0).click(rx.toggle_popup()))
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let s = self.devices.as_ref()?;
        let bold = th.font();
        let measure = |text: &str| crate::gfx::measure_text(text, &bold).width;
        // The right column, wide enough for any of its texts, so that they line up
        let actions = [
            t("Connetti", "Connect"),
            t("Disconnetti", "Disconnect"),
            t("Connessione…", "Connecting…"),
            t("Disconnessione…", "Disconnecting…"),
            t("Non riuscito", "Failed"),
            t("Non consentito", "Not Allowed"),
        ];
        let mut right: f64 = 0.0;
        if !self.bluetooth.paired.is_empty() {
            right = actions.iter().map(|a| measure(a)).fold(0.0, f64::max);
        }
        if s.outputs.iter().chain(&s.inputs).any(|d| self.battery(d).is_some()) {
            right = right.max(crate::gfx::measure_text("100%", &th.font_with(Weight::Bold, 11.0)).width + 18.0);
        }

        let mut rows = Vec::new();
        for (output, devices, volume) in [(true, &s.outputs, s.output_volume), (false, &s.inputs, s.input_volume)] {
            let side = if output { "out" } else { "in" };
            let title = if output { t("Uscita", "Output") } else { t("Ingresso", "Input") };
            if volume != Volume::None {
                let percent = match volume {
                    Volume::Level(v) => format!("{v}%"),
                    _ => String::new(),
                };
                rows.push(
                    Node::row(vec![Node::text(title, bold.clone(), th.text), Node::spacer(), Node::text(percent, bold.clone(), th.subtext).key("percent")])
                        .padding(0.0, 8.0, 0.0, 8.0)
                        .height(if output { 30.0 } else { 38.0 })
                        .key(format!("{side}-title")),
                );
                rows.push(match volume {
                    Volume::Level(v) => Node::row(vec![
                        Node::new(crate::ui::Kind::Slider { value: v as f64 / 100.0, color: th.primary, track: th.surface }).grow().click(rx.action(&format!("volume:{side}"))).key("slider"),
                    ])
                    .padding(0.0, 8.0, 0.0, 8.0)
                    .height(24.0)
                    .key(format!("{side}-volume")),
                    _ => Node::row(vec![Node::text(t("Volume regolabile solo dal dispositivo", "Volume set on the device only"), th.font_with(Weight::Medium, 11.0), th.subtext)])
                        .padding(0.0, 8.0, 0.0, 8.0)
                        .height(24.0)
                        .key(format!("{side}-fixed")),
                });
            } else if !output && !devices.is_empty() {
                rows.push(Node::default().height(8.0).key("in-gap"));
            }
            for (i, d) in devices.iter().enumerate() {
                rows.push(self.device_row(rx, d, output, format!("default:{side}:{i}"), right));
            }
        }

        if !self.bluetooth.paired.is_empty() {
            rows.push(Node::row(vec![Node::text("Bluetooth", bold.clone(), th.text)]).padding(0.0, 8.0, 0.0, 8.0).height(38.0).key("bt-title"));
            for (i, p) in self.bluetooth.paired.iter().enumerate() {
                let (text, color) = match (self.busy.get(&p.address), self.failed.get(&p.address)) {
                    (Some(true), _) => (actions[2], th.yellow),
                    (Some(false), _) => (actions[3], th.yellow),
                    (None, Some(Outcome::Denied)) => (actions[5], th.red),
                    (None, Some(_)) => (actions[4], th.red),
                    (None, None) if self.connected(&p.address) => (actions[1], th.primary),
                    (None, None) => (actions[0], th.primary),
                };
                rows.push(
                    Node::row(vec![
                        icon_column(rx, &icon(true, Badges::Speaker, Transport::Bluetooth, p.kind, &p.name)),
                        Node::text(&p.name, bold.clone(), th.text).key("name"),
                        Node::spacer(),
                        Node::row(vec![Node::text(text, bold.clone(), color).key("action")]).min_width(right).justify(Justify::End).key("right"),
                    ])
                    .gap(8.0)
                    .padding(0.0, 8.0, 0.0, 8.0)
                    .height(26.0)
                    .radius(th.row_radius)
                    .click(rx.action(&format!("bt:{i}")))
                    .key(format!("bt:{}", p.address)),
                );
            }
        }
        if rows.is_empty() {
            return None;
        }
        rows.push(Node::default().height(4.0).key("bottom"));
        Some(rx.popup(260.0, rows).padding(4.0, 4.0, 0.0, 4.0).gap(0.0))
    }

    fn ready(&self) -> bool {
        self.devices.is_some()
    }

    fn action(&mut self, name: &str, value: Option<f64>, cx: &mut Cx) {
        let Some(s) = &self.devices else { return };
        let parts: Vec<&str> = name.split(':').collect();
        match parts.as_slice() {
            ["volume", side] => {
                let (Some(value), output) = (value, *side == "out") else { return };
                let percent = (value * 100.0).round().clamp(0.0, 100.0) as u8;
                let devices = if output { &s.outputs } else { &s.inputs };
                let Some(object) = devices.iter().find(|d| d.default).map(|d| d.object) else { return };
                // The title shows it at once; CoreAudio confirms it
                if let Some(s) = &mut self.devices {
                    *(if output { &mut s.output_volume } else { &mut s.input_volume }) = Volume::Level(percent);
                }
                cx.redraw();
                if self.mock.is_none() {
                    cx.background::<Audio, ()>(move || set_volume(object, output, percent), |w, _, cx| w.read(cx));
                }
            }
            ["default", side, i] => {
                let output = *side == "out";
                let devices = if output { &s.outputs } else { &s.inputs };
                let Some(d) = i.parse::<usize>().ok().and_then(|i| devices.get(i)) else { return };
                if d.default || self.mock.is_some() {
                    return;
                }
                let object = d.object;
                cx.background::<Audio, ()>(move || set_default(object, output), |w, _, cx| w.read(cx));
            }
            ["bt", i] => {
                if let Some(p) = i.parse::<usize>().ok().and_then(|i| self.bluetooth.paired.get(i)) {
                    let address = p.address.clone();
                    self.bluetooth_action(address, cx);
                }
            }
            _ => {}
        }
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        match event {
            Event::Wake => {
                self.read(cx);
                if self.devices.as_ref().is_some_and(|s| !bluetooth_ids(s).is_empty()) {
                    self.profile(cx);
                }
            }
            // The batteries and the paired devices, which nothing tells about
            Event::PopupOpened => {
                self.failed.clear();
                self.read(cx);
                self.profile(cx);
            }
            _ => {}
        }
    }

    crate::any_widget!();
}

// CoreAudio

mod ca {
    use std::ffi::c_void;

    use objc2_foundation::NSString;

    use crate::iokit::{CFTypeRef, owned};

    #[repr(C)]
    #[derive(Clone, Copy, PartialEq, Debug)]
    pub struct Address {
        pub selector: u32,
        pub scope: u32,
        pub element: u32,
    }

    pub const fn fc(s: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*s)
    }

    pub const SYSTEM: u32 = 1;
    pub const GLOBAL: u32 = fc(b"glob");
    pub const OUTPUT: u32 = fc(b"outp");
    pub const INPUT: u32 = fc(b"inpt");
    pub const DEVICES: u32 = fc(b"dev#");
    pub const DEFAULT_OUTPUT: u32 = fc(b"dOut");
    pub const DEFAULT_INPUT: u32 = fc(b"dIn ");
    /// Where alerts and sound effects play.
    pub const DEFAULT_SYSTEM_OUTPUT: u32 = fc(b"sOut");
    pub const UID: u32 = fc(b"uid ");
    pub const NAME: u32 = fc(b"lnam");
    pub const TRANSPORT: u32 = fc(b"tran");
    pub const STREAMS: u32 = fc(b"stm#");
    pub const TERMINAL: u32 = fc(b"term");
    pub const DATA_SOURCE: u32 = fc(b"ssrc");
    pub const HIDDEN: u32 = fc(b"hidn");
    pub const CAN_BE_DEFAULT: u32 = fc(b"dflt");
    pub const CAN_BE_SYSTEM: u32 = fc(b"sflt");
    /// The volume of the macOS slider, whether the device has a main volume or one per channel.
    pub const MAIN_VOLUME: u32 = fc(b"vmvc");
    pub const VOLUME: u32 = fc(b"volm");
    pub const MUTE: u32 = fc(b"mute");

    pub type Listener = extern "C" fn(u32, u32, *const Address, *mut c_void) -> i32;

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectHasProperty(id: u32, address: *const Address) -> u8;
        fn AudioObjectIsPropertySettable(id: u32, address: *const Address, settable: *mut u8) -> i32;
        fn AudioObjectGetPropertyDataSize(id: u32, address: *const Address, qualifier_size: u32, qualifier: *const c_void, size: *mut u32) -> i32;
        fn AudioObjectGetPropertyData(id: u32, address: *const Address, qualifier_size: u32, qualifier: *const c_void, size: *mut u32, data: *mut c_void) -> i32;
        fn AudioObjectSetPropertyData(id: u32, address: *const Address, qualifier_size: u32, qualifier: *const c_void, size: u32, data: *const c_void) -> i32;
        pub fn AudioObjectAddPropertyListener(id: u32, address: *const Address, listener: Listener, data: *mut c_void) -> i32;
        pub fn AudioObjectRemovePropertyListener(id: u32, address: *const Address, listener: Listener, data: *mut c_void) -> i32;
    }

    pub fn address(selector: u32, scope: u32, element: u32) -> Address {
        Address { selector, scope, element }
    }

    pub fn has(id: u32, a: Address) -> bool {
        unsafe { AudioObjectHasProperty(id, &a) != 0 }
    }

    pub fn settable(id: u32, a: Address) -> bool {
        let mut settable = 0u8;
        has(id, a) && unsafe { AudioObjectIsPropertySettable(id, &a, &mut settable) } == 0 && settable != 0
    }

    pub fn get<T: Copy + Default>(id: u32, a: Address) -> Option<T> {
        let mut value = T::default();
        let mut size = size_of::<T>() as u32;
        let status = unsafe { AudioObjectGetPropertyData(id, &a, 0, std::ptr::null(), &mut size, &mut value as *mut T as *mut c_void) };
        (status == 0 && size as usize == size_of::<T>()).then_some(value)
    }

    pub fn set<T: Copy>(id: u32, a: Address, value: T) -> bool {
        unsafe { AudioObjectSetPropertyData(id, &a, 0, std::ptr::null(), size_of::<T>() as u32, &value as *const T as *const c_void) == 0 }
    }

    pub fn size(id: u32, a: Address) -> u32 {
        let mut size = 0;
        match unsafe { AudioObjectGetPropertyDataSize(id, &a, 0, std::ptr::null(), &mut size) } {
            0 => size,
            _ => 0,
        }
    }

    pub fn objects(id: u32, a: Address) -> Vec<u32> {
        let mut size = size(id, a);
        let mut ids = vec![0u32; size as usize / 4];
        if ids.is_empty() || unsafe { AudioObjectGetPropertyData(id, &a, 0, std::ptr::null(), &mut size, ids.as_mut_ptr() as *mut c_void) } != 0 {
            return Vec::new();
        }
        ids.truncate(size as usize / 4);
        ids
    }

    pub fn string(id: u32, a: Address) -> Option<String> {
        let mut cf: CFTypeRef = std::ptr::null();
        let mut size = size_of::<CFTypeRef>() as u32;
        if unsafe { AudioObjectGetPropertyData(id, &a, 0, std::ptr::null(), &mut size, &mut cf as *mut CFTypeRef as *mut c_void) } != 0 {
            return None;
        }
        unsafe { owned::<NSString>(cf) }.map(|s| s.to_string())
    }
}

use ca::{Address, GLOBAL, INPUT, OUTPUT, SYSTEM, address};

/// The widgets to tell, and the volumes listened to (those of the devices in use).
struct Watch {
    handles: Vec<Handle<Audio>>,
    system: bool,
    volumes: Vec<(u32, Address)>,
}

static WATCH: Mutex<Watch> = Mutex::new(Watch { handles: Vec::new(), system: false, volumes: Vec::new() });

extern "C" fn audio_changed(_id: u32, _count: u32, _addresses: *const Address, _data: *mut c_void) -> i32 {
    let handles = WATCH.lock().map(|w| w.handles.clone()).unwrap_or_default();
    for h in handles {
        h.send(|w, cx| w.changed(cx));
    }
    0
}

/// Listens to the devices and the defaults; the volumes are listened to by `read_devices`.
fn watch(handle: Handle<Audio>) {
    let Ok(mut watch) = WATCH.lock() else { return };
    watch.handles.retain(|h| h.alive());
    watch.handles.push(handle);
    if !watch.system {
        watch.system = true;
        for selector in [ca::DEVICES, ca::DEFAULT_OUTPUT, ca::DEFAULT_INPUT] {
            unsafe { ca::AudioObjectAddPropertyListener(SYSTEM, &address(selector, GLOBAL, 0), audio_changed, std::ptr::null_mut()) };
        }
    }
}

/// The volume of the devices in use, which also changes with the volume keys. The macOS slider's
/// own isn't reliable to listen to, so every one they have.
fn watch_volumes(devices: &[(u32, u32)]) {
    let Ok(mut watch) = WATCH.lock() else { return };
    let mut wanted = Vec::new();
    for &(id, scope) in devices {
        for a in [address(ca::VOLUME, scope, 0), address(ca::VOLUME, scope, 1), address(ca::VOLUME, scope, 2), address(ca::MUTE, scope, 0), address(ca::MAIN_VOLUME, scope, 0)] {
            if ca::has(id, a) {
                wanted.push((id, a));
            }
        }
    }
    for old in watch.volumes.iter().filter(|o| !wanted.contains(o)) {
        unsafe { ca::AudioObjectRemovePropertyListener(old.0, &old.1, audio_changed, std::ptr::null_mut()) };
    }
    for new in wanted.iter().filter(|n| !watch.volumes.contains(n)) {
        unsafe { ca::AudioObjectAddPropertyListener(new.0, &new.1, audio_changed, std::ptr::null_mut()) };
    }
    watch.volumes = wanted;
}

fn read_devices() -> Snapshot {
    let defaults = [ca::get::<u32>(SYSTEM, address(ca::DEFAULT_OUTPUT, GLOBAL, 0)).unwrap_or(0), ca::get::<u32>(SYSTEM, address(ca::DEFAULT_INPUT, GLOBAL, 0)).unwrap_or(0)];
    watch_volumes(&[(defaults[0], OUTPUT), (defaults[1], INPUT)]);
    // The batteries that IOKit knows (headsets over HFP), by name
    let batteries: HashMap<String, u8> = crate::iokit::power_sources(true)
        .iter()
        .filter_map(|d| {
            let name = crate::iokit::string(d, objc2_foundation::ns_string!("Name"))?;
            let level = crate::iokit::number(d, objc2_foundation::ns_string!("Current Capacity"))?;
            let max = crate::iokit::number(d, objc2_foundation::ns_string!("Max Capacity")).filter(|&m| m > 0).unwrap_or(100);
            Some((name, (level * 100 / max).clamp(0, 100) as u8))
        })
        .collect();
    let all = ca::objects(SYSTEM, address(ca::DEVICES, GLOBAL, 0));
    let list = |scope: u32, default: u32| -> Vec<Device> { all.iter().filter_map(|&id| device(id, scope, default, &batteries)).collect() };
    Snapshot {
        outputs: list(OUTPUT, defaults[0]),
        inputs: list(INPUT, defaults[1]),
        output_volume: volume(defaults[0], OUTPUT),
        input_volume: volume(defaults[1], INPUT),
    }
}

/// A device as System Settings lists it for a direction: not hidden, not one of an app (Microsoft
/// Teams Audio…), with streams that way.
fn device(id: u32, scope: u32, default: u32, batteries: &HashMap<String, u8>) -> Option<Device> {
    if ca::size(id, address(ca::STREAMS, scope, 0)) == 0
        || ca::get::<u32>(id, address(ca::HIDDEN, GLOBAL, 0)) == Some(1)
        || ca::get::<u32>(id, address(ca::CAN_BE_DEFAULT, scope, 0)) != Some(1)
    {
        return None;
    }
    let uid = ca::string(id, address(ca::UID, GLOBAL, 0))?;
    let name = ca::string(id, address(ca::NAME, GLOBAL, 0)).unwrap_or_default();
    let transport = match &ca::get::<u32>(id, address(ca::TRANSPORT, GLOBAL, 0)).unwrap_or(0).to_be_bytes() {
        b"bltn" => Transport::Builtin,
        b"blue" | b"blea" => Transport::Bluetooth,
        b"dprt" | b"hdmi" => Transport::Display,
        b"airp" => Transport::Airplay,
        b"virt" | b"grup" | b"fgrp" => Transport::Virtual,
        _ => Transport::External,
    };
    let battery = (transport == Transport::Bluetooth).then(|| batteries.get(&name).copied()).flatten();
    Some(Device { object: id, uid, kind: kind(id, scope), name, transport, battery, default: id == default })
}

/// What the device is, from its first stream: CoreAudio's codes or, for USB devices, those of the
/// USB Audio spec.
fn kind(id: u32, scope: u32) -> Type {
    let terminal = ca::objects(id, address(ca::STREAMS, scope, 0)).first().and_then(|&s| ca::get::<u32>(s, address(ca::TERMINAL, GLOBAL, 0))).unwrap_or(0);
    match (&terminal.to_be_bytes(), terminal) {
        (b"hdph", _) | (_, 0x0302) => Type::Headphones,
        (b"hmic", _) | (_, 0x0401 | 0x0402) => Type::Headset,
        (b"spkr" | b"lfes" | b"rspk", _) | (_, 0x0301 | 0x0304..=0x0307 | 0x0403..=0x0405) => Type::Speaker,
        (b"micr" | b"rmic", _) | (_, 0x0201..=0x0206) => Type::Microphone,
        // Intel Macs switch the built-in output to the headphone jack as a data source
        _ if ca::get::<u32>(id, address(ca::DATA_SOURCE, scope, 0)) == Some(ca::fc(b"hdpn")) => Type::Headphones,
        _ => Type::Unknown,
    }
}

fn volume(id: u32, scope: u32) -> Volume {
    if id == 0 {
        return Volume::None;
    }
    let main = address(ca::MAIN_VOLUME, scope, 0);
    let Some(v) = ca::settable(id, main).then(|| ca::get::<f32>(id, main)).flatten() else { return Volume::Fixed };
    // Muted, the slider of macOS is empty too
    if ca::get::<u32>(id, address(ca::MUTE, scope, 0)) == Some(1) {
        return Volume::Level(0);
    }
    Volume::Level((v as f64 * 100.0).round().clamp(0.0, 100.0) as u8)
}

fn set_volume(id: u32, output: bool, percent: u8) {
    let scope = if output { OUTPUT } else { INPUT };
    if !ca::set(id, address(ca::MAIN_VOLUME, scope, 0), percent as f32 / 100.0) {
        return;
    }
    // Like the volume keys, raising the volume unmutes
    let mute = address(ca::MUTE, scope, 0);
    if percent > 0 && ca::settable(id, mute) {
        ca::set(id, mute, 0u32);
    }
}

fn set_default(id: u32, output: bool) {
    let global = |s| address(s, GLOBAL, 0);
    if !output {
        ca::set(SYSTEM, global(ca::DEFAULT_INPUT), id);
        return;
    }
    // Alerts and sound effects move with the output, unless they play on another device
    let effects = ca::get::<u32>(SYSTEM, global(ca::DEFAULT_SYSTEM_OUTPUT)) == ca::get::<u32>(SYSTEM, global(ca::DEFAULT_OUTPUT))
        && ca::get::<u32>(id, address(ca::CAN_BE_SYSTEM, OUTPUT, 0)) == Some(1);
    if ca::set(SYSTEM, global(ca::DEFAULT_OUTPUT), id) && effects {
        ca::set(SYSTEM, global(ca::DEFAULT_SYSTEM_OUTPUT), id);
    }
}

// Bluetooth

/// The paired headphones, headsets and speakers, and the batteries of the connected ones, which
/// CoreAudio doesn't know. None if `system_profiler` fails.
fn read_bluetooth() -> Option<Bluetooth> {
    let out = std::process::Command::new("/usr/sbin/system_profiler").args(["SPBluetoothDataType", "-json"]).output().ok()?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let mut b = Bluetooth::default();
    let percent = |v: &serde_json::Value| v.as_str().and_then(|s| s.trim_end_matches('%').parse::<u8>().ok());
    for controller in json["SPBluetoothDataType"].as_array()? {
        // Bluetooth off: no devices to connect
        if controller["controller_properties"]["controller_state"] != "attrib_on" {
            continue;
        }
        for list in ["device_connected", "device_not_connected"] {
            for entry in controller[list].as_array().into_iter().flatten() {
                for (name, d) in entry.as_object().into_iter().flatten() {
                    let Some(address) = d["device_address"].as_str() else { continue };
                    let address = address.to_uppercase().replace(':', "-");
                    // The lower of the two earbuds in use
                    let sides: Vec<u8> = ["device_batteryLevelLeft", "device_batteryLevelRight"].iter().filter_map(|k| percent(&d[*k])).collect();
                    if let Some(level) = sides.iter().min().copied().or_else(|| percent(&d["device_batteryLevelMain"])) {
                        b.batteries.insert(address.clone(), level);
                    }
                    let kind = match d["device_minorType"].as_str() {
                        Some("Headphones") => Type::Headphones,
                        Some("Headset" | "Hands-free") => Type::Headset,
                        Some("Speaker" | "Loudspeaker" | "Portable Audio" | "Car Audio" | "HiFi Audio") => Type::Speaker,
                        Some("Microphone") => Type::Microphone,
                        _ => continue,
                    };
                    b.paired.push(Paired { name: name.clone(), address, kind });
                }
            }
        }
    }
    // As the Finder sorts: rows don't move when a device connects
    b.paired.sort_by(|x, y| NSString::from_str(&x.name).localizedStandardCompare(&NSString::from_str(&y.name)).into());
    Some(b)
}

/// Connects or disconnects a paired device. The first time macOS asks for the Bluetooth
/// permission; the device's audio then comes or goes by itself, which CoreAudio tells.
fn connect_bluetooth(address: &str, connect: bool) -> Outcome {
    unsafe {
        if libc::dlopen(c"/System/Library/Frameworks/CoreBluetooth.framework/CoreBluetooth".as_ptr(), libc::RTLD_NOW).is_null() {
            return Outcome::Failed;
        }
        let Some(manager_class) = AnyClass::get(c"CBManager") else { return Outcome::Failed };
        let authorization = || -> isize { msg_send![manager_class, authorization] };
        if authorization() == 0 {
            // A central manager makes macOS ask; the answer may take a while
            let central = AnyClass::get(c"CBCentralManager");
            let _manager: Option<Retained<AnyObject>> = central.map(|c| {
                let allocated: objc2::rc::Allocated<AnyObject> = msg_send![c, alloc];
                msg_send![allocated, initWithDelegate: std::ptr::null::<AnyObject>(), queue: std::ptr::null::<AnyObject>()]
            });
            for _ in 0..300 {
                if authorization() != 0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
        // 3: allowed always
        if authorization() != 3 {
            return Outcome::Denied;
        }
        let Some(device) = objc2_io_bluetooth::IOBluetoothDevice::deviceWithAddressString(Some(&NSString::from_str(address))) else { return Outcome::Failed };
        // Waits for the device to answer, or gives up after a few seconds when it's off or away
        let status = if connect { device.openConnection() } else { device.closeConnection() };
        if status == 0 { Outcome::Done } else { Outcome::Failed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_icons() {
        assert_eq!(common_name("Altoparlanti MacBook Pro", "Microfono MacBook Pro").as_deref(), Some("MacBook Pro"));
        assert_eq!(common_name("MacBook Pro Speakers", "MacBook Pro Microphone").as_deref(), Some("MacBook Pro"));
        assert_eq!(common_name("External Headphones", "MacBook Pro Microphone"), None);
        assert_eq!(icon(true, Badges::Both, Transport::Builtin, Type::Speaker, "MacBook Pro"), "laptopcomputer+speaker.fill+mic.fill");
        assert_eq!(icon(true, Badges::Speaker, Transport::Builtin, Type::Headphones, "External Headphones"), "headphones");
        assert_eq!(icon(true, Badges::Speaker, Transport::Bluetooth, Type::Unknown, "AirPods Pro"), "airpods.pro");
        assert_eq!(icon(false, Badges::Microphone, Transport::Bluetooth, Type::Headset, "Headset"), "headset");
        assert_eq!(icon(true, Badges::Speaker, Transport::Bluetooth, Type::Speaker, "Boom"), "hifispeaker");
        assert_eq!(battery_symbol(90), "battery.100");
        assert_eq!(battery_symbol(12), "battery.0");
    }
}
