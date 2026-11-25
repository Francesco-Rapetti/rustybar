//! Calls, for `now_playing`: a Google Meet in a browser, a Slack huddle, a Teams meeting or a
//! WhatsApp call, with its mic, camera, raised hand and a way out (see `NowPlaying`, which shows
//! them).
//!
//! - Meet, in a browser built on Chromium (Dia, Chrome, Arc, Brave, Edge…), through RustyBar's
//!   extension (`extension/` in the repo, in the app's Resources): the Meet page tells the bar
//!   when its call starts, changes and ends, and presses its buttons when the bar asks, over a
//!   WebSocket to the bar on this Mac (see `meet`). Nothing is read on a timer, and macOS asks
//!   for no permission.
//! - Slack and Teams, through Accessibility. When one may start: CoreAudio says which processes
//!   capture audio, with no permission, and tells when that changes (its process objects, macOS
//!   14.2 and later). Only an app that captures is asked whether it's in a call, and while it is,
//!   every second: neither says when its state changes. The buttons of the call are found by their
//!   names (which say what a click does: "Mute" while the mic is on), pressed with AXPress, so no
//!   keystrokes and the app stays where it is. Electron (Slack) shows what's in its pages to
//!   Accessibility only with `AXManualAccessibility`, which RustyBar sets while it captures.
//!   Microsoft's local API for Teams (ws://localhost:8124) was retired in June 2026, so Teams
//!   goes the same way; its pages are in the web view of a child process (Edge's), which shows
//!   them only when asked with `AXEnhancedUserInterface`, as an assistive app would, and its
//!   buttons have ids that don't change with the language (seen in a real meeting, Teams 26304).
//!   Both have a page before the call, where they capture already: Teams' "Partecipazione alla
//!   riunione" and Slack's "Huddle Preview", a call in `lobby` as Meet's page before joining.
//! - WhatsApp (the app for the Mac), the same way, with what is its own (seen in real calls,
//!   WhatsApp 26.47). A call has a window of its own with a group `Calling_Window`, and only
//!   there its buttons are looked for: the chats have a "Rispondi" too. The buttons have no ids
//!   and are named by their state, not by what a click does ("audio riattivato" while the mic is
//!   on), with a left-to-right mark in front: its own words are matched whole, in Italian and
//!   English. The camera's is named the other way round ("fotocamera disattivata" while it's
//!   on), so WhatsApp's picture of you in the call, which says when the camera is off, is
//!   believed first. The buttons of a call that ended go on answering with their names, so they
//!   count only while their window is still one of the app's. A call that rings plays and
//!   doesn't capture yet: WhatsApp is asked while it plays too, and a call with Accept in place
//!   of Leave is in `lobby`.
//!
//! Everything here but `watch` and `capturing` runs on a background thread: Accessibility asks the
//! app for each attribute.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSApplicationActivationOptions, NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
use objc2_foundation::{NSArray, NSNumber, NSString};
use serde::Deserialize;

use super::Handle;
use super::audio::ca;
use super::now_playing::NowPlaying;
use crate::iokit::{CFTypeRef, cf, owned};

pub const SLACK: &str = "com.tinyspeck.slackmacgap";
pub const TEAMS: &str = "com.microsoft.teams2";
pub const WHATSAPP: &str = "net.whatsapp.WhatsApp";
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Service {
    #[default]
    Meet,
    Slack,
    Teams,
    WhatsApp,
}

impl Service {
    pub fn name(self) -> &'static str {
        match self {
            Service::Meet => "Google Meet",
            Service::Slack => "Slack",
            Service::Teams => "Microsoft Teams",
            Service::WhatsApp => "WhatsApp",
        }
    }

    /// For a tab.
    pub fn short(self) -> &'static str {
        match self {
            Service::Meet => "Meet",
            Service::Slack => "Slack",
            Service::Teams => "Teams",
            Service::WhatsApp => "WhatsApp",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Service::Meet => "meet",
            Service::Slack => "slack",
            Service::Teams => "teams",
            Service::WhatsApp => "whatsapp",
        }
    }

    /// The service an app that captures may be in a call of (Meet tells by itself).
    pub fn of(bundle: &str) -> Option<Service> {
        match bundle {
            SLACK => Some(Service::Slack),
            TEAMS => Some(Service::Teams),
            WHATSAPP => Some(Service::WhatsApp),
            _ => None,
        }
    }
}

/// What keeps RustyBar from reading a call, or from controlling it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// RustyBar doesn't have Accessibility, for Slack, Teams and WhatsApp.
    Accessibility,
}

/// A call, as its app shows it. `mic` and `camera` are on (not muted), None when the app doesn't
/// say.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Call {
    pub service: Service,
    /// The app it's in: the browser, for Meet.
    pub bundle: String,
    /// Its name, as macOS shows it.
    pub app: String,
    /// The meeting, or the channel of a huddle; empty if the app doesn't say.
    pub name: String,
    /// Meet's code of the meeting (`abc-defg-hij`), to find it in the calendar.
    pub code: String,
    pub mic: Option<bool>,
    pub camera: Option<bool>,
    /// None without the button (Slack has none).
    pub hand: Option<bool>,
    pub leave: bool,
    /// Meet's page before joining: the mic and camera of the preview, and Join in place of Leave.
    /// A WhatsApp call that rings: Join answers it.
    pub lobby: bool,
    /// Join is there: not while it waits for someone to let you in.
    pub join: bool,
    /// How long it's been on, in made-up data.
    #[serde(rename = "elapsed")]
    pub elapsed: f64,
    /// The app's process: the browser's, for Meet.
    #[serde(skip)]
    pub pid: i32,
    /// Meet's: the browser's tab, and the line to the extension it came by.
    #[serde(skip)]
    pub tab: i64,
    #[serde(skip)]
    pub line: u64,
    /// When RustyBar saw it start.
    #[serde(skip)]
    pub since: f64,
    #[serde(skip)]
    pub blocked: Option<Blocked>,
}

/// An app that captures, to ask.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub service: Service,
    pub pid: i32,
    pub bundle: String,
}

pub enum Found {
    Call(Call),
    Nothing,
    Blocked(Blocked),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Control {
    Mic,
    Camera,
    Hand,
    Leave,
    /// Meet's, before joining.
    Join,
}

impl Control {
    pub fn key(self) -> &'static str {
        match self {
            Control::Mic => "mic",
            Control::Camera => "camera",
            Control::Hand => "hand",
            Control::Leave => "leave",
            Control::Join => "join",
        }
    }
}

/// Asks each target (Slack, Teams or WhatsApp) whether it's in a call.
pub fn read(targets: &[Target]) -> Vec<Found> {
    targets.iter().map(ax::read).collect()
}

/// Presses a control of the call of `target`; true if it found it.
pub fn press(target: &Target, control: Control) -> bool {
    ax::press(target, control)
}

/// Brings the call's window forward in its app (see `activate`).
pub fn show(target: &Target) {
    ax::show(target)
}

/// The app of `pid` in front, on the main thread.
pub fn activate(pid: i32) {
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
        app.activateWithOptions(NSApplicationActivationOptions::empty());
    }
}

/// The name of the app of `pid`, as macOS shows it.
pub fn app_name(pid: i32) -> String {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid).and_then(|a| a.localizedName()).map(|n| n.to_string()).unwrap_or_default()
}

/// What Accessibility shows of the windows of `pid`, a line per element, to learn what a call
/// looks like (`rustybar msg trigger call_dump`).
pub fn dump(pid: i32) -> String {
    ax::dump(pid)
}

// What captures, from CoreAudio

/// The widgets to tell, the process objects listened to, and those of them that are WhatsApp's.
struct Watch {
    handles: Vec<Handle<NowPlaying>>,
    system: bool,
    processes: Vec<u32>,
    ringers: Vec<u32>,
}

static WATCH: Mutex<Watch> = Mutex::new(Watch { handles: Vec::new(), system: false, processes: Vec::new(), ringers: Vec::new() });

/// Whether a process plays (`kAudioProcessPropertyIsRunningOutput`).
const PROCESS_OUTPUT: u32 = ca::fc(b"piro");

extern "C" fn capture_changed(_id: u32, _count: u32, _addresses: *const ca::Address, _data: *mut c_void) -> i32 {
    let handles = WATCH.lock().map(|w| w.handles.clone()).unwrap_or_default();
    for h in handles {
        h.send(|w, cx| w.capture_changed(cx));
    }
    0
}

/// Tells `handle` when a process starts or stops capturing audio.
pub fn watch(handle: Handle<NowPlaying>) {
    let Ok(mut watch) = WATCH.lock() else { return };
    watch.handles.retain(|h| h.alive());
    watch.handles.push(handle);
    if !watch.system {
        watch.system = true;
        unsafe { ca::AudioObjectAddPropertyListener(ca::SYSTEM, &ca::address(ca::PROCESSES, ca::GLOBAL, 0), capture_changed, std::ptr::null_mut()) };
    }
}

/// The apps that capture audio now, each once: its pid and bundle; and WhatsApp while it plays,
/// since a call that rings there doesn't capture yet. Listens to the processes it doesn't know
/// yet, so that their next start or stop is told.
pub fn capturing() -> Vec<(i32, String)> {
    let processes = ca::objects(ca::SYSTEM, ca::address(ca::PROCESSES, ca::GLOBAL, 0));
    let mut ringers = Vec::new();
    if let Ok(mut watch) = WATCH.lock() {
        let new: Vec<u32> = processes.iter().copied().filter(|p| !watch.processes.contains(p)).collect();
        for p in new {
            unsafe { ca::AudioObjectAddPropertyListener(p, &ca::address(ca::PROCESS_INPUT, ca::GLOBAL, 0), capture_changed, std::ptr::null_mut()) };
            // Whose it is, once: WhatsApp's is told when it plays too
            let whatsapp = ca::get::<i32>(p, ca::address(ca::PROCESS_PID, ca::GLOBAL, 0)).and_then(app_of).is_some_and(|(_, bundle)| bundle == WHATSAPP);
            if whatsapp {
                unsafe { ca::AudioObjectAddPropertyListener(p, &ca::address(PROCESS_OUTPUT, ca::GLOBAL, 0), capture_changed, std::ptr::null_mut()) };
                watch.ringers.push(p);
            }
        }
        // The objects of processes gone are gone too, with their listeners
        watch.ringers.retain(|p| processes.contains(p));
        watch.processes = processes.clone();
        ringers = watch.ringers.clone();
    }
    let mut apps: Vec<(i32, String)> = Vec::new();
    for p in processes {
        let captures = ca::get::<u32>(p, ca::address(ca::PROCESS_INPUT, ca::GLOBAL, 0)) == Some(1);
        if !captures && !(ringers.contains(&p) && ca::get::<u32>(p, ca::address(PROCESS_OUTPUT, ca::GLOBAL, 0)) == Some(1)) {
            continue;
        }
        let Some(pid) = ca::get::<i32>(p, ca::address(ca::PROCESS_PID, ca::GLOBAL, 0)) else { continue };
        if let Some(app) = app_of(pid)
            && !apps.contains(&app)
        {
            apps.push(app);
        }
    }
    apps
}

/// The app a process belongs to: itself, or the app that started it (a browser's helpers, and
/// theirs).
pub fn app_of(mut pid: i32) -> Option<(i32, String)> {
    for _ in 0..4 {
        if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            && app.activationPolicy() == NSApplicationActivationPolicy::Regular
            && let Some(bundle) = app.bundleIdentifier()
        {
            return Some((pid, bundle.to_string()));
        }
        pid = parent(pid)?;
    }
    None
}

fn parent(pid: i32) -> Option<i32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_bsdinfo>() as i32;
    let read = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, &mut info as *mut _ as *mut c_void, size) };
    (read == size && info.pbi_ppid > 1).then_some(info.pbi_ppid as i32)
}

// Slack, Teams and WhatsApp, through Accessibility

mod ax {
    use super::*;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> u8;
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> u8;
        fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
        fn AXUIElementCopyAttributeValue(element: CFTypeRef, attribute: CFTypeRef, value: *mut CFTypeRef) -> i32;
        fn AXUIElementSetAttributeValue(element: CFTypeRef, attribute: CFTypeRef, value: CFTypeRef) -> i32;
        fn AXUIElementPerformAction(element: CFTypeRef, action: CFTypeRef) -> i32;
        fn AXUIElementSetMessagingTimeout(element: CFTypeRef, seconds: f32) -> i32;
    }

    /// An element, kept between reads. Accessibility's elements are plain values that any thread
    /// may ask about.
    #[derive(Clone)]
    struct El(Retained<AnyObject>);
    unsafe impl Send for El {}

    /// How far a search of the windows goes: Slack's are big.
    const MOST: usize = 6000;

    /// The buttons of the call found last, by pid: asked again at each read, and searched for
    /// again only when one is gone.
    struct Found {
        controls: HashMap<Control, El>,
        window: Option<El>,
        /// WhatsApp's picture of you in the call, which says when the camera is off.
        own: Option<El>,
    }

    static CACHE: Mutex<Option<HashMap<i32, Found>>> = Mutex::new(None);
    /// The apps told to show their pages to Accessibility (Electron's `AXManualAccessibility`).
    static MANUAL: Mutex<Vec<i32>> = Mutex::new(Vec::new());
    /// When the web view of each app was last asked to show its pages (see `show_web_view`).
    static WEB_VIEW: Mutex<Vec<(i32, f64)>> = Mutex::new(Vec::new());

    fn app(pid: i32) -> Option<El> {
        let app: Retained<AnyObject> = unsafe { owned(AXUIElementCreateApplication(pid)) }?;
        unsafe { AXUIElementSetMessagingTimeout(cf(&*app), 0.5) };
        Some(El(app))
    }

    fn attribute(e: &El, name: &str) -> Option<Retained<AnyObject>> {
        let mut value: CFTypeRef = std::ptr::null();
        if unsafe { AXUIElementCopyAttributeValue(cf(&*e.0), cf(&*NSString::from_str(name)), &mut value) } != 0 {
            return None;
        }
        unsafe { owned(value) }
    }

    fn string(e: &El, name: &str) -> String {
        attribute(e, name).and_then(|v| v.downcast::<NSString>().ok()).map(|s| s.to_string()).unwrap_or_default()
    }

    fn elements(e: &El, name: &str) -> Vec<El> {
        attribute(e, name).and_then(|v| v.downcast::<NSArray>().ok()).map(|a| a.iter().map(El).collect()).unwrap_or_default()
    }

    fn number(e: &El, name: &str) -> Option<f64> {
        attribute(e, name).and_then(|v| v.downcast::<NSNumber>().ok()).map(|n| n.doubleValue())
    }

    /// What a button says it does: its title, description and help, in lower case.
    fn label(e: &El) -> String {
        let mut parts = Vec::new();
        for name in ["AXTitle", "AXDescription", "AXHelp"] {
            let s = string(e, name);
            if !s.is_empty() && !parts.contains(&s) {
                parts.push(s);
            }
        }
        parts.join(" ").to_lowercase()
    }

    fn words(label: &str) -> Vec<&str> {
        label.split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty()).collect()
    }

    fn any(words: &[&str], of: &[&str]) -> bool {
        words.iter().any(|w| of.contains(w))
    }

    /// A name without the marks of direction WhatsApp puts in it (a left-to-right mark in front).
    fn plain(label: &str) -> String {
        label.chars().filter(|c| !matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')).collect::<String>().trim().to_string()
    }

    /// The control a button of a WhatsApp call is, and whether what it controls is on, by its name
    /// in lower case. WhatsApp names them by their state, not by what a click does: these are its
    /// own words, Italian and English (read in real calls, and from the app's strings). The
    /// camera's is the other way round: "disattivata" while the camera is on.
    pub(super) fn whatsapp(label: &str) -> Option<(Control, Option<bool>)> {
        Some(match plain(label).as_str() {
            "audio riattivato" | "mute off" => (Control::Mic, Some(true)),
            "microfono disattivato" | "call muted" | "audio disattivato" | "mute on" => (Control::Mic, Some(false)),
            "fotocamera disattivata" | "camera off" => (Control::Camera, Some(true)),
            "fotocamera attiva" | "camera on" => (Control::Camera, Some(false)),
            // A Mac without a camera: the button is there, with nothing to turn on
            "fotocamera non disponibile" | "camera unavailable" => (Control::Camera, None),
            "abbandona chiamata" | "leave call" | "termina chiamata" | "end call" => (Control::Leave, None),
            // A call that rings
            "accetta" | "rispondi" | "accept" | "accept call" | "rispondi alla chiamata" | "answer call" | "partecipa" | "join call" => (Control::Join, None),
            _ => return None,
        })
    }

    /// Whether the camera of a WhatsApp call is on, None if there's none to turn on. WhatsApp's
    /// picture of you in the call ("Feed video ridotto") adds "Fotocamera disattivata" to its name
    /// when the camera is off: where the call has one it is believed, else the button's name.
    pub(super) fn whatsapp_camera(button: &str, own: Option<&str>) -> Option<bool> {
        let (_, on) = whatsapp(button).filter(|(c, _)| *c == Control::Camera)?;
        on?;
        if let Some(own) = own.map(plain)
            && ["feed video ridotto", "my minimized video feed"].iter().any(|name| own.starts_with(name))
        {
            return Some(!(own.contains("fotocamera disattivata") || own.contains("camera off")));
        }
        on
    }

    /// Who a WhatsApp call is with, from its window's title: "Ada - Videochiamata WhatsApp".
    pub(super) fn whatsapp_name(title: &str) -> String {
        plain(title).rsplit_once(" - ").map(|(name, _)| name.trim().to_string()).unwrap_or_default()
    }

    /// The control a button is, by its name, in English or Italian. Slack's leave button names the
    /// huddle; Teams' is just Leave.
    pub(super) fn classify(service: Service, label: &str) -> Option<Control> {
        if service == Service::WhatsApp {
            return whatsapp(label).map(|(c, _)| c);
        }
        let w = words(label);
        const TOGGLE: [&str; 14] = ["mute", "unmute", "turn", "disattiva", "riattiva", "attiva", "start", "stop", "accendi", "spegni", "enable", "disable", "on", "off"];
        // The page before a call. Teams' "Partecipa ora" ("Join now"), not the "Partecipa" of a
        // chat; Slack's "Start Huddle", not the "Start a Huddle" of the Huddles page nor the
        // "Start huddle with …" of a conversation, and its two switches, which have no verb.
        if service == Service::Teams && w.len() == 2 && ((any(&w, &["partecipa"]) && any(&w, &["ora"])) || (any(&w, &["join"]) && any(&w, &["now"]))) {
            return Some(Control::Join);
        }
        if service == Service::Slack {
            if w == ["start", "huddle"] || w == ["avvia", "huddle"] {
                return Some(Control::Join);
            }
            if w.len() == 1 && any(&w, &["microphone", "microfono"]) {
                return Some(Control::Mic);
            }
            if w.len() == 1 && any(&w, &["video"]) {
                return Some(Control::Camera);
            }
        }
        if any(&w, &["leave", "esci", "abbandona", "lascia", "hang"]) && (service == Service::Teams || any(&w, &["huddle"])) {
            // Not a channel, a team or a community to leave, nor the devices to sign out of
            if any(&w, &["canale", "channel", "team", "community", "chat", "gruppo", "group", "dispositivi", "devices", "visualizzazione", "view"]) {
                return None;
            }
            return Some(Control::Leave);
        }
        // Teams' is just "Mano" ("Hand"); Slack has none
        if any(&w, &["hand", "mano"]) && (any(&w, &["raise", "lower", "alza", "abbassa"]) || (service == Service::Teams && w.len() == 1)) {
            return Some(Control::Hand);
        }
        // Not the captions of a video, a clip to record, or everyone's video
        if any(&w, &["captions", "sottotitoli", "clip", "preview", "anteprima", "tutto", "tutti", "everyone", "all"]) {
            return None;
        }
        if any(&w, &["camera", "fotocamera", "video", "videocamera", "webcam"]) && any(&w, &TOGGLE) {
            return Some(Control::Camera);
        }
        // Not what silences a conversation's notifications, someone else, or the speakers too
        const OTHERS: [&str; 13] = ["conversation", "conversazione", "channel", "canale", "notifications", "notifiche", "thread", "partecipante", "partecipanti", "participant", "participants", "altoparlanti", "temporaneamente"];
        if any(&w, &OTHERS) || any(&w, &["altoparlante", "speakers"]) {
            return None;
        }
        if (any(&w, &["mic", "microphone", "microfono"]) && any(&w, &TOGGLE)) || any(&w, &["mute", "unmute"]) || (any(&w, &["audio"]) && any(&w, &["disattiva", "riattiva"])) {
            return Some(Control::Mic);
        }
        None
    }

    /// Teams' buttons have an id in its page that doesn't change with the language.
    pub(super) fn by_id(service: Service, id: &str) -> Option<Control> {
        if service != Service::Teams {
            return None;
        }
        match id {
            "microphone-button" => Some(Control::Mic),
            "video-button" => Some(Control::Camera),
            "raisehands-button" => Some(Control::Hand),
            "hangup-button" => Some(Control::Leave),
            "prejoin-join-button" => Some(Control::Join),
            _ => None,
        }
    }

    /// The control a button is: by its id where the app gives one, else by its name.
    fn kind(service: Service, e: &El, label: &str) -> Option<Control> {
        let id = if service == Service::Teams { by_id(service, &string(e, "AXDOMIdentifier")) } else { None };
        id.or_else(|| classify(service, label))
    }

    /// Whether what a button controls is on now, from its name, which says what a click does:
    /// "Mute" while the mic is on, "Raise hand" while it's down.
    pub(super) fn on_now(control: Control, label: &str) -> Option<bool> {
        // A way out or a way in is neither on nor off
        if matches!(control, Control::Leave | Control::Join) {
            return None;
        }
        let w = words(label);
        let (off_now, on_now): (&[&str], &[&str]) = match control {
            Control::Hand => (&["raise", "alza"], &["lower", "abbassa"]),
            _ => (&["unmute", "riattiva", "attiva", "start", "accendi", "on", "enable"], &["mute", "disattiva", "stop", "spegni", "off", "disable"]),
        };
        match (any(&w, off_now), any(&w, on_now)) {
            (true, false) => Some(false),
            (false, true) => Some(true),
            _ => None,
        }
    }

    const BUTTONS: [&str; 4] = ["AXButton", "AXCheckBox", "AXToggle", "AXRadioButton"];

    /// Each element of the windows of `app`, breadth first, with the window it's in, as far as
    /// `MOST`; `f` says whether to look inside it.
    fn walk(app: &El, mut f: impl FnMut(&El, &El, usize) -> bool) {
        let mut queue: std::collections::VecDeque<(El, El, usize)> = elements(app, "AXWindows").into_iter().map(|w| (w.clone(), w, 0)).collect();
        let mut seen = 0;
        while let Some((e, window, depth)) = queue.pop_front() {
            seen += 1;
            if seen > MOST {
                break;
            }
            if f(&e, &window, depth) {
                for child in elements(&e, "AXChildren") {
                    queue.push_back((child, window.clone(), depth + 1));
                }
            }
        }
    }

    fn search(service: Service, app: &El) -> Found {
        if service == Service::WhatsApp {
            return search_whatsapp(app);
        }
        let mut found = Found { controls: HashMap::new(), window: None, own: None };
        walk(app, |e, window, _| {
            let role = string(e, "AXRole");
            if BUTTONS.contains(&role.as_str()) {
                if let Some(c) = kind(service, e, &label(e)) {
                    found.controls.entry(c).or_insert_with(|| e.clone());
                    // The call's window, or the one of the page before it
                    if c == Control::Leave || (c == Control::Join && found.window.is_none()) {
                        found.window = Some(window.clone());
                    }
                }
                return false;
            }
            // Menus aren't the call
            !matches!(role.as_str(), "AXMenuBar" | "AXMenu" | "AXStaticText" | "AXImage")
        });
        found
    }

    /// The group of a call in one of WhatsApp's windows: a content group, and in it
    /// `Calling_Window`.
    fn call_group(window: &El) -> Option<El> {
        let mut level = vec![window.clone()];
        for _ in 0..3 {
            let mut next = Vec::new();
            for e in &level {
                for child in elements(e, "AXChildren") {
                    if string(&child, "AXRole") != "AXGroup" {
                        continue;
                    }
                    if string(&child, "AXIdentifier") == "Calling_Window" {
                        return Some(child);
                    }
                    next.push(child);
                }
            }
            level = next;
        }
        None
    }

    /// The buttons of WhatsApp's call: those of the group of a call, in the window that has one.
    fn search_whatsapp(app: &El) -> Found {
        for window in elements(app, "AXWindows") {
            let Some(group) = call_group(&window) else { continue };
            let mut controls = HashMap::new();
            let mut own = None;
            let mut queue = std::collections::VecDeque::from([group]);
            let mut seen = 0;
            while let Some(e) = queue.pop_front() {
                seen += 1;
                if seen > MOST {
                    break;
                }
                let role = string(&e, "AXRole");
                if BUTTONS.contains(&role.as_str()) {
                    if let Some((c, _)) = whatsapp(&label(&e)) {
                        controls.entry(c).or_insert(e);
                    }
                    continue;
                }
                if own.is_none() && role != "AXGroup" && string(&e, "AXIdentifier") == "CallUI_FloatingSelfVideoView" {
                    own = Some(e.clone());
                }
                if !matches!(role.as_str(), "AXStaticText" | "AXImage") {
                    queue.extend(elements(&e, "AXChildren"));
                }
            }
            if controls.contains_key(&Control::Leave) || controls.contains_key(&Control::Join) {
                return Found { controls, window: Some(window), own };
            }
        }
        Found { controls: HashMap::new(), window: None, own: None }
    }

    /// Electron shows its pages only to an app that asks: once per process.
    fn show_pages(pid: i32, app: &El) {
        let Ok(mut manual) = MANUAL.lock() else { return };
        if manual.contains(&pid) {
            return;
        }
        manual.push(pid);
        let yes = NSNumber::new_bool(true);
        unsafe { AXUIElementSetAttributeValue(cf(&*app.0), cf(&*NSString::from_str("AXManualAccessibility")), cf(&*yes)) };
    }

    /// Teams draws its pages in the web view of another process, its child "Microsoft Teams
    /// WebView" (Edge's), which shows only the windows to Accessibility until it's asked with
    /// `AXEnhancedUserInterface`, as an assistive app would. That process is asked, not Teams,
    /// whose windows a window manager would then move badly. Before a search, at most every 5 s:
    /// a browser that nobody asks for a while may stop.
    fn show_web_view(pid: i32) {
        let now = crate::time::now();
        {
            let Ok(mut asked) = WEB_VIEW.lock() else { return };
            if asked.iter().any(|(p, at)| *p == pid && now - at < 5.0) {
                return;
            }
            asked.retain(|(p, _)| *p != pid);
            asked.push((pid, now));
        }
        let yes = NSNumber::new_bool(true);
        for child in NSWorkspace::sharedWorkspace().runningApplications().iter() {
            let web_view = child.localizedName().is_some_and(|n| n.to_string().contains("WebView"));
            if web_view
                && parent(child.processIdentifier()) == Some(pid)
                && let Some(view) = app(child.processIdentifier())
            {
                unsafe { AXUIElementSetAttributeValue(cf(&*view.0), cf(&*NSString::from_str("AXEnhancedUserInterface")), cf(&*yes)) };
            }
        }
    }

    /// The buttons of a call, each with its name now.
    type Labelled = HashMap<Control, (El, String)>;

    /// The call's buttons, still there from the last read, or found again, its window, and
    /// WhatsApp's picture of you.
    fn controls(t: &Target, app: &El) -> Option<(Labelled, Option<El>, Option<El>)> {
        let mut cache = CACHE.lock().ok()?;
        let cache = cache.get_or_insert_with(HashMap::new);
        // A button whose name says it's something else now isn't one of them any more
        let labelled = |f: &Found| -> Labelled { f.controls.iter().map(|(c, e)| (*c, (e.clone(), label(e)))).filter(|(c, (e, l))| kind(t.service, e, l) == Some(*c)).collect() };
        if let Some(f) = cache.get(&t.pid) {
            // WhatsApp's buttons go on answering with their names after the call's window is gone:
            // they count only while it is still one of the app's
            let there = t.service != Service::WhatsApp || f.window.as_ref().is_some_and(|w| elements(app, "AXWindows").iter().any(|o| unsafe { CFEqual(cf(&*o.0), cf(&*w.0)) } != 0));
            let now = labelled(f);
            if there && (now.contains_key(&Control::Leave) || now.contains_key(&Control::Join)) && now.len() == f.controls.len() {
                return Some((now, f.window.clone(), f.own.clone()));
            }
        }
        if t.service == Service::Teams {
            show_web_view(t.pid);
        }
        let f = search(t.service, app);
        let now = labelled(&f);
        let (window, own) = (f.window.clone(), f.own.clone());
        cache.insert(t.pid, f);
        Some((now, window, own))
    }

    pub fn read(t: &Target) -> super::Found {
        if unsafe { AXIsProcessTrusted() } == 0 {
            return super::Found::Blocked(Blocked::Accessibility);
        }
        let Some(app) = app(t.pid) else { return super::Found::Nothing };
        if t.service != Service::WhatsApp {
            show_pages(t.pid, &app);
        }
        let Some((controls, window, own)) = controls(t, &app) else { return super::Found::Nothing };
        // Teams' hand is just "Mano", and says it's raised as an expanded button; the microphone and
        // the video of Slack's page before a huddle are switches, on or off. WhatsApp's names say
        // the state, and its camera's is read with its picture of you
        let state = |c: Control| {
            controls.get(&c).and_then(|(e, l)| {
                if t.service == Service::WhatsApp {
                    return match c {
                        Control::Camera => whatsapp_camera(l, own.as_ref().map(label).as_deref()),
                        _ => whatsapp(l).and_then(|(_, on)| on),
                    };
                }
                on_now(c, l).or_else(|| match c {
                    Control::Hand => number(e, "AXExpanded").map(|n| n != 0.0),
                    Control::Mic | Control::Camera => number(e, "AXValue").map(|n| n != 0.0),
                    _ => None,
                })
            })
        };
        // Teams names its windows: "Weekly sync | …" a call's, "Partecipazione alla riunione |
        // Weekly sync | …" the page before it
        let title: Vec<String> = match (t.service, &window) {
            (Service::Teams, Some(w)) => string(w, "AXTitle").split(" | ").map(|p| p.trim().to_string()).collect(),
            _ => Vec::new(),
        };
        // WhatsApp's window is called after who the call is with, ringing or on
        let (before, during) = match (t.service, &window) {
            (Service::WhatsApp, Some(w)) => {
                let who = whatsapp_name(&string(w, "AXTitle"));
                (who.clone(), who)
            }
            _ => (title.get(1).cloned().unwrap_or_default(), title.first().cloned().unwrap_or_default()),
        };
        // The page before joining: the microphone and the camera of the preview, and Join
        if !controls.contains_key(&Control::Leave)
            && let Some((join, _)) = controls.get(&Control::Join)
        {
            return super::Found::Call(Call {
                service: t.service,
                bundle: t.bundle.clone(),
                name: before,
                mic: state(Control::Mic),
                camera: state(Control::Camera),
                hand: None,
                leave: false,
                lobby: true,
                join: number(join, "AXEnabled") != Some(0.0),
                pid: t.pid,
                ..Default::default()
            });
        }
        // A call has a way out and a microphone: a Leave alone is something else of the app
        if !controls.contains_key(&Control::Leave) || !controls.contains_key(&Control::Mic) {
            return super::Found::Nothing;
        }
        super::Found::Call(Call {
            service: t.service,
            bundle: t.bundle.clone(),
            name: during,
            mic: state(Control::Mic),
            camera: state(Control::Camera),
            hand: controls.contains_key(&Control::Hand).then(|| state(Control::Hand).unwrap_or(false)),
            leave: true,
            pid: t.pid,
            ..Default::default()
        })
    }

    pub fn press(t: &Target, control: Control) -> bool {
        let Some(app) = app(t.pid) else { return false };
        let Some((controls, _, _)) = controls(t, &app) else { return false };
        let Some((e, _)) = controls.get(&control) else { return false };
        unsafe { AXUIElementPerformAction(cf(&*e.0), cf(&*NSString::from_str("AXPress"))) == 0 }
    }

    pub fn show(t: &Target) {
        let Some(app) = app(t.pid) else { return };
        if let Some((_, Some(window), _)) = controls(t, &app) {
            unsafe { AXUIElementPerformAction(cf(&*window.0), cf(&*NSString::from_str("AXRaise"))) };
        }
    }

    pub fn dump(pid: i32) -> String {
        if unsafe { AXIsProcessTrusted() } == 0 {
            return "RustyBar doesn't have Accessibility\n".into();
        }
        let Some(app) = app(pid) else { return String::new() };
        show_pages(pid, &app);
        show_web_view(pid);
        let mut out = String::new();
        walk(&app, |e, _, depth| {
            let mut line = format!("{}{}", "  ".repeat(depth), string(e, "AXRole"));
            for name in ["AXSubrole", "AXTitle", "AXDescription", "AXHelp", "AXIdentifier", "AXDOMIdentifier"] {
                let s = string(e, name);
                if !s.is_empty() {
                    line.push_str(&format!(" {name}={s:?}"));
                }
            }
            if let Some(v) = attribute(e, "AXValue") {
                if let Ok(n) = v.clone().downcast::<NSNumber>() {
                    line.push_str(&format!(" AXValue={}", n.doubleValue()));
                } else if let Ok(s) = v.downcast::<NSString>() {
                    let s = s.to_string();
                    if !s.is_empty() {
                        line.push_str(&format!(" AXValue={:?}", s.chars().take(60).collect::<String>()));
                    }
                }
            }
            out.push_str(&line);
            out.push('\n');
            true
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buttons_say_what_a_click_does() {
        use ax::{classify, on_now};
        let teams = |l: &str| classify(Service::Teams, &l.to_lowercase());
        assert_eq!(teams("Mute mic (⌘+Shift+M)"), Some(Control::Mic));
        assert_eq!(on_now(Control::Mic, "mute mic (⌘+shift+m)"), Some(true));
        assert_eq!(on_now(Control::Mic, "unmute mic"), Some(false));
        assert_eq!(teams("Disattiva audio"), Some(Control::Mic));
        assert_eq!(on_now(Control::Mic, "riattiva audio"), Some(false));
        assert_eq!(teams("Turn camera off"), Some(Control::Camera));
        assert_eq!(on_now(Control::Camera, "turn camera off"), Some(true));
        assert_eq!(on_now(Control::Camera, "attiva fotocamera"), Some(false));
        assert_eq!(on_now(Control::Camera, "disattiva fotocamera"), Some(true));
        assert_eq!(teams("Raise hand"), Some(Control::Hand));
        assert_eq!(on_now(Control::Hand, "lower hand"), Some(true));
        assert_eq!(teams("Leave (⌘+Shift+H)"), Some(Control::Leave));
        // Slack's leave names the huddle, so that no other Leave is taken for it
        assert_eq!(classify(Service::Slack, "leave channel"), None);
        assert_eq!(classify(Service::Slack, "leave huddle"), Some(Control::Leave));
        assert_eq!(classify(Service::Slack, "share screen"), None);
    }

    /// The names the two apps really give, from the strings of their clients and a real Teams
    /// meeting in Italian.
    #[test]
    fn a_calls_buttons_are_told_from_the_apps_other_ones() {
        use ax::{by_id, classify, on_now};
        let slack = |l: &str| classify(Service::Slack, &l.to_lowercase());
        let teams = |l: &str| classify(Service::Teams, &l.to_lowercase());
        assert_eq!(slack("Mute microphone"), Some(Control::Mic));
        assert_eq!(on_now(Control::Mic, "unmute microphone"), Some(false));
        assert_eq!(slack("Turn on camera"), Some(Control::Camera));
        assert_eq!(on_now(Control::Camera, "turn off my video"), Some(true));
        assert_eq!(slack("Leave Huddle"), Some(Control::Leave));
        assert_eq!(slack("Mute conversation"), None);
        assert_eq!(slack("Turn on captions for this video"), None);
        assert_eq!(slack("Record video clip"), None);
        assert_eq!(slack("Start a Huddle"), None);
        assert_eq!(slack("Mano"), None);
        assert_eq!(teams("Riattiva microfono"), Some(Control::Mic));
        assert_eq!(on_now(Control::Mic, "riattiva microfono"), Some(false));
        assert_eq!(teams("Attiva videocamera"), Some(Control::Camera));
        assert_eq!(on_now(Control::Camera, "disattiva videocamera"), Some(true));
        assert_eq!(teams("Abbandona"), Some(Control::Leave));
        assert_eq!(teams("Abbandona canale"), None);
        assert_eq!(teams("Esci dalla community"), None);
        assert_eq!(teams("Disattiva audio partecipante"), None);
        assert_eq!(teams("Disattiva tutto il video"), None);
        assert_eq!(teams("Disattiva microfono e altoparlanti"), None);
        // Teams' hand has no verb: its state is the button's, not its name's
        assert_eq!(teams("Mano"), Some(Control::Hand));
        assert_eq!(on_now(Control::Hand, "mano"), None);
        assert_eq!(by_id(Service::Teams, "hangup-button"), Some(Control::Leave));
        assert_eq!(by_id(Service::Teams, "raisehands-button"), Some(Control::Hand));
        assert_eq!(by_id(Service::Teams, "share-button"), None);
        assert_eq!(by_id(Service::Slack, "hangup-button"), None);
    }

    /// WhatsApp's names, as a real call had them (Italian, with the mark it puts in front) and as
    /// its strings have them in English: they say the state, and the camera's the opposite of it.
    #[test]
    fn whatsapp_names_its_buttons_by_their_state() {
        use ax::{classify, whatsapp, whatsapp_camera, whatsapp_name};
        let wa = |l: &str| whatsapp(&l.to_lowercase());
        assert_eq!(wa("\u{200e}audio riattivato"), Some((Control::Mic, Some(true))));
        assert_eq!(wa("\u{200e}audio disattivato"), Some((Control::Mic, Some(false))));
        assert_eq!(wa("\u{200e}microfono disattivato"), Some((Control::Mic, Some(false))));
        assert_eq!(wa("mute off"), Some((Control::Mic, Some(true))));
        assert_eq!(wa("call muted"), Some((Control::Mic, Some(false))));
        assert_eq!(wa("\u{200e}abbandona chiamata"), Some((Control::Leave, None)));
        assert_eq!(wa("\u{200e}Fotocamera non disponibile"), Some((Control::Camera, None)));
        assert_eq!(wa("\u{200e}Accetta"), Some((Control::Join, None)));
        // What else the call's bar and the app have
        for other in ["reaction", "\u{200e}Chat", "\u{200e}Aggiungi persone", "\u{200e}Avvia la condivisione dello schermo", "\u{200e}Selezione dei dispositivi audio", "\u{200e}Rifiuta", "\u{200e}Abbandona gruppo", "\u{200e}Avvia chiamata con Ada Lovelace"] {
            assert_eq!(wa(other), None, "{other}");
        }
        assert_eq!(classify(Service::WhatsApp, "\u{200e}audio riattivato"), Some(Control::Mic));
        assert_eq!(classify(Service::WhatsApp, "mute"), None);
        assert_eq!(classify(Service::Teams, "audio riattivato"), None);
        // The camera: a video call had the button on "disattivata" with the camera on, and the
        // picture of you without "Fotocamera disattivata"
        assert_eq!(whatsapp_camera("\u{200e}fotocamera disattivata", Some("\u{200e}feed video ridotto")), Some(true));
        assert_eq!(whatsapp_camera("\u{200e}fotocamera disattivata", Some("\u{200e}feed video ridotto, \u{200e}microfono disattivato")), Some(true));
        assert_eq!(whatsapp_camera("\u{200e}fotocamera attiva", Some("\u{200e}feed video ridotto, \u{200e}fotocamera disattivata")), Some(false));
        assert_eq!(whatsapp_camera("\u{200e}fotocamera non disponibile", Some("\u{200e}feed video ridotto, \u{200e}fotocamera disattivata")), None);
        // without the picture, the button's name the other way round
        assert_eq!(whatsapp_camera("\u{200e}fotocamera disattivata", None), Some(true));
        assert_eq!(whatsapp_camera("camera on", None), Some(false));
        // and should WhatsApp put the name right, the picture still says how it is
        assert_eq!(whatsapp_camera("\u{200e}fotocamera attiva", Some("\u{200e}feed video ridotto")), Some(true));
        assert_eq!(whatsapp_camera("camera on", Some("my minimized video feed, camera off")), Some(false));
        assert_eq!(whatsapp_name("\u{200e}Ada Lovelace - Videochiamata WhatsApp"), "Ada Lovelace");
        assert_eq!(whatsapp_name("Book club - evening - Chiamata di gruppo"), "Book club - evening");
        assert_eq!(whatsapp_name("WhatsApp"), "");
    }

    /// The page before a call: Teams' "Partecipazione alla riunione" and Slack's "Huddle Preview".
    #[test]
    fn the_page_before_a_call_has_its_way_in() {
        use ax::{by_id, classify, on_now};
        let slack = |l: &str| classify(Service::Slack, &l.to_lowercase());
        let teams = |l: &str| classify(Service::Teams, &l.to_lowercase());
        assert_eq!(by_id(Service::Teams, "prejoin-join-button"), Some(Control::Join));
        assert_eq!(teams("Partecipa ora"), Some(Control::Join));
        assert_eq!(teams("Join now"), Some(Control::Join));
        assert_eq!(teams("Partecipa"), None);
        assert_eq!(teams("Riattiva microfono (⇧ ⌘ M)"), Some(Control::Mic));
        assert_eq!(on_now(Control::Mic, "riattiva microfono (⇧ ⌘ m)"), Some(false));
        assert_eq!(teams("Disattiva l'altoparlante (⇧ ⌘ U)"), None);
        assert_eq!(teams("La videocamera non è disponibile"), None);
        assert_eq!(slack("Start Huddle"), Some(Control::Join));
        assert_eq!(slack("Start a Huddle"), None);
        assert_eq!(slack("Start huddle with Ada Lovelace"), None);
        // Slack's switches there have no verb: their state is their value, not their name
        assert_eq!(slack("Microphone"), Some(Control::Mic));
        assert_eq!(slack("Video"), Some(Control::Camera));
        assert_eq!(on_now(Control::Mic, "microphone"), None);
        assert_eq!(on_now(Control::Join, "start huddle"), None);
        assert_eq!(teams("Video"), None);
        assert_eq!(slack("Join now"), None);
    }
}
