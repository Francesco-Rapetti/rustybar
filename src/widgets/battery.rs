//! The battery of the Mac: the charge with an icon for its state, and a dashboard in the popup.
//! On top the state in words and the charge, with a bar that marks the limit; then the Charge
//! Limit of System Settings → Battery (macOS 26.4+) as a slider that stops at its steps, with
//! Charge to Full Now under it; tiles with the maximum capacity and the condition, the cycles
//! against the ones the battery is made for, the temperature and the power the Mac draws; the
//! adapter, while one is plugged in; the Battery settings.
//!
//! The icon: on battery the level, yellow in Low Power Mode or else red at 20% or less, as in the
//! battery menu of macOS; charging, the bolt; plugged in but not charging, the plug, yellow when
//! macOS doesn't say why. Green is a battery macOS looks after (charging up to the limit, holding
//! the charge at it, Optimized Battery Charging, the desktop mode); yellow is also the bolt or the
//! plug while Charge to Full Now pauses the limit.
//!
//! IOKit says when the battery changes and PowerUIAgent when the limit does, so nothing polls. The
//! Charge Limit is PowerUIAgent's, asked through the private framework of System Settings. The
//! temperature and the power change all the time and have no event: they're read from IOKit only
//! while the popup is open, every 2 seconds.
//!
//! With made-up data (see `mock`) the state comes from `battery.json`, and the slider and the
//! buttons change only that, in memory.

use std::ffi::{c_char, c_int, c_void};
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{AnyThread, msg_send};
use objc2_foundation::{NSProcessInfo, NSString, NSUserDefaults, ns_string};

use serde::Deserialize;

use super::{Cx, Event, Rx, Widget};
use crate::gfx::{Rgba, Weight};
use crate::i18n::t;
use crate::runtime;
use crate::ui::{Align, Justify, Kind, Node};

const LIMITS: [u8; 5] = [80, 85, 90, 95, 100];
/// Of `isMCLCurrentlyEnabled:`: on, or paused by Charge to Full Now.
const MCL_ON: usize = 1;
const MCL_PAUSED: usize = 3;

/// The dashboard, as the network and Docker ones: two tiles a row.
const TILE: f64 = 160.0;
const GAP: f64 = 8.0;
const WIDE: f64 = TILE * 2.0 + GAP;
/// A tile's content, inside its padding.
const INNER: f64 = TILE - 20.0;
const STAT: f64 = 54.0;
/// A tile with a bar under its value.
const BARRED: f64 = 64.0;
const LINE: f64 = 50.0;
const SLIDER: f64 = 22.0;
/// How often the temperature and the power are read while the popup is open.
const LIVE: f64 = 2.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum LimitState {
    /// A Mac without the Charge Limit.
    #[default]
    None,
    Off,
    On,
    Paused,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Status {
    percent: i64,
    ac: bool,
    charging: bool,
    charged: bool,
    /// To empty on battery, to full while charging; -1 while macOS works it out.
    minutes: i64,
    /// 80…100, 100 for none. While paused, the one it goes back to (0 if never seen on).
    limit: u8,
    limit_state: LimitState,
    /// macOS holds the charge: the limit, Optimized Battery Charging or the desktop mode.
    held: bool,
    desktop: bool,
    low_power: bool,
    cycles: Option<i64>,
    /// The cycles the battery is made for: 1000 on Apple's.
    design_cycles: Option<i64>,
    /// While one is plugged in.
    adapter: Option<Adapter>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Adapter {
    watts: i64,
    /// The voltage it gives, e.g. 20 V over USB-C.
    volts: Option<f64>,
    /// `USB-C` or `MagSafe`, from its name.
    kind: Option<String>,
}

/// What changes all the time, read while the popup is open.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Live {
    /// Of the battery, in °C.
    temperature: Option<f64>,
    /// What the Mac uses, in W.
    system: Option<f64>,
    /// Into the battery, in W; less than 0 out of it.
    battery: Option<f64>,
    /// From the adapter, in W.
    input: Option<f64>,
}

/// Battery Health as System Settings shows it: "Good" or another condition, and the maximum
/// capacity (`"92%"`).
#[derive(Default)]
struct Health {
    condition: Option<String>,
    max_capacity: Option<String>,
}

/// Made-up data (see `mock`): the state, the condition, the maximum capacity and the live values.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    #[serde(flatten)]
    status: Status,
    health: Option<String>,
    max_capacity: Option<String>,
    live: Live,
}

#[derive(Default)]
pub struct Battery {
    mock: bool,
    status: Option<Status>,
    loaded: bool,
    /// From system_profiler, the only one that says it: at most once a day, when the popup opens.
    health: Option<(Health, Instant)>,
    /// The last values read, kept when the popup closes so it opens again without a jump.
    live: Option<Live>,
    /// Bumped when the popup opens or closes: the reads of an older one stop.
    ticking: u64,
    reading: bool,
    queued: bool,
    /// The action macOS refused (`limit` for the slider, `limit:80` or `full` for the button),
    /// until the next update.
    failed: Option<String>,
    source: Option<RunLoopSource>,
    token: Option<c_int>,
    observer: Option<Retained<objc2::runtime::ProtocolObject<dyn objc2_foundation::NSObjectProtocol>>>,
}

impl Battery {
    /// Reads the battery again on a worker thread (PowerUIAgent answers over XPC), coalescing the
    /// bursts of notifications, e.g. when the adapter is plugged in.
    fn update(&mut self, cx: &mut Cx) {
        if self.reading {
            self.queued = true;
            return;
        }
        self.reading = true;
        cx.after::<Battery>(0.2, |w, cx| {
            cx.background::<Battery, _>(read_status, |w, status, cx| {
                w.reading = false;
                w.status = status;
                w.loaded = true;
                w.failed = None;
                cx.redraw();
                if std::mem::take(&mut w.queued) {
                    w.update(cx);
                }
            });
            let _ = w;
        });
    }

    /// Every 2 seconds while the popup is open: the temperature and the power.
    fn tick(&mut self, ticking: u64, cx: &mut Cx) {
        if ticking != self.ticking || !cx.popup_open {
            return;
        }
        cx.background::<Battery, _>(read_live, move |w, live, cx| {
            if ticking != w.ticking {
                return;
            }
            w.live = Some(live);
            cx.redraw();
            cx.after::<Battery>(LIVE, move |w, cx| w.tick(ticking, cx));
        });
    }

    fn icon(&self, rx: &Rx, s: &Status) -> (String, Rgba) {
        let th = rx.theme;
        let level = match s.percent {
            88.. => "battery.100",
            63.. => "battery.75",
            38.. => "battery.50",
            13.. => "battery.25",
            _ => "battery.0",
        };
        if !s.ac {
            let color = if s.low_power {
                th.yellow
            } else if s.percent <= 20 {
                th.red
            } else {
                th.text
            };
            return (level.into(), color);
        }
        if s.charging {
            let color = match s.limit_state {
                LimitState::On => th.green,
                LimitState::Paused => th.yellow,
                _ => th.text,
            };
            return ("battery.100.bolt".into(), color);
        }
        let mut color = if s.charged || s.percent >= 100 {
            th.text
        } else if (s.limit_state == LimitState::On && s.percent >= s.limit as i64 - 5) || s.held {
            th.green
        } else {
            th.yellow
        };
        if s.limit_state == LimitState::Paused {
            color = th.yellow;
        }
        ("powerplug.fill".into(), color)
    }

    /// The state in words, its color and the details under it.
    fn state(&self, rx: &Rx, s: &Status) -> (String, Rgba, String) {
        let th = rx.theme;
        let mut details: Vec<String> = Vec::new();
        let (status, color) = if !s.ac {
            details.push(if s.minutes > 0 { t(&format!("Ancora {}", duration(s.minutes)), &format!("{} left", duration(s.minutes))).into() } else { t("Calcolo del tempo rimanente…", "Calculating time remaining…").into() });
            if s.low_power {
                details.push(t("risparmio energetico", "Low Power Mode").into());
            }
            (t("A batteria", "On Battery").to_string(), th.text)
        } else if s.charging {
            match s.limit_state {
                LimitState::On => details.push(t(&format!("Fino al limite ({}%)", s.limit), &format!("Up to the limit ({}%)", s.limit)).into()),
                LimitState::Paused => details.push(t("Fino al 100%", "Up to 100%").into()),
                _ if s.minutes > 0 => details.push(t(&format!("Completa tra {}", duration(s.minutes)), &format!("Full in {}", duration(s.minutes))).into()),
                _ => {}
            }
            (t("In carica", "Charging").to_string(), th.text)
        } else if s.charged || s.percent >= 100 {
            (t("Carica completa", "Fully Charged").to_string(), th.text)
        } else if s.limit_state == LimitState::On && s.percent >= s.limit as i64 - 5 {
            // Apple: the Mac stops within a few percent of the limit and charges again 5% below it
            (t(&format!("Ferma al limite ({}%)", s.limit), &format!("Held at the limit ({}%)", s.limit)).to_string(), th.text)
        } else if s.held {
            details.push(if s.desktop { t("Modalità desktop", "Desktop mode") } else { t("Caricamento ottimizzato", "Optimized Battery Charging") }.into());
            (t("Carica sospesa", "Charging On Hold").to_string(), th.text)
        } else {
            (t("La batteria non è in carica", "Battery Is Not Charging").to_string(), th.yellow)
        };
        (status, color, capitalize(&details.join(" · ")))
    }

    // Drawing

    /// Two tiles wide: the icon, the state in words and the charge, over a bar of the charge
    /// with a mark at the limit. The bar has the color of the icon, as in the battery menu of
    /// macOS, and the tile a little of it unless that's the text's.
    fn header(&self, rx: &Rx, s: &Status) -> Node {
        let th = rx.theme;
        let (icon, color) = self.icon(rx, s);
        let (status, status_color, details) = self.state(rx, s);
        let inner = WIDE - 24.0;
        let percent = format!("{}%", s.percent);
        let big = th.font_with(Weight::Semibold, 20.0);
        // The padding, the icon, the gaps and the charge
        let names = inner - 26.0 - 20.0 - crate::gfx::measure_text(&percent, &big).width;
        let mut lines = vec![Node::text(status, th.font(), status_color).max_width(names).key("state")];
        if !details.is_empty() {
            lines.push(Node::text(details, th.small_font(), th.subtext).max_width(names).key("details"));
        }
        let top = Node::row(vec![
            Node::row(vec![Node::symbol(icon, 18.0, Weight::Semibold, color)]).width(26.0).justify(Justify::Center).key("icon"),
            Node::column(lines).gap(2.0).align(Align::Start).key("names"),
            Node::spacer(),
            Node::text(percent, big, th.text).key("percent"),
        ])
        .gap(10.0)
        .width(inner)
        .key("top");
        let mut bar = vec![Node::new(Kind::Progress { value: s.percent as f64 / 100.0, color, track: th.surface }).size(inner, 6.0).key("level")];
        if matches!(s.limit_state, LimitState::On | LimitState::Paused) && (LIMITS[0]..100).contains(&s.limit) {
            let x = (inner * s.limit as f64 / 100.0 - 1.0).round();
            bar.push(Node::default().size(2.0, 12.0).radius(1.0).background(th.subtext).at(x, -3.0).key("mark"));
        }
        Node::column(vec![top, Node::row(bar).size(inner, 6.0).key("bar")])
            .gap(12.0)
            .padding(10.0, 12.0, 12.0, 12.0)
            .width(WIDE)
            .radius(10.0)
            .background(th.tile(color, color != th.text))
            .key("header")
    }

    /// The Charge Limit: a slider with its steps, and what pauses it. Then Charge to Full Now, or
    /// while it's paused the button that turns the limit back on.
    fn limit(&self, rx: &Rx, s: &Status) -> Vec<Node> {
        let th = rx.theme;
        let paused = s.limit_state == LimitState::Paused;
        let note = if self.failed.as_deref() == Some("limit") {
            Some((t("Non riuscito", "Failed"), th.red))
        } else if paused {
            Some((t("Sospeso", "Paused"), th.yellow))
        } else {
            None
        };
        let mut top = vec![Node::text(t("Limite di carica", "Charge Limit"), th.small_font(), th.subtext).key("label"), Node::spacer()];
        if let Some((text, color)) = note {
            top.push(Node::text(text, th.small_font(), color).key("note"));
        }
        let labels = LIMITS.iter().map(|&l| if l == 100 { t("Nessun limite", "No Limit").to_string() } else { format!("{l}%") }).collect();
        let step = match s.limit_state {
            LimitState::Off => LIMITS.len() - 1,
            _ => LIMITS.iter().position(|&l| l == s.limit).unwrap_or(LIMITS.len() - 1),
        };
        // While paused a step would turn the limit back on, at another one
        let action = (!paused || s.limit == 0).then(|| rx.action("limit"));
        let mut column = vec![
            Node::row(top).width(WIDE - 20.0).height(14.0).key("top"),
            rx.steps(labels, step, action).size(WIDE - 20.0, SLIDER).key("slider"),
            self.steps(rx, step).key("steps"),
        ];
        if paused {
            column.push(Node::text(t("Per Completa carica ora: macOS lo riattiva più tardi", "For Charge to Full Now: macOS turns it back on later"), th.small_font(), th.subtext).max_width(WIDE - 20.0).key("paused"));
        }
        let mut rows = vec![Node::column(column).gap(6.0).align(Align::Start).padding(9.0, 10.0, 10.0, 10.0).width(WIDE).radius(10.0).background(th.tile(th.primary, false)).key("limit")];

        let full = match s.limit_state {
            LimitState::Paused if s.limit > 0 => Some(("arrow.uturn.backward", t("Riattiva il limite ora", "Turn the Limit Back On Now"), format!("limit:{}", s.limit))),
            LimitState::On if s.ac && s.percent < 100 => Some(("bolt.fill", t("Completa carica ora", "Charge to Full Now"), "full".to_string())),
            _ => None,
        };
        if let Some((symbol, label, action)) = full {
            let failed = self.failed.as_deref() == Some(action.as_str());
            let (symbol, label, color, text) = if failed { ("exclamationmark.circle.fill", t("Non riuscito", "Failed"), th.red, th.red) } else { (symbol, label, th.primary, th.text) };
            rows.push(
                Node::row(vec![Node::symbol(symbol, 11.0, Weight::Bold, color).key("icon"), Node::text(label, th.font_with(Weight::Bold, 12.0), text).key("label")])
                    .gap(6.0)
                    .justify(Justify::Center)
                    .size(WIDE, 30.0)
                    .radius(15.0)
                    .background(th.surface.faded(0.7))
                    .click(rx.action(&action))
                    .key("full"),
            );
        }
        rows
    }

    /// The limits under the slider, as System Settings has them, each under the dot of its step.
    fn steps(&self, rx: &Rx, at: usize) -> Node {
        let th = rx.theme;
        let (width, travel) = (WIDE - 20.0, WIDE - 20.0 - SLIDER);
        let font = th.small_font();
        let labels = LIMITS.iter().enumerate().map(|(i, l)| {
            let text = format!("{l}%");
            let w = crate::gfx::measure_text(&text, &font).width.ceil();
            // Centered under the step, the first and the last inside the tile
            let center = SLIDER / 2.0 + travel * i as f64 / (LIMITS.len() - 1) as f64;
            let x = (center - w / 2.0).clamp(0.0, width - w).round();
            Node::text(text, font.clone(), if i == at { th.text } else { th.subtext }).at(x, 0.0).key(format!("step-{l}"))
        });
        Node::row(labels.collect()).size(width, 14.0)
    }

    /// The maximum capacity and the condition, the cycles, the temperature and the power.
    fn tiles(&self, rx: &Rx, s: &Status) -> Vec<Node> {
        let th = rx.theme;
        let value = |text: String, color: Rgba| Node::text(text, th.font_with(Weight::Medium, 14.0), color);
        let note = |text: String, color: Rgba| Some(Node::text(text, th.small_font(), color));
        let mut tiles = Vec::new();

        // The condition and the maximum capacity once system_profiler says them
        let health = self.health.as_ref().map(|(h, _)| h);
        let good = health.and_then(|h| h.condition.as_deref()).map(|c| c == "Good");
        let warn = if good == Some(false) { th.yellow } else { th.text };
        let capacity = health.and_then(|h| h.max_capacity.clone());
        let fraction = capacity.as_deref().and_then(|c| c.trim_end_matches('%').trim().parse::<f64>().ok()).map(|p| p / 100.0);
        let condition = good.and_then(|g| if g { note(t("Normale", "Normal").into(), th.subtext) } else { note(t("Assistenza", "Service").into(), th.yellow) });
        let bar = Some((fraction.unwrap_or(0.0), if good == Some(false) { th.yellow } else { th.green }));
        tiles.push(stat(rx, t("Capacità", "Capacity"), condition, value(capacity.unwrap_or_else(|| "…".into()), warn), bar).key("capacity"));

        if let Some(c) = s.cycles {
            let of = s.design_cycles.filter(|d| *d > 0);
            let used = of.map(|d| c as f64 / d as f64);
            let color = used.map(|u| th.level(u, 0.8, 1.0, th.green)).unwrap_or(th.green);
            let total = of.and_then(|d| note(t(&format!("su {d}"), &format!("of {d}")).into(), th.subtext));
            tiles.push(stat(rx, t("Cicli", "Cycles"), total, value(c.to_string(), th.text), Some((used.unwrap_or(0.0), color))).key("cycles"));
        }

        let live = self.live.clone().unwrap_or_default();
        if let Some(c) = live.temperature {
            let (word, color) = match c {
                45.0.. => (t("Molto calda", "Hot"), th.red),
                40.0.. => (t("Calda", "Warm"), th.yellow),
                _ => (t("Normale", "Normal"), th.subtext),
            };
            let tint = if color == th.subtext { th.text } else { color };
            tiles.push(stat(rx, t("Temperatura", "Temperature"), note(word.into(), color), value(temperature(c), tint), None).key("temperature"));
        }

        // What the Mac uses; on an Intel Mac, on battery, what comes out of the battery
        let used = live.system.or(live.battery.filter(|_| !s.ac).map(|b| -b));
        if let Some(w) = used {
            let flow = match live.battery {
                Some(b) if b >= 0.5 => note(t(&format!("Batteria +{b:.0} W"), &format!("Battery +{b:.0} W")).into(), th.green),
                Some(b) if b <= -0.5 && s.ac => note(t(&format!("Batteria −{:.0} W", -b), &format!("Battery −{:.0} W", -b)).into(), th.yellow),
                _ if !s.ac => note(t("Dalla batteria", "From battery").into(), th.subtext),
                _ => None,
            };
            tiles.push(stat(rx, t("Consumo", "Power"), flow, value(watts(w), th.text), None).key("power"));
        }
        tiles
    }

    /// The adapter, two tiles wide: its power and what it is, and what it gives now.
    fn adapter(&self, rx: &Rx, a: &Adapter) -> Node {
        let th = rx.theme;
        let detail: Vec<String> = [a.kind.clone(), a.volts.map(|v| format!("{} V", decimal(v, if v.fract() == 0.0 { 0 } else { 1 })))].into_iter().flatten().collect();
        let detail = if detail.is_empty() { t("Collegato", "Connected").to_string() } else { detail.join(" · ") };
        let input = self.live.as_ref().and_then(|l| l.input).filter(|w| *w > 0.0);
        let mut row = vec![
            Node::row(vec![Node::symbol("powerplug.fill", 15.0, Weight::Semibold, th.primary)]).width(26.0).justify(Justify::Center).key("icon"),
            Node::column(vec![
                Node::text(t(&format!("Alimentatore da {} W", a.watts), &format!("{} W Power Adapter", a.watts)), th.font(), th.text).max_width(WIDE - 170.0).key("name"),
                Node::text(detail, th.small_font(), th.subtext).max_width(WIDE - 170.0).key("detail"),
            ])
            .gap(2.0)
            .align(Align::Start)
            .key("names"),
            Node::spacer(),
        ];
        if let Some(w) = input {
            row.push(
                Node::column(vec![Node::text(watts(w), th.font_with(Weight::Medium, 14.0), th.text).key("value"), Node::text(t("in ingresso", "drawing"), th.small_font(), th.subtext).key("label")])
                    .gap(2.0)
                    .align(Align::End)
                    .key("input"),
            );
        }
        Node::row(row).gap(10.0).padding(8.0, 12.0, 8.0, 10.0).size(WIDE, LINE).radius(10.0).background(th.tile(th.primary, false)).key("adapter")
    }
}

/// A tile of the dashboard: a label with a note on its right, the value under them and, with
/// `bar`, a bar of a fraction under the value.
fn stat(rx: &Rx, label: &str, note: Option<Node>, value: Node, bar: Option<(f64, Rgba)>) -> Node {
    let th = rx.theme;
    // The note takes what the label leaves
    let room = INNER - crate::gfx::measure_text(label, &th.small_font()).width - 6.0;
    let mut top = vec![Node::text(label, th.small_font(), th.subtext).key("label"), Node::spacer()];
    if let Some(n) = note {
        top.push(n.max_width(room).key("note"));
    }
    let mut column = vec![Node::row(top).gap(6.0).width(INNER).height(14.0).key("top"), value.max_width(INNER).key("value")];
    if let Some((fraction, color)) = bar {
        let progress = Node::new(Kind::Progress { value: fraction.clamp(0.0, 1.0), color, track: th.surface }).size(INNER, 4.0).key("progress");
        column.push(Node::row(vec![progress]).padding(3.0, 0.0, 0.0, 0.0).key("bar"));
    }
    Node::column(column)
        .gap(4.0)
        .align(Align::Start)
        .padding(9.0, 10.0, 9.0, 10.0)
        .size(TILE, if bar.is_some() { BARRED } else { STAT })
        .radius(10.0)
        .background(th.tile(th.primary, false))
}

impl Widget for Battery {
    fn start(&mut self, cx: &mut Cx) {
        if let Some(m) = crate::mock::load::<Mock>("battery") {
            self.mock = true;
            self.status = Some(m.status);
            self.health = Some((Health { condition: m.health, max_capacity: m.max_capacity }, Instant::now()));
            self.live = Some(m.live);
            self.loaded = true;
            return;
        }
        // IOKit: any change of the battery, the adapter, the charge or the time left
        let handle = Box::new(cx.handle::<Battery>());
        self.source = RunLoopSource::new(handle);
        // PowerUIAgent: the Charge Limit changed, also in System Settings
        let h = cx.handle::<Battery>();
        let block = RcBlock::new(move |_: c_int| h.send(|w: &mut Battery, cx| w.update(cx)));
        let mut token: c_int = 0;
        let ok = unsafe { notify_register_dispatch(c"com.apple.powerui.mclstatuschanged".as_ptr(), &mut token, dispatch2::DispatchQueue::main(), &block) };
        if ok == 0 {
            self.token = Some(token);
        }
        // Low Power Mode
        let h = cx.handle::<Battery>();
        let block = RcBlock::new(move |_: std::ptr::NonNull<objc2_foundation::NSNotification>| h.send(|w: &mut Battery, cx| w.update(cx)));
        self.observer = Some(unsafe {
            objc2_foundation::NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(objc2_foundation::NSProcessInfoPowerStateDidChangeNotification),
                None,
                None,
                &block,
            )
        });
        self.update(cx);
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let s = self.status.as_ref()?;
        let (icon, color) = self.icon(rx, s);
        Some(
            rx.pill(vec![
                Node::symbol(icon, th.icon_size, Weight::Semibold, color).key("icon"),
                Node::text(format!("{}%", s.percent), th.font(), th.text).key("percent"),
            ])
            .click(rx.toggle_popup()),
        )
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let s = self.status.as_ref()?;
        let mut rows = vec![self.header(rx, s)];
        if s.limit_state != LimitState::None {
            rows.extend(self.limit(rx, s));
        }
        rows.push(Node::row(self.tiles(rx, s)).columns(2).gap(GAP).key("tiles"));
        if let Some(a) = s.adapter.as_ref().filter(|_| s.ac) {
            rows.push(self.adapter(rx, a));
        }
        rows.push(rx.row_button(vec![Node::text(t("Impostazioni Batteria…", "Battery Settings…"), th.font(), th.primary)], vec![], rx.action("settings")).key("settings"));
        Some(rx.popup(WIDE, rows).gap(GAP).padding(8.0, 8.0, 6.0, 8.0))
    }

    fn ready(&self) -> bool {
        self.loaded
    }

    fn action(&mut self, name: &str, value: Option<f64>, cx: &mut Cx) {
        if name == "settings" {
            cx.close_popup();
            if !self.mock {
                runtime::open("x-apple.systempreferences:com.apple.Battery-Settings.extension");
            }
            return;
        }
        // The slider's value is its step; the button's limit is in its name
        let limit = match (name, value) {
            ("limit", Some(v)) => Some(LIMITS[((v * (LIMITS.len() - 1) as f64).round() as usize).min(LIMITS.len() - 1)]),
            _ => name.strip_prefix("limit:").and_then(|l| l.parse().ok()),
        };
        let Some(s) = self.status.as_mut() else { return };
        let current = if s.limit_state == LimitState::Off { 100 } else { s.limit };
        if name == "limit" && limit == Some(current) && s.limit_state != LimitState::Paused {
            return;
        }
        // At once, so the slider stays where it's released: the update that follows corrects it
        match limit {
            Some(l) => (s.limit, s.limit_state) = (l, if l < 100 { LimitState::On } else { LimitState::Off }),
            None if name == "full" => s.limit_state = LimitState::Paused,
            None => return,
        }
        self.failed = None;
        cx.redraw();
        if self.mock {
            return;
        }
        let name = name.to_string();
        cx.background::<Battery, _>(
            {
                let name = name.clone();
                move || match limit {
                    Some(l) => set_limit(l),
                    None if name == "full" => charge_to_full(),
                    None => true,
                }
            },
            move |w, ok, cx| {
                w.update(cx);
                if !ok {
                    w.failed = Some(name);
                    cx.redraw();
                }
            },
        );
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        if self.mock {
            return;
        }
        match event {
            Event::Wake => self.update(cx),
            Event::PopupOpened => {
                let stale = self.health.as_ref().is_none_or(|(_, at)| at.elapsed().as_secs() > 24 * 3600);
                if stale && self.status.is_some() {
                    cx.background::<Battery, _>(read_health, |w, health, cx| {
                        if let Some(h) = health {
                            w.health = Some((h, Instant::now()));
                            cx.redraw();
                        }
                    });
                }
                self.ticking += 1;
                self.tick(self.ticking, cx);
            }
            Event::PopupClosed => self.ticking += 1,
            _ => {}
        }
    }

    crate::any_widget!();
}

impl Drop for Battery {
    fn drop(&mut self) {
        if let Some(t) = self.token.take() {
            unsafe { notify_cancel(t) };
        }
        if let Some(o) = self.observer.take() {
            unsafe { objc2_foundation::NSNotificationCenter::defaultCenter().removeObserver(o.as_ref()) };
        }
    }
}

/// `14,2` in Italian, `14.2` in English.
fn decimal(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if crate::i18n::italian() { s.replace('.', ",") } else { s }
}

/// `14,2 W`, `112 W`.
fn watts(w: f64) -> String {
    format!("{} W", decimal(w, if w < 100.0 { 1 } else { 0 }))
}

/// `31,4 °C`, or in Fahrenheit when the Mac says so (System Settings → Language & Region).
fn temperature(c: f64) -> String {
    let fahrenheit = NSUserDefaults::standardUserDefaults().stringForKey(ns_string!("AppleTemperatureUnit")).is_some_and(|u| u.to_string() == "Fahrenheit");
    if fahrenheit { format!("{} °F", decimal(c * 9.0 / 5.0 + 32.0, 0)) } else { format!("{} °C", decimal(c, 1)) }
}

fn duration(minutes: i64) -> String {
    if minutes >= 60 { format!("{} h {} min", minutes / 60, minutes % 60) } else { format!("{minutes} min") }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

// IOKit and libnotify

use crate::iokit::{CFTypeRef, Dict, number, owned, string};

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPSCopyExternalPowerAdapterDetails() -> CFTypeRef;
    fn IOPSNotificationCreateRunLoopSource(callback: extern "C" fn(*mut c_void), context: *mut c_void) -> CFTypeRef;
}

unsafe extern "C" {
    fn CFRunLoopGetMain() -> CFTypeRef;
    fn CFRunLoopAddSource(rl: CFTypeRef, source: CFTypeRef, mode: CFTypeRef);
    fn CFRunLoopRemoveSource(rl: CFTypeRef, source: CFTypeRef, mode: CFTypeRef);
    fn CFRelease(cf: CFTypeRef);
    static kCFRunLoopDefaultMode: CFTypeRef;
    fn notify_register_dispatch(name: *const c_char, out_token: *mut c_int, queue: &dispatch2::DispatchQueue, handler: &block2::Block<dyn Fn(c_int)>) -> u32;
    fn notify_cancel(token: c_int) -> u32;
}

/// IOKit's notification of the power sources on the main run loop, removed when dropped.
struct RunLoopSource {
    source: CFTypeRef,
    context: *mut super::Handle<Battery>,
}

extern "C" fn power_changed(context: *mut c_void) {
    let handle = unsafe { &*(context as *const super::Handle<Battery>) };
    handle.send(|w, cx| w.update(cx));
}

impl RunLoopSource {
    fn new(handle: Box<super::Handle<Battery>>) -> Option<RunLoopSource> {
        let context = Box::into_raw(handle);
        let source = unsafe { IOPSNotificationCreateRunLoopSource(power_changed, context as *mut c_void) };
        if source.is_null() {
            drop(unsafe { Box::from_raw(context) });
            return None;
        }
        unsafe { CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopDefaultMode) };
        Some(RunLoopSource { source, context })
    }
}

impl Drop for RunLoopSource {
    fn drop(&mut self) {
        unsafe {
            CFRunLoopRemoveSource(CFRunLoopGetMain(), self.source, kCFRunLoopDefaultMode);
            CFRelease(self.source);
            drop(Box::from_raw(self.context));
        }
    }
}



/// The internal battery as IOKit describes it, as `pmset -g ps` does.
fn internal_battery() -> Option<Retained<Dict>> {
    crate::iokit::power_sources(false).into_iter().find(|d| string(d, ns_string!("Type")).as_deref() == Some("InternalBattery"))
}

/// The cycles so far and the ones the battery is made for.
fn cycles() -> (Option<i64>, Option<i64>) {
    match crate::iokit::services(c"AppleSmartBattery").first() {
        Some(b) => (b.number("CycleCount"), b.number("DesignCycleCount9C")),
        None => (None, None),
    }
}

/// The temperature and the power, as the battery's gauge and the Mac's telemetry say them. The
/// battery's own from its voltage and its current, which is less than 0 while it discharges.
fn read_live() -> Live {
    let Some(b) = crate::iokit::services(c"AppleSmartBattery").into_iter().next() else { return Live::default() };
    let telemetry = b.dict("PowerTelemetryData");
    let watts = |key: &NSString| telemetry.as_ref().and_then(|t| number(t, key)).filter(|mw| *mw >= 0).map(|mw| mw as f64 / 1000.0);
    let battery = match (b.number("Voltage"), b.number("Amperage")) {
        (Some(mv), Some(ma)) => Some(mv as f64 * ma as f64 / 1e6),
        _ => None,
    };
    Live {
        temperature: b.number("Temperature").filter(|c| *c > 0).map(|c| c as f64 / 100.0),
        system: watts(ns_string!("SystemLoad")),
        battery,
        input: watts(ns_string!("SystemPowerIn")),
    }
}

/// The adapter plugged in: its power, its voltage and whether it's USB-C or MagSafe.
fn read_adapter() -> Option<Adapter> {
    let a: Retained<Dict> = unsafe { owned(IOPSCopyExternalPowerAdapterDetails()) }?;
    let watts = number(&a, ns_string!("Watts")).filter(|w| *w > 0)?;
    let name = string(&a, ns_string!("Name")).unwrap_or_default();
    let description = string(&a, ns_string!("Description")).unwrap_or_default();
    let kind = if name.contains("MagSafe") {
        Some("MagSafe")
    } else if name.contains("USB-C") || description == "pd charger" {
        Some("USB-C")
    } else {
        None
    };
    let volts = number(&a, ns_string!("AdapterVoltage")).filter(|mv| *mv > 0).map(|mv| mv as f64 / 1000.0);
    Some(Adapter { watts, volts, kind: kind.map(String::from) })
}

/// PowerUIAgent's client, as System Settings uses it. MCL is the Manual Charge Limit.
fn client() -> Option<Retained<AnyObject>> {
    unsafe {
        if libc::dlopen(c"/System/Library/PrivateFrameworks/PowerUI.framework/PowerUI".as_ptr(), libc::RTLD_NOW).is_null() {
            return None;
        }
        let class = AnyClass::get(c"PowerUISmartChargeClient")?;
        let allocated: objc2::rc::Allocated<AnyObject> = msg_send![class, alloc];
        let client: Option<Retained<AnyObject>> = msg_send![allocated, initWithClientName: &*NSString::from_str("rustybar")];
        let client = client?;
        let supported: bool = msg_send![&*client, isMCLSupported];
        supported.then_some(client)
    }
}

/// The limit Charge to Full Now pauses comes back by itself, but PowerUIAgent tells which one only
/// to System Settings: RustyBar remembers the last one it saw on.
fn defaults() -> Option<Retained<NSUserDefaults>> {
    NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(ns_string!("rustybar")))
}

fn remember(limit: u8) {
    if limit < 100
        && let Some(d) = defaults() {
            d.setInteger_forKey(limit as isize, ns_string!("battery.limit"));
        }
}

fn remembered() -> u8 {
    let mine = defaults().map(|d| d.integerForKey(ns_string!("battery.limit"))).unwrap_or(0);
    if mine > 0 {
        return mine as u8;
    }
    // The one the SketchyBar helper saved, if any
    NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(ns_string!("sketchybar.battery_charge"))).map(|d| d.integerForKey(ns_string!("limit"))).unwrap_or(0) as u8
}

fn read_status() -> Option<Status> {
    let b = internal_battery()?;
    let current = number(&b, ns_string!("Current Capacity"))?;
    let max = number(&b, ns_string!("Max Capacity")).filter(|m| *m > 0)?;
    let ac = string(&b, ns_string!("Power Source State")).as_deref() == Some("AC Power");
    let charging = number(&b, ns_string!("Is Charging")).unwrap_or(0) != 0;
    let minutes = number(&b, if charging { ns_string!("Time to Full Charge") } else { ns_string!("Time to Empty") }).unwrap_or(-1);
    let (cycles, design_cycles) = cycles();
    let mut status = Status {
        percent: current * 100 / max,
        ac,
        charging,
        charged: number(&b, ns_string!("Is Charged")).unwrap_or(0) != 0,
        minutes,
        limit: 100,
        limit_state: LimitState::None,
        held: false,
        desktop: false,
        low_power: NSProcessInfo::processInfo().isLowPowerModeEnabled(),
        cycles,
        design_cycles,
        adapter: if ac { read_adapter() } else { None },
    };
    if let Some(c) = client() {
        unsafe {
            let mut error: *mut AnyObject = std::ptr::null_mut();
            let limit: u8 = msg_send![&*c, getMCLLimitWithError: &mut error];
            let limit = if error.is_null() && LIMITS.contains(&limit) { limit } else { 100 };
            let state: usize = msg_send![&*c, isMCLCurrentlyEnabled: std::ptr::null_mut::<*mut AnyObject>()];
            (status.limit, status.limit_state) = match state {
                MCL_ON if limit < 100 => {
                    remember(limit);
                    (limit, LimitState::On)
                }
                MCL_PAUSED => (remembered(), LimitState::Paused),
                _ => (100, LimitState::Off),
            };
            let (mut held, mut desktop, mut allowed) = (Bool::NO, Bool::NO, Bool::NO);
            let ok: bool = msg_send![&*c, isOBCEngaged: &mut held, asDesktopDevice: &mut desktop, chargingOverrideAllowed: &mut allowed, withError: std::ptr::null_mut::<*mut AnyObject>()];
            if ok {
                status.held = held.as_bool();
                status.desktop = desktop.as_bool();
            }
        }
    }
    Some(status)
}

/// Sets the Charge Limit, which also turns it back on while Charge to Full Now pauses it.
fn set_limit(limit: u8) -> bool {
    let Some(c) = client() else { return false };
    if !LIMITS.contains(&limit) {
        return false;
    }
    unsafe {
        let ok: bool = msg_send![&*c, setMCLLimit: limit, error: std::ptr::null_mut::<*mut AnyObject>()];
        let state: usize = msg_send![&*c, isMCLCurrentlyEnabled: std::ptr::null_mut::<*mut AnyObject>()];
        if !ok || (limit < 100 && state != MCL_ON) {
            return false;
        }
    }
    remember(limit);
    true
}

/// Charge to Full Now, as in the battery menu of macOS: the limit pauses until macOS turns it on.
fn charge_to_full() -> bool {
    let Some(c) = client() else { return false };
    unsafe {
        let state: usize = msg_send![&*c, isMCLCurrentlyEnabled: std::ptr::null_mut::<*mut AnyObject>()];
        if state == MCL_ON {
            let limit: u8 = msg_send![&*c, getMCLLimitWithError: std::ptr::null_mut::<*mut AnyObject>()];
            remember(limit);
        }
        msg_send![&*c, temporarilyDisableMCL: std::ptr::null_mut::<*mut AnyObject>()]
    }
}

/// The condition and the maximum capacity System Settings shows, e.g. "Good" and "96%", which only
/// system_profiler says. IOKit's `BatteryHealth` isn't the same: it can say "Check Battery" for a
/// battery system_profiler calls Good, with few cycles and no failure.
fn read_health() -> Option<Health> {
    let out = runtime::run("/usr/sbin/system_profiler", &["SPPowerDataType", "-json"], None, std::time::Duration::from_secs(20));
    let v: serde_json::Value = serde_json::from_str(&out.stdout).ok()?;
    let info = v["SPPowerDataType"].as_array()?.iter().map(|e| &e["sppower_battery_health_info"]).find(|i| i.is_object())?;
    let field = |key: &str| info[key].as_str().filter(|s| !s.is_empty()).map(String::from);
    Some(Health { condition: field("sppower_battery_health"), max_capacity: field("sppower_battery_health_maximum_capacity") })
}
