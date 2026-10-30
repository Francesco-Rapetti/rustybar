//! The battery of the Mac: the charge with an icon for its state, and a popup with the state in
//! words, the Charge Limit of System Settings → Battery (macOS 26.4+), where a click sets another
//! percentage, Charge to Full Now, the health of the battery and the Battery settings.
//!
//! The icon: on battery the level, yellow in Low Power Mode or else red at 20% or less, as in the
//! battery menu of macOS; charging, the bolt; plugged in but not charging, the plug, yellow when
//! macOS doesn't say why. Green is a battery macOS looks after (charging up to the limit, holding
//! the charge at it, Optimized Battery Charging, the desktop mode); yellow is also the bolt or the
//! plug while Charge to Full Now pauses the limit.
//!
//! IOKit says when the battery changes and PowerUIAgent when the limit does, so nothing polls. The
//! Charge Limit is PowerUIAgent's, asked through the private framework of System Settings.

use std::ffi::{c_char, c_int, c_void};
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{AnyThread, msg_send};
use objc2_foundation::{NSProcessInfo, NSString, NSUserDefaults, ns_string};

use super::{Cx, Event, Rx, Widget};
use crate::gfx::{Rgba, Weight};
use crate::i18n::t;
use crate::runtime;
use crate::ui::Node;

const LIMITS: [u8; 5] = [80, 85, 90, 95, 100];
/// Of `isMCLCurrentlyEnabled:`: on, or paused by Charge to Full Now.
const MCL_ON: usize = 1;
const MCL_PAUSED: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
enum LimitState {
    /// A Mac without the Charge Limit.
    None,
    Off,
    On,
    Paused,
}

#[derive(Clone, Debug)]
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
    watts: Option<i64>,
    /// "Good" or another condition, and the cycles.
    health: Option<String>,
    cycles: Option<i64>,
}

#[derive(Default)]
pub struct Battery {
    status: Option<Status>,
    loaded: bool,
    /// From system_profiler, the only one that says it: at most once a day, when the popup opens.
    max_capacity: Option<(String, Instant)>,
    reading: bool,
    queued: bool,
    /// The row whose action macOS refused, until the next update.
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
        if let (true, Some(w)) = (s.ac, s.watts) {
            details.push(t(&format!("alimentatore da {w} W"), &format!("{w} W adapter")).into());
        }
        (status, color, capitalize(&details.join(" · ")))
    }
}

impl Widget for Battery {
    fn start(&mut self, cx: &mut Cx) {
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
        let (status, status_color, details) = self.state(rx, s);
        let (_, color) = self.icon(rx, s);
        let bold = th.font();
        let mut rows = vec![rx.section(&status, Some(Node::text(format!("{}%", s.percent), bold.clone(), color))).key("status")];
        rows[0] = Node::row(vec![Node::text(&status, bold.clone(), status_color), Node::spacer(), Node::text(format!("{}%", s.percent), bold.clone(), color)])
            .padding(6.0, 8.0, 2.0, 8.0)
            .key("status");
        if !details.is_empty() {
            rows.push(rx.detail(&details).key("details"));
        }

        if s.limit_state != LimitState::None {
            let paused = s.limit_state == LimitState::Paused;
            let right = paused.then(|| Node::text(t("Sospeso", "Paused"), bold.clone(), th.yellow));
            rows.push(rx.section(t("Limite di carica", "Charge Limit"), right).padding(14.0, 8.0, 4.0, 8.0).key("limit"));
            if paused {
                rows.push(rx.detail(t("Per Completa carica ora: macOS lo riattiva da solo più tardi", "For Charge to Full Now: macOS turns it back on later")).key("paused"));
            }
            // While paused a click on a percentage would turn the limit back on, at another one
            let disabled = paused && s.limit > 0;
            for l in LIMITS {
                let name = format!("limit:{l}");
                let label = if l == 100 { t("Nessun limite", "No Limit").to_string() } else { format!("{l}%") };
                let current = l == s.limit || (l == 100 && s.limit_state == LimitState::Off);
                let failed = self.failed.as_deref() == Some(name.as_str());
                let text = if failed { t("Non riuscito", "Failed").to_string() } else { label };
                let color = if failed {
                    th.red
                } else if disabled {
                    th.overlay
                } else {
                    th.text
                };
                let mut row = Node::row(vec![Node::text(text, bold.clone(), color)]).pad(4.0, 8.0).height(24.0).radius(th.row_radius).key(name.clone());
                if current {
                    row = row.background(th.surface);
                }
                if !disabled {
                    row = row.click(rx.action(&name));
                }
                rows.push(row);
            }
            let full = match s.limit_state {
                LimitState::Paused if s.limit > 0 => Some((t("Riattiva il limite ora", "Turn the Limit Back On Now"), format!("limit:{}", s.limit))),
                LimitState::On if s.ac && s.percent < 100 => Some((t("Completa carica ora", "Charge to Full Now"), "full".to_string())),
                _ => None,
            };
            if let Some((label, action)) = full {
                let failed = self.failed.as_deref() == Some("full");
                let (text, color) = if failed { (t("Non riuscito", "Failed"), th.red) } else { (label, th.primary) };
                rows.push(Node::row(vec![Node::text(text, bold.clone(), color)]).pad(4.0, 8.0).height(24.0).radius(th.row_radius).click(rx.action(&action)).key("full"));
            }
        }

        if let Some(health) = &s.health {
            let (text, color) = if health == "Good" { (t("Normale", "Normal"), th.subtext) } else { (t("Assistenza consigliata", "Service Recommended"), th.yellow) };
            rows.push(rx.section(t("Stato batteria", "Battery Health"), Some(Node::text(text, bold.clone(), color))).padding(14.0, 8.0, 4.0, 8.0).key("health"));
            let mut parts = Vec::new();
            if let Some((cap, _)) = &self.max_capacity {
                parts.push(t(&format!("Capacità massima {cap}"), &format!("Maximum capacity {cap}")).to_string());
            }
            if let Some(c) = s.cycles {
                parts.push(t(&format!("{c} cicli"), &format!("{c} cycles")).to_string());
            }
            if !parts.is_empty() {
                rows.push(rx.detail(&parts.join(" · ")).key("health-details"));
            }
        }

        rows.push(Node::default().height(8.0).key("gap"));
        rows.push(rx.row_button(vec![Node::text(t("Impostazioni Batteria…", "Battery Settings…"), bold, th.primary)], vec![], rx.action("settings")).key("settings"));
        Some(rx.popup(260.0, rows).padding(4.0, 4.0, 4.0, 4.0).gap(0.0))
    }

    fn ready(&self) -> bool {
        self.loaded
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        if name == "settings" {
            cx.close_popup();
            runtime::open("x-apple.systempreferences:com.apple.Battery-Settings.extension");
            return;
        }
        let name = name.to_string();
        cx.background::<Battery, _>(
            {
                let name = name.clone();
                move || match name.strip_prefix("limit:") {
                    Some(l) => l.parse().ok().map(set_limit).unwrap_or(false),
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
        match event {
            Event::Wake => self.update(cx),
            Event::PopupOpened => {
                let stale = self.max_capacity.as_ref().is_none_or(|(_, at)| at.elapsed().as_secs() > 24 * 3600);
                if stale && self.status.as_ref().is_some_and(|s| s.health.is_some()) {
                    cx.background::<Battery, _>(max_capacity, |w, cap, cx| {
                        if let Some(c) = cap {
                            w.max_capacity = Some((c, Instant::now()));
                            cx.redraw();
                        }
                    });
                }
            }
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

fn cycle_count() -> Option<i64> {
    crate::iokit::services(c"AppleSmartBattery").first()?.number("CycleCount")
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
        watts: None,
        health: string(&b, ns_string!("BatteryHealth")).filter(|h| !h.is_empty()),
        cycles: cycle_count(),
    };
    if ac {
        let adapter: Option<Retained<Dict>> = unsafe { owned(IOPSCopyExternalPowerAdapterDetails()) };
        status.watts = adapter.and_then(|a| number(&a, ns_string!("Watts")));
    }
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

/// The maximum capacity System Settings shows, e.g. "96%", which only system_profiler says.
fn max_capacity() -> Option<String> {
    let out = runtime::run("/usr/sbin/system_profiler", &["SPPowerDataType", "-json"], None, std::time::Duration::from_secs(20));
    let v: serde_json::Value = serde_json::from_str(&out.stdout).ok()?;
    v["SPPowerDataType"].as_array()?.iter().find_map(|e| e["sppower_battery_health_info"]["sppower_battery_health_maximum_capacity"].as_str().map(String::from))
}
