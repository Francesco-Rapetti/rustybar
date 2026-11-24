//! WhatsApp's unread chats: how many messages wait, who wrote, and a popup to read them and
//! answer without opening the app. On a screen with a notch they're in the notch: after
//! `now_playing` in the center group, the widget is the island while nothing plays (the picture
//! and the name of who wrote last, the count), and a sign in the music's island while something
//! does; a new message grows the island for a moment, with who wrote and how it starts.
//!
//! The chats come from the database of WhatsApp's Mac app (see `wa`), read again each time the
//! app writes it: no polling, nothing asked of WhatsApp. The popup lists the chats with something
//! unread, then the latest of the others; a click shows the chat: texts, pictures, videos and
//! documents (a click opens them), voice notes (played here), links. Reading a chat here doesn't
//! tell WhatsApp: who wrote sees no blue ticks, and the chat leaves the bar's green count until
//! something new comes; while WhatsApp still counts it, it stays in the bar dimmed (gray, with
//! WhatsApp's number), so that the bar doesn't say nothing while the app says one. A reply typed here is sent through the app, with Accessibility: the app
//! comes to the front for a moment and opens the chat, so WhatsApp marks it read.
//!
//! The same widget is `telegram`, with Telegram's chats (see `tg`): there RustyBar is a client of
//! its own, so a reply and a chat marked read go straight to Telegram, without its app, and what
//! a chat shows is downloaded when the chat opens. `channels` (false) follows the channels too.
//!
//! Options: `recent` (how many chats without anything unread the popup lists, 5), `muted` (count
//! the muted chats too, false), `groups` (true), `preview` (the text of a new message in the
//! notch, true), `notch` (true), `hover` (the peek, true), `always` (in the bar with nothing
//! unread too, false).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{AnyThread, msg_send, sel};
use objc2_foundation::{NSString, NSURL, NSUserDefaults, ns_string};
use serde::Deserialize;

use super::{nc, tg};
use super::wa::{self, Chat, Kind as Is, Message};
use super::{Cx, Event, Island, Rx, Widget};
use crate::config::Options;
use crate::gfx::{Rgba, Weight};
use crate::i18n::t;
use crate::theme::Theme;
use crate::time;
use crate::ui::{Action, Align, Justify, Kind, Node, wrap};

/// Whose chats a widget shows: all that differs between `whatsapp`, `telegram`, `slack`, `teams`
/// and the widget of any other app (see `for_app`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    WhatsApp,
    Telegram,
    Slack,
    Teams,
    /// Any app that notifies: what it tells macOS, as Slack and Teams.
    App(&'static AppInfo),
}

/// An app with a widget of its own, for as long as the bar runs.
#[derive(Debug, PartialEq, Eq)]
pub struct AppInfo {
    bundle: &'static str,
    /// `app-<bundle>`: the widget's name, and of its defaults.
    key: &'static str,
    name: &'static str,
}

thread_local! {
    /// The apps that got a widget, each once: a reload finds them here.
    static APPS: RefCell<Vec<&'static AppInfo>> = const { RefCell::new(Vec::new()) };
}

impl Service {
    /// All of them: the four, then the apps with a widget.
    fn all() -> Vec<Service> {
        let mut all = vec![Service::WhatsApp, Service::Telegram, Service::Slack, Service::Teams];
        APPS.with(|a| all.extend(a.borrow().iter().map(|a| Service::App(a))));
        all
    }

    /// The service of the app `bundle`: Slack's and Teams' own, or one made for it.
    fn of(bundle: &str) -> Service {
        if bundle == super::call::SLACK {
            return Service::Slack;
        }
        if bundle == super::call::TEAMS {
            return Service::Teams;
        }
        APPS.with(|apps| {
            let mut apps = apps.borrow_mut();
            if let Some(app) = apps.iter().find(|a| a.bundle == bundle) {
                return Service::App(app);
            }
            let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
            // As the Finder calls it; else the end of its identifier
            let name = objc2_app_kit::NSWorkspace::sharedWorkspace()
                .URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle))
                .and_then(|url| url.path())
                .map(|path| objc2_foundation::NSFileManager::defaultManager().displayNameAtPath(&path).to_string())
                .map(|name| name.strip_suffix(".app").unwrap_or(&name).to_string())
                .unwrap_or_else(|| bundle.rsplit('.').next().unwrap_or(bundle).to_string());
            let app: &'static AppInfo = Box::leak(Box::new(AppInfo { bundle: leak(bundle.to_string()), key: leak(format!("app-{bundle}")), name: leak(name) }));
            apps.push(app);
            Service::App(app)
        })
    }

    /// The name of the widget's kind: of its made-up data, its defaults and its triggers too.
    fn key(self) -> &'static str {
        match self {
            Service::WhatsApp => "whatsapp",
            Service::Telegram => "telegram",
            Service::Slack => "slack",
            Service::Teams => "teams",
            Service::App(app) => app.key,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Service::WhatsApp => "WhatsApp",
            Service::Telegram => "Telegram",
            Service::Slack => "Slack",
            Service::Teams => "Teams",
            Service::App(app) => app.name,
        }
    }

    /// WhatsApp's green, Telegram's blue, the red of Slack's mark, Teams' violet; macOS's blue
    /// for any other app.
    fn color(self) -> Rgba {
        match self {
            Service::WhatsApp => Rgba(0.145, 0.827, 0.4, 1.0),
            Service::Telegram => Rgba(0.165, 0.671, 0.933, 1.0),
            Service::Slack => Rgba(0.878, 0.118, 0.353, 1.0),
            Service::Teams => Rgba(0.482, 0.514, 0.922, 1.0),
            Service::App(_) => Rgba(0.04, 0.518, 1.0, 1.0),
        }
    }

    /// What's written on that color.
    fn ink(self) -> Rgba {
        match self {
            Service::WhatsApp => Rgba(0.0, 0.0, 0.0, 1.0),
            _ => Rgba(1.0, 1.0, 1.0, 1.0),
        }
    }

    fn bundle(self) -> &'static str {
        match self {
            Service::WhatsApp => wa::BUNDLE,
            Service::Telegram => tg::BUNDLE,
            Service::Slack => super::call::SLACK,
            Service::Teams => super::call::TEAMS,
            Service::App(app) => app.bundle,
        }
    }

    fn symbol(self) -> &'static str {
        match self {
            Service::WhatsApp => "phone.bubble.fill",
            Service::Telegram => "paperplane.fill",
            Service::Slack => "number",
            Service::Teams => "person.2.fill",
            Service::App(_) => "app.badge.fill",
        }
    }

    /// Slack, Teams and any other app: the messages are what the app notifies on this Mac
    /// (see `nc`).
    fn notified(self) -> Option<nc::App> {
        match self {
            Service::Slack => Some(nc::App::Slack),
            Service::Teams => Some(nc::App::Teams),
            Service::App(_) => Some(nc::App::Other),
            _ => None,
        }
    }
}

/// The bundle identifier of an entry of `notifications`' `apps`: itself, or that of the app with
/// that name in the Applications folders. None for a name no app has.
pub fn bundle_of(entry: &str) -> Option<String> {
    if entry.contains('.') && !entry.contains([' ', '/']) && !entry.ends_with(".app") {
        return Some(entry.to_string());
    }
    let home = crate::config::home();
    [std::path::PathBuf::from("/Applications"), "/System/Applications".into(), home.join("Applications")]
        .iter()
        .map(|dir| dir.join(format!("{}.app", entry.trim_end_matches(".app"))))
        .filter(|path| path.exists())
        .find_map(|path| objc2_foundation::NSBundle::bundleWithPath(&NSString::from_str(&path.to_string_lossy()))?.bundleIdentifier().map(|b| b.to_string()))
}

/// The app whose messages a kind of widget shows, for the kinds that show an app's.
pub fn bundle_of_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "whatsapp" => Some(wa::BUNDLE),
        "telegram" => Some(tg::BUNDLE),
        "slack" => Some(super::call::SLACK),
        "teams" => Some(super::call::TEAMS),
        _ => None,
    }
}

/// The widget of the app `bundle`, an entry of `notifications`' `apps`: what the app notifies,
/// as `slack` shows Slack's, and its name in the bar. None with the made-up data for an app
/// they have nothing of: `notifications` then keeps its badge.
pub fn for_app(bundle: &str, options: &Options) -> Option<(String, WhatsApp)> {
    let service = Service::of(bundle);
    let mocking = std::env::var_os("RUSTYBAR_MOCK").is_some();
    if mocking && matches!(service, Service::App(_)) {
        return None;
    }
    Some((service.key().to_string(), WhatsApp::new(options, service)))
}

const WIDTH: f64 = 340.0;
/// How wide the text of a message gets before it goes on another line.
const BUBBLE: f64 = 232.0;
/// How long after a message came the island of another widget shows it (see `guest_notice`).
const FRESH: f64 = 4.0;

#[derive(Clone, Debug, PartialEq)]
enum State {
    Loading,
    Ready,
    /// WhatsApp isn't installed, or never opened.
    Missing,
    /// macOS doesn't let RustyBar read its data.
    Denied,
    /// Telegram has no session: `rustybar telegram login`.
    SignedOut,
    Failed(String),
}

/// What a read on a thread brings: WhatsApp's chats, or the notifications of Slack or Teams.
enum Read {
    Chats(Vec<Chat>),
    /// The notifications, the number on the Dock icon, and whether macOS let read the former.
    Notes(Vec<nc::Note>, Option<u64>, bool),
}

/// A message that just came, for the notice.
#[derive(Clone, Debug, PartialEq)]
struct Arrival {
    jid: String,
    id: i64,
    at: f64,
}

/// A voice note that plays: where it was `at`, to go on from there.
struct Playing {
    id: i64,
    /// None with the made-up data, which only counts the time.
    sound: Option<Sound>,
    from: f64,
    at: f64,
    length: f64,
    paused: bool,
}

impl Playing {
    fn position(&self, now: f64) -> f64 {
        match &self.sound {
            Some(s) => s.position(),
            None if self.paused => self.from,
            None => (self.from + now - self.at).min(self.length),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Mock {
    chats: Vec<Chat>,
    /// What `msg trigger whatsapp_message` brings, one at a time: a chat's jid and a message.
    incoming: Vec<Incoming>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Incoming {
    jid: String,
    message: Message,
}

/// What the island of another widget shows of this one (see `island_mark`, `guest_notice`), set
/// for its generation.
#[derive(Clone)]
struct Shared {
    service: Service,
    generation: u64,
    widget: String,
    unread: i64,
    /// The messages WhatsApp still counts of the chats read here (see `WhatsApp::held`).
    held: i64,
    /// A voice note plays.
    playing: bool,
    /// The chat of the message that just came, with that message alone, and when it came.
    arrival: Option<(Chat, f64)>,
    preview: bool,
}

thread_local! {
    static SHARED: RefCell<Vec<Shared>> = const { RefCell::new(Vec::new()) };
}

fn shared(service: Service) -> Option<Shared> {
    SHARED.with(|s| s.borrow().iter().find(|s| s.service == service).cloned()).filter(|s| s.generation == crate::app::current_gen())
}

/// The unread count in the island of another widget, right of the notch: the app's icon and the
/// number, dimmed when all of it was read in the bar and only WhatsApp still counts it, and a
/// wave while a voice note plays; a click opens the chats. None with nothing unread and nothing
/// playing, or without a `whatsapp` that wants the notch.
pub fn island_mark(th: &Theme) -> Option<Node> {
    let marks: Vec<Node> = Service::all().into_iter().filter_map(|service| service_mark(th, service)).collect();
    match marks.len() {
        0 => None,
        1 => marks.into_iter().next(),
        _ => Some(Node::row(marks).gap(4.0).align(Align::Center).key("chats")),
    }
}

/// The mark of one of the two (see `island_mark`).
fn service_mark(th: &Theme, service: Service) -> Option<Node> {
    let s = shared(service).filter(|s| s.unread + s.held > 0 || s.playing)?;
    let (n, color) = tally(th, service, s.unread, s.held);
    let mut signs = Vec::new();
    if s.playing {
        signs.push(listening(service));
    }
    signs.push(sign(service, 12.0, if n > 0 { color } else { service.color() }));
    if n > 0 {
        signs.push(Node::text(count(n), th.font_with(Weight::Bold, 12.0), color).key("count"));
    }
    Some(
        Node::row(signs)
            .key(service.key())
            .gap(4.0)
            .padding(1.0, 4.0, 1.0, 4.0)
            .radius(6.0)
            .click(Action::Popup { widget: s.widget })
            .hover(service.color().faded(0.25)),
    )
}

/// The message that just came, for the island of another widget to show as its own notice.
pub fn guest_notice(rx: &Rx) -> Option<Node> {
    // Of the two, the one that came last
    let now = time::now();
    let s = Service::all().into_iter().filter_map(shared).filter(|s| s.arrival.as_ref().is_some_and(|(_, at)| now - at < FRESH)).max_by(|a, b| a.arrival.as_ref().map(|a| a.1).partial_cmp(&b.arrival.as_ref().map(|a| a.1)).unwrap_or(std::cmp::Ordering::Equal))?;
    let (chat, _) = s.arrival.as_ref()?;
    let theme = notch_theme(rx.theme, s.service);
    notice_node(&theme, s.service, &s.widget, chat, chat.messages.last()?, s.preview, WIDTH)
}

/// The notch's colors with the service's own.
fn notch_theme(theme: &Theme, service: Service) -> Theme {
    theme.notch(Some((service.color(), service.ink())))
}

pub struct WhatsApp {
    service: Service,
    recent: usize,
    /// Telegram's channels are followed too.
    channels: bool,
    muted: bool,
    groups: bool,
    preview: bool,
    notch: bool,
    hover: bool,
    always: bool,
    state: State,
    chats: Vec<Chat>,
    /// The chats read here, by jid, with the id of their last message then: out of the count
    /// until another comes. In the defaults (not with the made-up data).
    seen: HashMap<String, i64>,
    /// The highest id of a message at the last read, to tell what's new; none before the first.
    newest: Option<i64>,
    arrival: Option<Arrival>,
    /// The chat the popup shows, by jid; none, the list.
    open: Option<String>,
    /// The popup opens on `open`, not on the list: a click on a chat outside it.
    keep: bool,
    /// What's typed and not sent, by jid.
    drafts: HashMap<String, String>,
    sending: bool,
    send_error: Option<String>,
    playing: Option<Playing>,
    watcher: Option<wa::Watch>,
    /// Telegram's connection.
    telegram: Option<tg::Handle>,
    /// Slack's and Teams' conversations, made of their notifications.
    book: nc::Book,
    /// macOS lets read those notifications; false, only the Dock's number is known.
    texts: bool,
    /// The app was seen with a number on its Dock icon, now or before (in the defaults): then
    /// that number says what waits.
    badged: bool,
    /// The messages whose file Telegram is asked for, after a click.
    fetching: HashSet<i64>,
    /// A read is under way; `again`, the database changed meanwhile.
    reading: bool,
    again: bool,
    mock: Option<Mock>,
}

fn defaults() -> Option<Retained<NSUserDefaults>> {
    NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(ns_string!("rustybar")))
}

/// The defaults key of the chats read here: a JSON object, jid → message id.
fn seen_key(service: Service) -> Retained<NSString> {
    NSString::from_str(&format!("{}.seen", service.key()))
}

fn count(n: i64) -> String {
    if n > 99 { "99+".into() } else { n.to_string() }
}

/// The service's sign on the notch's black, where the app's icon (WhatsApp's is dark, as macOS
/// draws it now) is lost; for any other app there's only its icon.
fn sign(service: Service, size: f64, color: Rgba) -> Node {
    match service {
        Service::App(app) => Node::app_icon(app.bundle, size + 3.0).opacity(if color == service.color() { 1.0 } else { 0.6 }).key("icon"),
        _ => Node::symbol(service.symbol(), size, Weight::Semibold, color).key("icon"),
    }
}

/// A voice note plays: for the notch.
fn listening(service: Service) -> Node {
    Node::symbol("waveform", 12.0, Weight::Semibold, service.color()).key("listening")
}

/// The number the bar shows and its color: the `new` messages in green, or with none of them
/// those `held` (read here, unread for WhatsApp), dimmed.
fn tally(th: &Theme, service: Service, new: i64, held: i64) -> (i64, Rgba) {
    if new > 0 { (new, service.color()) } else { (held, th.subtext) }
}

/// The app's icon; the service's sign in its color where the app isn't installed.
fn glyph(service: Service, size: f64) -> Node {
    thread_local! {
        static INSTALLED: RefCell<HashMap<&'static str, bool>> = RefCell::new(HashMap::new());
    }
    let installed = INSTALLED.with(|i| *i.borrow_mut().entry(service.bundle()).or_insert_with(|| objc2_app_kit::NSWorkspace::sharedWorkspace().URLForApplicationWithBundleIdentifier(&NSString::from_str(service.bundle())).is_some()));
    if installed { Node::app_icon(service.bundle(), size).key("icon") } else { Node::symbol(service.symbol(), size - 1.0, Weight::Semibold, service.color()).key("icon") }
}

/// `19:42`.
fn clock(at: f64) -> String {
    let tm = time::local(at);
    format!("{}:{:02}", tm.tm_hour, tm.tm_min)
}

/// The day of `at` as a chat has it between its messages: Today, Yesterday, or the date.
fn day(at: f64, now: f64) -> String {
    let (a, n) = (time::local(at), time::local(now));
    let days = |tm: &libc::tm| tm.tm_year as i64 * 366 + tm.tm_yday as i64;
    match days(&n) - days(&a) {
        0 => t("Oggi", "Today").into(),
        1 => t("Ieri", "Yesterday").into(),
        _ => format!("{}/{}/{}", a.tm_mday, a.tm_mon + 1, a.tm_year + 1900),
    }
}

/// When a chat's last message came, for the list: the time today, else the day.
fn when(at: f64, now: f64) -> String {
    let (a, n) = (time::local(at), time::local(now));
    if a.tm_year == n.tm_year && a.tm_yday == n.tm_yday { clock(at) } else { day(at, now) }
}

/// `0:14`.
fn length(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// The first link in a text.
fn link(text: &str) -> Option<&str> {
    text.split_whitespace().find(|w| w.starts_with("https://") || w.starts_with("http://"))
}

/// `example.com` of `https://www.example.com/a`.
fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.strip_prefix("www.").unwrap_or(host)
}

/// A message in a line: a symbol for what isn't a text, and its words.
fn summary(m: &Message) -> (Option<&'static str>, String) {
    let or = |text: &str, other: &str| if text.is_empty() { other.to_string() } else { text.lines().next().unwrap_or_default().to_string() };
    match m.kind {
        Is::Text | Is::Link | Is::Other => (None, or(&m.text, t("Messaggio", "Message"))),
        Is::Image => (Some("camera.fill"), or(&m.text, t("Foto", "Photo"))),
        Is::Video => (Some("video.fill"), or(&m.text, "Video")),
        Is::Gif => (Some("photo.fill"), or(&m.text, "GIF")),
        Is::Voice => (Some("mic.fill"), length(m.seconds)),
        Is::Contact => (Some("person.fill"), or(&m.title, t("Contatto", "Contact"))),
        Is::Location => (Some("mappin.and.ellipse"), t("Posizione", "Location").into()),
        Is::File => (Some("doc.fill"), or(&m.title, t("Documento", "Document"))),
        Is::Sticker => (Some("face.smiling"), "Sticker".into()),
        Is::Deleted => (Some("nosign"), t("Messaggio eliminato", "Deleted message").into()),
        Is::Poll => (Some("chart.bar.fill"), or(&m.text, t("Sondaggio", "Poll"))),
        Is::Once => (Some("1.circle"), t("Visualizzabile una volta", "View once").into()),
    }
}

/// The first name of who wrote in a group.
fn first_name(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}

impl WhatsApp {
    pub fn new(options: &Options, service: Service) -> WhatsApp {
        let mock = crate::mock::load::<Mock>(service.key());
        let seen = match &mock {
            Some(_) => HashMap::new(),
            None => defaults().and_then(|d| d.stringForKey(&seen_key(service))).and_then(|s| serde_json::from_str(&s.to_string()).ok()).unwrap_or_default(),
        };
        WhatsApp {
            service,
            channels: options.bool("channels").unwrap_or(false),
            telegram: None,
            book: nc::Book::default(),
            texts: true,
            badged: mock.is_none() && defaults().is_some_and(|d| d.boolForKey(&NSString::from_str(&format!("{}.badged", service.key())))),
            fetching: HashSet::new(),
            recent: options.f64("recent").map_or(5, |n| n.max(0.0) as usize),
            muted: options.bool("muted").unwrap_or(false),
            groups: options.bool("groups").unwrap_or(true),
            preview: options.bool("preview").unwrap_or(true),
            notch: options.bool("notch").unwrap_or(true),
            hover: options.bool("hover").unwrap_or(true),
            always: options.bool("always").unwrap_or(false),
            state: State::Loading,
            chats: Vec::new(),
            seen,
            newest: None,
            arrival: None,
            open: None,
            keep: false,
            drafts: HashMap::new(),
            sending: false,
            send_error: None,
            playing: None,
            watcher: None,
            reading: false,
            again: false,
            mock,
        }
    }

    fn save_seen(&self) {
        if self.mock.is_some() {
            return;
        }
        let Some(d) = defaults() else { return };
        match serde_json::to_string(&self.seen) {
            Ok(json) if !self.seen.is_empty() => unsafe { d.setObject_forKey(Some(&NSString::from_str(&json)), &seen_key(self.service)) },
            _ => d.removeObjectForKey(&seen_key(self.service)),
        }
    }

    /// How many messages of `chat` wait, for the bar: WhatsApp's count, unless the chat is muted
    /// or a group left out, or was read here; then only what came after.
    fn unread(&self, chat: &Chat) -> i64 {
        if chat.unread <= 0 || (chat.muted && !self.muted) || (chat.group && !self.groups) {
            return 0;
        }
        match self.seen.get(&chat.jid) {
            None => chat.unread,
            Some(seen) if chat.newest <= *seen => 0,
            Some(seen) => (chat.messages.iter().filter(|m| !m.mine && m.id > *seen).count() as i64).clamp(1, chat.unread),
        }
    }

    fn total(&self) -> i64 {
        self.chats.iter().map(|c| self.unread(c)).sum()
    }

    /// How many messages of `chat` WhatsApp still counts though the chat was read here, up to
    /// its last message: they show dimmed, so that the bar doesn't say nothing while the app
    /// says one. 0 for a chat with something new, or that doesn't count.
    fn held(&self, chat: &Chat) -> i64 {
        let counts = chat.unread > 0 && (!chat.muted || self.muted) && (!chat.group || self.groups);
        if counts && self.unread(chat) == 0 { chat.unread } else { 0 }
    }

    fn held_total(&self) -> i64 {
        self.chats.iter().map(|c| self.held(c)).sum()
    }

    /// The chats with something unread, those with something new first, then those read only
    /// here; among each, the one written to last first.
    fn waiting(&self) -> Vec<&Chat> {
        let mut chats: Vec<&Chat> = self.chats.iter().filter(|c| self.unread(c) > 0 || self.held(c) > 0).collect();
        chats.sort_by(|a, b| (self.unread(b) > 0).cmp(&(self.unread(a) > 0)).then(b.at.total_cmp(&a.at)));
        chats
    }

    fn chat(&self, jid: &str) -> Option<&Chat> {
        self.chats.iter().find(|c| c.jid == jid)
    }

    fn opened(&self) -> Option<&Chat> {
        self.open.as_deref().and_then(|jid| self.chat(jid))
    }

    /// The chat `jid` is read here, up to its last message.
    fn see(&mut self, jid: &str) {
        let Some(chat) = self.chat(jid) else { return };
        if chat.unread > 0 && self.seen.get(jid) != Some(&chat.newest) {
            let newest = chat.newest;
            self.seen.insert(jid.to_string(), newest);
            self.save_seen();
        }
    }

    /// Reads the database again, on a thread; once more after, if it changes meanwhile.
    fn load(&mut self, cx: &mut Cx) {
        if self.mock.is_some() {
            return;
        }
        // Telegram's connection reads them again itself
        if self.service == Service::Telegram {
            if let Some(telegram) = &self.telegram {
                telegram.send(tg::Cmd::Refresh);
            }
            return;
        }
        if self.reading {
            self.again = true;
            return;
        }
        self.reading = true;
        let (recent, service) = (self.recent, self.service);
        cx.background(
            move || match service.notified() {
                // With the number on the app's Dock icon: how many of them still wait
                Some(_) => {
                    let waiting = super::notifications::dock_count(service.bundle());
                    match nc::read(&nc::database(), service.bundle()) {
                        Ok(notes) => Ok(Read::Notes(notes, waiting, true)),
                        // Without the texts there's still the number, where the Dock can be read
                        Err(wa::Error::Denied) if waiting.is_some() => Ok(Read::Notes(Vec::new(), waiting, false)),
                        Err(e) => Err(e),
                    }
                }
                None => wa::read(&wa::container(), recent).map(Read::Chats),
            },
            |w: &mut WhatsApp, result, cx| {
                w.reading = false;
                match result {
                    Ok(Read::Chats(chats)) => w.loaded(chats, cx),
                    Ok(Read::Notes(notes, waiting, texts)) => {
                        let Some(app) = w.service.notified() else { return };
                        w.texts = texts;
                        // An app that never has a number on its icon (Calendar) says nothing
                        // with none: what it notified since the bar started waits
                        if waiting.is_some_and(|n| n > 0) && !w.badged {
                            w.badged = true;
                            if let (Some(d), None) = (defaults(), &w.mock) {
                                d.setBool_forKey(true, &NSString::from_str(&format!("{}.badged", w.service.key())));
                            }
                        }
                        let waiting = waiting.filter(|_| w.badged || !matches!(w.service, Service::App(_)));
                        let chats = w.book.update(app, &notes, waiting.map(|n| n as usize), w.recent, w.service.name());
                        w.loaded(chats, cx);
                    }
                    Err(e) => {
                        let state = match e {
                            wa::Error::Missing => State::Missing,
                            wa::Error::Denied => State::Denied,
                            wa::Error::Other(e) => State::Failed(e),
                        };
                        if w.state != state {
                            if let State::Failed(e) = &state {
                                crate::log(&format!("{}: {e}", w.service.key()));
                            }
                            w.state = state;
                            w.chats.clear();
                            w.share(cx);
                            cx.redraw();
                        }
                    }
                }
                if std::mem::take(&mut w.again) {
                    w.load(cx);
                }
            },
        );
    }

    /// The chats as they are now: what's new among the unread grows the island for a moment,
    /// unless its chat is the one the popup shows.
    fn loaded(&mut self, chats: Vec<Chat>, cx: &mut Cx) {
        if self.state == State::Ready && self.chats == chats {
            return;
        }
        self.state = State::Ready;
        self.chats = chats;
        // A chat WhatsApp has read has nothing to remember
        let before = self.seen.len();
        let chats = &self.chats;
        self.seen.retain(|jid, _| chats.iter().any(|c| c.jid == *jid && c.unread > 0));
        if self.seen.len() != before {
            self.save_seen();
        }
        if cx.popup_open && let Some(jid) = self.open.clone() {
            self.see(&jid);
            // What its new messages show
            self.show_chat(&jid);
        }
        let last = self.chats.iter().filter(|c| self.unread(c) > 0).filter_map(|c| c.messages.iter().rev().find(|m| !m.mine).map(|m| (m.id, c.jid.clone()))).max();
        // Slack's and Teams' wait by the Dock's number, which may come after the notification:
        // one that doesn't wait yet is still new when it does
        let highest = match self.service.notified() {
            Some(_) => last.as_ref().map_or(0, |(id, _)| *id),
            None => self.chats.iter().map(|c| c.newest).max().unwrap_or(0),
        };
        if let (Some(before), Some((id, jid))) = (self.newest, last)
            && id > before
        {
            self.arrival = Some(Arrival { jid, id, at: time::now() });
            self.share(cx);
            cx.notice();
        } else {
            self.share(cx);
        }
        self.newest = Some(self.newest.unwrap_or(0).max(highest));
        cx.redraw();
    }

    /// The chat `jid` is on show: Telegram downloads what its messages show.
    fn show_chat(&self, jid: &str) {
        if let Some(telegram) = &self.telegram {
            telegram.send(tg::Cmd::Open(jid.to_string()));
        }
    }

    /// Tells the island of another widget what to show of this one.
    fn share(&self, cx: &Cx) {
        let shared = self.notch.then(|| Shared {
            service: self.service,
            generation: cx.generation,
            widget: cx.name.to_string(),
            unread: self.total(),
            held: self.held_total(),
            playing: self.playing.as_ref().is_some_and(|p| !p.paused),
            arrival: self.arrival.as_ref().and_then(|a| {
                let chat = self.chat(&a.jid)?;
                let message = chat.messages.iter().find(|m| m.id == a.id)?.clone();
                Some((Chat { messages: vec![message], ..chat.clone() }, a.at))
            }),
            preview: self.preview,
        });
        SHARED.with(|s| {
            let mut all = s.borrow_mut();
            all.retain(|s| s.service != self.service);
            all.extend(shared);
        });
    }

}

/// The picture of a chat, round; without one, its initials on a color of its own.
fn avatar(th: &Theme, chat: &Chat, size: f64) -> Node {
    if let Some(path) = &chat.picture {
        return Node::image(path.as_str(), size, size, None).radius(size / 2.0).key("avatar");
    }
    let initials: String = chat.name.split_whitespace().filter_map(|w| w.chars().find(|c| c.is_alphanumeric())).take(2).collect::<String>().to_uppercase();
    let colors = [th.green, th.yellow, th.peach, th.red, th.primary];
    let color = colors[chat.jid.bytes().map(usize::from).sum::<usize>() % colors.len()];
    let inside = if initials.is_empty() {
        Node::symbol(if chat.group { "person.2.fill" } else { "person.fill" }, size * 0.45, Weight::Semibold, color)
    } else {
        Node::text(initials, th.font_with(Weight::Semibold, (size * 0.38).max(8.0)), color)
    };
    Node::row(vec![inside.key("initials")]).size(size, size).radius(size / 2.0).justify(Justify::Center).background(color.faded(0.22)).key("avatar")
}

/// The notice of the message `m` of `chat`, on the notch's black (`th`): who wrote, in which group,
/// and how it starts (with `preview`). A click opens its chat in the popup of `widget`.
fn notice_node(th: &Theme, service: Service, widget: &str, chat: &Chat, m: &Message, preview: bool, width: f64) -> Option<Node> {
    let max = width - 28.0 - 44.0 - 12.0;
    let title = if chat.group && !m.sender.is_empty() { m.sender.as_str() } else { chat.name.as_str() };
    let mut head = vec![Node::text(title, th.font_with(Weight::Semibold, th.font_size + 1.0), th.text).max_width(max - if chat.group { 160.0 } else { 24.0 }).key("name"), Node::spacer(), sign(service, 13.0, service.color())];
    if chat.group {
        head.insert(1, Node::text(format!("  {}", chat.name), th.small_font(), th.subtext).max_width(130.0).key("group"));
    }
    let mut lines = vec![Node::row(head).key("head")];
    if preview {
        let (symbol, text) = summary(m);
        let font = th.font();
        let mut wrapped = wrap(&text, &font, max - if symbol.is_some() { 16.0 } else { 0.0 });
        wrapped.truncate(2);
        for (i, line) in wrapped.into_iter().enumerate() {
            let text = Node::text(line, font.clone(), th.subtext).key("text");
            lines.push(match symbol.filter(|_| i == 0) {
                Some(s) => Node::row(vec![Node::symbol(s, 11.0, Weight::Semibold, th.subtext).key("symbol"), text]).gap(5.0).key(format!("line{i}")),
                None => Node::row(vec![text]).key(format!("line{i}")),
            });
        }
    } else {
        lines.push(Node::text(t("Nuovo messaggio", "New message"), th.font(), th.subtext).key("text"));
    }
    Some(
        Node::row(vec![avatar(th, chat, 44.0), Node::column(lines).gap(2.0).grow().key("lines")])
            .gap(12.0)
            .align(Align::Center)
            .padding(6.0, 14.0, 14.0, 14.0)
            .min_width(width)
            .click(Action::Widget { widget: widget.to_string(), name: format!("chat:{}", chat.jid) })
            .hover(Rgba(0.0, 0.0, 0.0, 0.0)),
    )
}

impl WhatsApp {

    /// The last message of a chat in a line, who wrote it first in a group: for the list, the peek
    /// and the notice.
    fn preview_line(&self, th: &Theme, chat: &Chat, width: f64, color: Rgba) -> Node {
        let Some(m) = chat.messages.last() else { return Node::default().key("preview") };
        let (symbol, text) = summary(m);
        let who = if m.mine {
            format!("{}: ", t("Tu", "You"))
        } else if chat.group && !m.sender.is_empty() {
            format!("{}: ", first_name(&m.sender))
        } else {
            String::new()
        };
        let font = th.small_font();
        let mut row = Vec::new();
        let mut room = width;
        if !who.is_empty() {
            room -= crate::gfx::measure_text(&who, &font).width;
            row.push(Node::text(who, font.clone(), color).key("who"));
        }
        if let Some(s) = symbol {
            room -= 15.0;
            row.push(Node::symbol(s, 10.0, Weight::Semibold, color).padding(0.0, 4.0, 0.0, 0.0).key("symbol"));
        }
        row.push(Node::text(text, font, color).max_width(room.max(20.0)).key("text"));
        Node::row(row).key("preview")
    }

    /// The island (see the top): left of the notch the picture and the name of who wrote last,
    /// right of it the count. Compact, the picture alone.
    fn island_bar(&self, rx: &Rx, island: Island, chat: &Chat) -> Node {
        let theme = notch_theme(rx.theme, self.service);
        let rx = &Rx { theme: &theme, ..*rx };
        let th = rx.theme;
        let size = (island.height - 12.0).clamp(12.0, 28.0);
        let inset = ((island.height - size) / 2.0).round();
        // Nothing new, only what was read here: dimmed
        let (n, color) = tally(th, self.service, self.total(), self.held_total());
        let dim = color != self.service.color();
        let mut left = vec![avatar(th, chat, size).opacity(if dim { 0.6 } else { 1.0 })];
        if !rx.compact {
            let font = th.font();
            let whole = crate::gfx::measure_text(&chat.name, &font).width.ceil();
            let width = whole.min(64.0) + (whole - whole.min(64.0)).clamp(0.0, island.left.max(0.0));
            left.push(Node::text(&chat.name, font, if dim { th.subtext } else { th.text }).max_width(width).key("name"));
        }
        let left = Node::row(left).gap(8.0).align(Align::Center).height(island.height).padding(0.0, 8.0, 0.0, inset).key("wing");
        let mut right: Vec<Node> = super::fn_keys::island_mark(th).into_iter().collect();
        if self.playing.as_ref().is_some_and(|p| !p.paused) {
            right.push(listening(self.service));
        }
        right.push(sign(self.service, 13.0, color));
        right.push(Node::text(count(n), th.font_with(Weight::Bold, 12.0), color).key("count"));
        let right = Node::row(right).gap(6.0).align(Align::Center).height(island.height).padding(0.0, inset + 2.0, 0.0, 8.0).key("wing");
        rx.island(vec![left], vec![right]).click(rx.toggle_popup()).hover(Rgba(0.0, 0.0, 0.0, 0.0))
    }

    /// A row of the list: the picture, the name and when, the last message and how many wait.
    fn chat_row(&self, rx: &Rx, chat: &Chat, now: f64) -> Node {
        let th = rx.theme;
        let unread = self.unread(chat);
        let lit = unread > 0;
        let name = Node::text(&chat.name, th.font_with(if lit { Weight::Semibold } else { Weight::Medium }, th.font_size), th.text).max_width(WIDTH - 130.0).key("name");
        let mut top = vec![name, Node::spacer()];
        if chat.muted {
            top.push(Node::symbol("bell.slash.fill", 9.0, Weight::Semibold, th.overlay).padding(0.0, 6.0, 0.0, 0.0).key("muted"));
        }
        top.push(Node::text(when(chat.at, now), th.small_font(), if lit { self.service.color() } else { th.subtext }).key("when"));
        let mut bottom = vec![self.preview_line(th, chat, WIDTH - 120.0, if lit { th.text } else { th.subtext }), Node::spacer()];
        // Green for what's new; gray for what was read here and WhatsApp still counts
        let held = self.held(chat);
        if lit || held > 0 {
            let (ink, fill) = if lit { (self.service.ink(), self.service.color()) } else { (th.subtext, th.surface) };
            let badge = Node::row(vec![Node::text(count(if lit { unread } else { held }), th.font_with(Weight::Bold, 10.0), ink).key("n")])
                .height(16.0)
                .min_width(16.0)
                .padding(0.0, 5.0, 0.0, 5.0)
                .justify(Justify::Center)
                .radius(8.0)
                .background(fill)
                .key("badge");
            bottom.push(if lit { badge } else { badge.tip(format!("{} {}", t("Letta qui, non ancora su", "Read here, not yet in"), self.service.name())) });
        }
        let lines = Node::column(vec![Node::row(top).key("top"), Node::row(bottom).key("bottom")]).gap(2.0).grow().key("lines");
        Node::row(vec![avatar(th, chat, 34.0), lines])
            .gap(10.0)
            .padding(6.0, 8.0, 6.0, 8.0)
            .radius(th.row_radius + 2.0)
            .click(rx.action(&format!("chat:{}", chat.jid)))
            .hover(th.highlight)
            .opacity(if lit { 1.0 } else { 0.8 })
            .key(format!("chat:{}", chat.jid))
    }

    /// The popup's first page: the chats that wait, then the latest of the others.
    fn list(&self, rx: &Rx) -> Node {
        let th = rx.theme;
        let now = time::now();
        let open_app = Node::row(vec![Node::symbol("arrow.up.forward.app", 12.0, Weight::Semibold, th.subtext).key("symbol")])
            .size(24.0, 22.0)
            .justify(Justify::Center)
            .radius(6.0)
            .click(rx.action("app"))
            .hover(th.highlight)
            .tip(format!("{} {}", t("Apri", "Open"), self.service.name()))
            .key("app");
        let mut rows = vec![rx.section(self.service.name(), Some(open_app)).key("title")];
        let note = |symbol: &str, color: Rgba, lines: Vec<String>| {
            let font = th.small_font();
            let text: Vec<Node> = lines.iter().flat_map(|l| wrap(l, &font, WIDTH - 60.0)).enumerate().map(|(i, l)| Node::text(l, font.clone(), th.subtext).key(format!("l{i}"))).collect();
            Node::row(vec![Node::symbol(symbol, 14.0, Weight::Semibold, color).key("symbol"), Node::column(text).gap(1.0).key("text")])
                .gap(10.0)
                .align(Align::Start)
                .padding(6.0, 8.0, 8.0, 8.0)
                .key("note")
        };
        match &self.state {
            State::Loading => rows.push(note("ellipsis", th.subtext, vec![t("Leggo le chat…", "Reading the chats…").into()])),
            State::Missing if self.service.notified().is_some() => rows.push(note(
                "questionmark.app",
                th.yellow,
                vec![t("Non trovo il database delle notifiche di macOS.", "The database of macOS's notifications isn't there.").into()],
            )),
            State::Denied if self.service.notified().is_some() => {
                rows.push(note(
                    "lock.fill",
                    th.yellow,
                    vec![
                        format!(
                            "{} {} {}",
                            t("I messaggi di", "The messages of"),
                            self.service.name(),
                            t("vengono dalle sue notifiche, che macOS lascia leggere solo con Accesso completo al disco.", "come from its notifications, which macOS lets read only with Full Disk Access.")
                        ),
                        t("Aggiungi RustyBar all'elenco, poi riavvia la barra.", "Add RustyBar to the list, then restart the bar.").into(),
                    ],
                ));
                rows.push(
                    rx.row_button(
                        vec![Node::text(t("Apri Accesso completo al disco…", "Open Full Disk Access…"), th.font(), th.text).key("text")],
                        vec![],
                        Action::Open("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles".into()),
                    )
                    .key("privacy"),
                );
            }
            State::Missing => rows.push(note(
                "questionmark.app",
                th.yellow,
                vec![t("WhatsApp per Mac non è installata, o non è mai stata aperta: le chat vengono dalla sua app.", "WhatsApp for Mac isn't installed, or was never opened: the chats come from its app.").into()],
            )),
            State::Denied => {
                rows.push(note(
                    "lock.fill",
                    th.yellow,
                    vec![
                        t("macOS non lascia leggere a RustyBar i dati di WhatsApp: alla sua domanda è stato risposto di no.", "macOS doesn't let RustyBar read WhatsApp's data: its question was answered no.").into(),
                        t("Per farsela fare di nuovo: tccutil reset SystemPolicyAppData io.github.francesco-rapetti.rustybar nel Terminale, poi riavvia la barra. O dai a RustyBar Accesso completo al disco.", "To be asked again: tccutil reset SystemPolicyAppData io.github.francesco-rapetti.rustybar in Terminal, then restart the bar. Or give RustyBar Full Disk Access.").into(),
                    ],
                ));
                rows.push(
                    rx.row_button(vec![Node::text(t("Apri Privacy e sicurezza…", "Open Privacy & Security…"), th.font(), th.text).key("text")], vec![], Action::Open("x-apple.systempreferences:com.apple.preference.security?Privacy".into()))
                        .key("privacy"),
                );
            }
            State::SignedOut => rows.push(note(
                "person.crop.circle.badge.questionmark",
                th.yellow,
                vec![
                    t("RustyBar non è entrato in Telegram. Nel Terminale: rustybar telegram login", "RustyBar isn't signed in to Telegram. In Terminal: rustybar telegram login").into(),
                    t("Chiede un api_id tuo (my.telegram.org), il numero e il codice che Telegram manda; poi le chat arrivano da sole.", "It asks for an api_id of yours (my.telegram.org), the number and the code Telegram sends; then the chats come by themselves.").into(),
                ],
            )),
            State::Failed(e) => rows.push(note("exclamationmark.triangle.fill", th.red, vec![e.clone()])),
            State::Ready => {
                if !self.texts {
                    rows.push(note(
                        "lock.fill",
                        th.yellow,
                        vec![format!(
                            "{} {} {}",
                            t("Per leggere qui chi scrive e cosa, RustyBar deve poter leggere le notifiche di", "To read here who writes and what, RustyBar must be let read the notifications of"),
                            self.service.name(),
                            t("con Accesso completo al disco; poi riavvia la barra.", "with Full Disk Access; then restart the bar.")
                        )],
                    ));
                    rows.push(
                        rx.row_button(
                            vec![Node::text(t("Apri Accesso completo al disco…", "Open Full Disk Access…"), th.font(), th.text).key("text")],
                            vec![],
                            Action::Open("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles".into()),
                        )
                        .key("privacy"),
                    );
                }
                let waiting = self.waiting();
                for c in &waiting {
                    rows.push(self.chat_row(rx, c, now));
                }
                if waiting.is_empty() {
                    let mut lines = vec![t("Niente da leggere.", "Nothing to read.").to_string()];
                    if self.service.notified().is_some() {
                        lines.push(format!("{} {} {}", t("Qui arriva ciò che", "What"), self.service.name(), t("notifica su questo Mac.", "notifies on this Mac comes here.")));
                    }
                    rows.push(note("checkmark.circle.fill", self.service.color(), lines));
                }
                let mut others: Vec<&Chat> = self.chats.iter().filter(|c| self.unread(c) == 0 && self.held(c) == 0 && (self.groups || !c.group)).collect();
                others.sort_by(|a, b| b.at.total_cmp(&a.at));
                others.truncate(self.recent);
                if !others.is_empty() {
                    rows.push(rx.detail(t("Recenti", "Recent")).padding(6.0, 8.0, 2.0, 8.0).key("recent"));
                    for c in others {
                        rows.push(self.chat_row(rx, c, now));
                    }
                }
            }
        }
        rx.popup(WIDTH, rows)
    }

    /// A message as the chat shows it: a bubble at the left, or at the right if it's mine, with
    /// what it is inside and the time under it.
    fn bubble(&self, rx: &Rx, chat: &Chat, m: &Message, now: f64) -> Node {
        let th = rx.theme;
        let font = th.font();
        let ink = th.text;
        let mut inside: Vec<Node> = Vec::new();
        if chat.group && !m.mine && !m.sender.is_empty() {
            inside.push(Node::text(&m.sender, th.font_with(Weight::Semibold, th.small_size), self.service.color()).max_width(BUBBLE).key("sender"));
        }
        let text_lines = |text: &str, color: Rgba| -> Vec<Node> {
            text.lines().flat_map(|l| if l.trim().is_empty() { vec![String::new()] } else { wrap(l, &font, BUBBLE) }).enumerate().map(|(i, l)| Node::text(l, font.clone(), color).key(format!("t{i}"))).collect()
        };
        let labelled = |symbol: &str, text: String| Node::row(vec![Node::symbol(symbol, 13.0, Weight::Semibold, th.subtext).key("symbol"), Node::text(text, font.clone(), ink).max_width(BUBBLE - 24.0).key("label")]).gap(8.0).key("what");
        // What a click on the bubble does, and its tip
        let mut click: Option<(Action, String)> = None;
        let in_app = || (rx.action(&format!("show:{}", chat.jid)), format!("{} {}", t("Apri in", "Open in"), self.service.name()));
        // A file that isn't on the Mac: WhatsApp's opens in the app, Telegram's is downloaded
        let open = |file: &Option<String>| match file {
            Some(f) => (Action::Open(f.clone()), t("Apri", "Open").to_string()),
            None if self.service == Service::Telegram && self.fetching.contains(&m.id) => (rx.action(&format!("fetch:{}", m.id)), t("Lo scarico…", "Downloading…").to_string()),
            None if self.service == Service::Telegram => (rx.action(&format!("fetch:{}", m.id)), t("Scarica e apri", "Download and open").to_string()),
            None => in_app(),
        };
        match m.kind {
            Is::Text | Is::Other => inside.extend(text_lines(&m.text, ink)),
            Is::Link => {
                let url = if m.url.is_empty() { link(&m.text).unwrap_or_default().to_string() } else { m.url.clone() };
                let mut card = Vec::new();
                if let Some(thumb) = &m.thumb {
                    card.push(Node::image(thumb.as_str(), 40.0, 40.0, None).radius(6.0).key("thumb"));
                }
                let mut words = Vec::new();
                if !m.title.is_empty() {
                    words.push(Node::text(&m.title, th.font_with(Weight::Semibold, th.small_size), ink).max_width(BUBBLE - 60.0).key("title"));
                }
                words.push(Node::row(vec![Node::symbol("link", 9.0, Weight::Semibold, th.primary).key("symbol"), Node::text(host(&url), th.small_font(), th.primary).max_width(BUBBLE - 76.0).key("host")]).gap(4.0).key("where"));
                card.push(Node::column(words).gap(1.0).key("words"));
                inside.push(Node::row(card).gap(8.0).padding(6.0, 8.0, 6.0, 6.0).radius(8.0).background(th.base.faded(0.35)).min_width(BUBBLE).key("card"));
                inside.extend(text_lines(&m.text, ink));
                if !url.is_empty() {
                    click = Some((Action::Open(url.clone()), format!("{} {}", t("Apri", "Open"), host(&url))));
                }
            }
            Is::Image | Is::Gif | Is::Video => {
                let moving = m.kind != Is::Image;
                // The picture itself, or the small one the app has before it; a video's is its first frame
                let picture = if moving { m.thumb.as_ref() } else { m.file.as_ref().or(m.thumb.as_ref()) };
                match picture {
                    Some(p) => {
                        let mut frame = Node::default().key("picture").push(Node::image(p.as_str(), 200.0, 0.0, None).radius(8.0).key("image"));
                        if moving {
                            let label = if m.kind == Is::Gif { "GIF".to_string() } else { length(m.seconds) };
                            frame = frame.push(
                                Node::row(vec![Node::symbol("play.fill", 9.0, Weight::Bold, Rgba(1.0, 1.0, 1.0, 1.0)).key("play"), Node::text(label, th.small_font(), Rgba(1.0, 1.0, 1.0, 1.0)).key("length")])
                                    .gap(4.0)
                                    .padding(2.0, 7.0, 2.0, 7.0)
                                    .radius(9.0)
                                    .background(Rgba(0.0, 0.0, 0.0, 0.55))
                                    .at(6.0, 6.0)
                                    .key("badge"),
                            );
                        }
                        inside.push(frame);
                    }
                    None => inside.push(labelled(if moving { "video.fill" } else { "camera.fill" }, summary(m).1)),
                }
                inside.extend(text_lines(&m.text, ink));
                click = Some(open(&m.file));
            }
            Is::Sticker => match m.file.as_ref().filter(|f| !f.ends_with(".was")) {
                Some(f) => inside.push(Node::image(f.as_str(), 96.0, 0.0, None).key("sticker")),
                None => inside.push(labelled("face.smiling", "Sticker".into())),
            },
            Is::Voice => match &m.file {
                Some(_) => {
                    let playing = self.playing.as_ref().filter(|p| p.id == m.id);
                    let position = playing.map_or(0.0, |p| p.position(now));
                    let going = playing.is_some_and(|p| !p.paused);
                    let play = Node::row(vec![Node::symbol(if going { "pause.fill" } else { "play.fill" }, 12.0, Weight::Bold, self.service.ink()).key("symbol")])
                        .size(28.0, 28.0)
                        .radius(14.0)
                        .justify(Justify::Center)
                        .background(self.service.color())
                        .hover(self.service.color().mix(Rgba(1.0, 1.0, 1.0, 1.0), 0.2))
                        .click(rx.action(&format!("play:{}", m.id)))
                        .tip(if going { t("Pausa", "Pause") } else { t("Ascolta", "Play") })
                        .key("play");
                    let value = if m.seconds > 0.0 { (position / m.seconds).clamp(0.0, 1.0) } else { 0.0 };
                    let mut bar = Node::new(Kind::Slider { value, color: self.service.color(), track: th.base.faded(0.4), font: None, labels: vec![] }).height(8.0).width(120.0);
                    if playing.is_some() {
                        bar = bar.click(rx.action(&format!("seek:{}", m.id)));
                    }
                    let shown = if playing.is_some() { position } else { m.seconds };
                    inside.push(Node::row(vec![play, bar.key("bar"), Node::row(vec![Node::text(length(shown), th.small_font(), th.subtext).key("text")]).width(30.0).justify(Justify::End).key("length")]).gap(8.0).key("voice"));
                }
                None => {
                    inside.push(labelled("mic.fill", format!("{} · {}", t("Vocale", "Voice note"), length(m.seconds))));
                    click = Some(in_app());
                }
            },
            Is::File => {
                inside.push(labelled("doc.fill", summary(m).1));
                inside.extend(text_lines(&m.text, ink));
                click = Some(open(&m.file));
            }
            Is::Contact | Is::Location | Is::Poll => {
                let (symbol, text) = summary(m);
                inside.push(labelled(symbol.unwrap_or("ellipsis"), text));
                click = Some(in_app());
            }
            // WhatsApp keeps it off the Mac's disk: nothing to show but that it's there
            Is::Once => {
                inside.push(labelled("1.circle", summary(m).1));
                inside.push(Node::text(format!("{} {}", t("Si apre solo in", "It opens only in"), self.service.name()), th.small_font(), th.subtext).key("where"));
                click = Some(in_app());
            }
            Is::Deleted => inside.push(Node::row(vec![Node::symbol("nosign", 11.0, Weight::Semibold, th.subtext).key("symbol"), Node::text(summary(m).1, font.clone(), th.subtext).key("label")]).gap(6.0).key("what")),
        }
        // A text with a link opens it
        if click.is_none()
            && let Some(url) = link(&m.text)
        {
            inside.push(Node::row(vec![Node::symbol("link", 9.0, Weight::Semibold, th.primary).key("symbol"), Node::text(host(url), th.small_font(), th.primary).max_width(BUBBLE - 20.0).key("host")]).gap(4.0).key("link"));
            click = Some((Action::Open(url.to_string()), format!("{} {}", t("Apri", "Open"), host(url))));
        }
        inside.push(Node::row(vec![Node::spacer(), Node::text(clock(m.at), th.font_with(Weight::Medium, 9.0), th.subtext).key("at")]).key("time"));
        let color = if m.mine { self.service.color().faded(0.24) } else { th.surface };
        let mut bubble = Node::column(inside).gap(3.0).padding(6.0, 9.0, 4.0, 9.0).radius(12.0).background(color).key("bubble");
        if let Some((action, tip)) = click {
            bubble = bubble.click(action).hover(color.mix(th.text, 0.08)).tip(tip);
        }
        let row = if m.mine { vec![Node::spacer(), bubble] } else { vec![bubble, Node::spacer()] };
        Node::row(row).align(Align::Start).key(format!("m{}", m.id))
    }

    /// The popup's second page: a chat, its last messages and where to answer.
    fn conversation(&self, rx: &Rx, chat: &Chat) -> Node {
        let th = rx.theme;
        let now = time::now();
        let button = |symbol: &str, action: &str, tip: &str| {
            Node::row(vec![Node::symbol(symbol, 12.0, Weight::Semibold, th.subtext).key("symbol")])
                .size(26.0, 24.0)
                .justify(Justify::Center)
                .radius(6.0)
                .click(rx.action(action))
                .hover(th.highlight)
                .tip(tip)
                .key(action)
        };
        let mut head = vec![
            button("chevron.left", "back", t("Tutte le chat", "All chats")),
            avatar(th, chat, 26.0),
            Node::text(&chat.name, th.font_with(Weight::Semibold, th.font_size), th.text).max_width(WIDTH - 150.0).key("name"),
            Node::spacer(),
        ];
        // Slack and Teams read a conversation when it's opened there: the ↗ does it
        if chat.unread > 0 && self.service.notified().is_none() {
            let tip = match self.service {
                Service::WhatsApp => t("Segna come letta su WhatsApp: apre la chat nell'app", "Mark as read in WhatsApp: opens the chat in the app").to_string(),
                _ => t("Segna come letta su Telegram", "Mark as read in Telegram").to_string(),
            };
            head.push(button("checkmark.message", &format!("read:{}", chat.jid), &tip));
        }
        head.push(button("arrow.up.forward.app", &format!("show:{}", chat.jid), &format!("{} {}", t("Apri in", "Open in"), self.service.name())));
        let mut rows = vec![Node::row(head).gap(8.0).padding(4.0, 4.0, 4.0, 2.0).key("head"), rx.divider().key("divider")];

        let mut list = Vec::new();
        let mut last_day = String::new();
        for m in &chat.messages {
            let d = day(m.at, now);
            if d != last_day {
                list.push(Node::row(vec![Node::text(&d, th.font_with(Weight::Medium, 9.0), th.subtext).padding(2.0, 8.0, 2.0, 8.0).radius(8.0).background(th.surface.faded(0.6)).key("day")]).justify(Justify::Center).key(format!("day:{d}")));
                last_day = d;
            }
            list.push(self.bubble(rx, chat, m, now));
        }
        if list.is_empty() {
            list.push(Node::row(vec![Node::text(t("Nessun messaggio qui.", "No messages here."), th.small_font(), th.subtext).key("text")]).justify(Justify::Center).padding(12.0, 0.0, 12.0, 0.0).key("empty"));
        }
        // From the last message; a new one starts it from there again
        let start = chat.messages.last().map(|m| format!("m{}", m.id));
        let height = (rx.display.frame.size.height * 0.5).clamp(200.0, 420.0);
        rows.push(
            Node::new(Kind::Scroll { max_height: height, start })
                .width(WIDTH)
                .key(format!("messages:{}:{}", chat.jid, chat.newest))
                .push(Node::column(list).gap(4.0).padding(6.0, 6.0, 6.0, 6.0)),
        );

        if self.service.notified().is_some() {
            rows.push(
                rx.row_button(
                    vec![Node::symbol("arrowshape.turn.up.left.fill", 12.0, Weight::Semibold, self.service.color()).key("symbol"), Node::text(format!("{} {}", t("Rispondi in", "Answer in"), self.service.name()), th.font(), th.text).key("text")],
                    vec![],
                    rx.action(&format!("show:{}", chat.jid)),
                )
                .key("answer"),
            );
            return rx.popup(WIDTH, rows).gap(4.0);
        }
        let draft = self.drafts.get(&chat.jid).map(String::as_str).unwrap_or_default();
        let ready = !draft.trim().is_empty() && !self.sending;
        let placeholder = format!("{} {}", t("Scrivi a", "Message"), chat.name);
        let field = Node::input(draft, placeholder, th.font(), th.text, th.subtext).grow().click(rx.action("draft")).key("draft");
        let send = Node::row(vec![Node::symbol(if self.sending { "ellipsis" } else { "arrow.up" }, 12.0, Weight::Bold, self.service.ink()).key("symbol")])
            .size(26.0, 26.0)
            .radius(13.0)
            .justify(Justify::Center)
            .background(if ready { self.service.color() } else { self.service.color().faded(0.35) })
            .hover(if ready { self.service.color().mix(Rgba(1.0, 1.0, 1.0, 1.0), 0.2) } else { self.service.color().faded(0.35) })
            .click(rx.action("send"))
            .tip(match self.service {
                Service::WhatsApp => t("Invia con WhatsApp: l'app passa davanti un momento e apre la chat, che segna come letta", "Send with WhatsApp: the app comes to the front for a moment and opens the chat, which it marks read"),
                _ => t("Invia: la chat è segnata come letta", "Send: the chat is marked read"),
            })
            .key("send");
        rows.push(Node::row(vec![field, send]).gap(8.0).padding(5.0, 5.0, 5.0, 12.0).radius(18.0).background(th.surface).key("compose"));
        if let Some(e) = &self.send_error {
            let font = th.small_font();
            let lines: Vec<Node> = wrap(e, &font, WIDTH - 50.0).into_iter().enumerate().map(|(i, l)| Node::text(l, font.clone(), th.red).key(format!("l{i}"))).collect();
            rows.push(Node::row(vec![Node::symbol("exclamationmark.triangle.fill", 11.0, Weight::Semibold, th.red).key("symbol"), Node::column(lines).gap(1.0).key("text")]).gap(8.0).align(Align::Start).padding(2.0, 8.0, 2.0, 10.0).key("error"));
        }
        rx.popup(WIDTH, rows).gap(4.0)
    }

    /// Sends what's typed to the open chat, through the app.
    fn send(&mut self, cx: &mut Cx) {
        let Some(chat) = self.opened() else { return };
        let (jid, name, phone) = (chat.jid.clone(), chat.name.clone(), chat.phone().map(String::from));
        let chat_jid = jid.clone();
        let text = self.drafts.get(&jid).map(|d| d.trim().to_string()).unwrap_or_default();
        if text.is_empty() || self.sending {
            return;
        }
        self.send_error = None;
        // Made up: the message is in the chat, and nothing leaves the Mac
        if self.mock.is_some() {
            if let Some(c) = self.chats.iter_mut().find(|c| c.jid == jid) {
                let id = c.newest + 1;
                c.messages.push(Message { id, mine: true, at: time::now(), text, ..Default::default() });
                c.newest = id;
                c.at = time::now();
                c.unread = 0;
            }
            self.drafts.remove(&jid);
            self.share(cx);
            cx.redraw();
            return;
        }
        self.sending = true;
        cx.redraw();
        if let Some(telegram) = &self.telegram {
            let handle = cx.handle::<WhatsApp>();
            let done = Box::new(move |result: Result<(), String>| {
                handle.send(move |w, cx| {
                    w.sending = false;
                    match result {
                        Ok(()) => drop(w.drafts.remove(&jid)),
                        Err(e) => {
                            crate::log(&format!("telegram: not sent: {e}"));
                            w.send_error = Some(e);
                        }
                    }
                    cx.redraw();
                });
            });
            telegram.send(tg::Cmd::Send { jid: chat_jid, text, done });
            return;
        }
        cx.background(
            move || wa::send(&name, phone.as_deref(), &text),
            move |w: &mut WhatsApp, result, cx| {
                w.sending = false;
                // WhatsApp was in front for a moment and took the keyboard: back to the field,
                // for the next message
                cx.focus_input();
                match result {
                    Ok(()) => {
                        w.drafts.remove(&jid);
                        w.load(cx);
                    }
                    Err(e) => {
                        // Without the text, which is the user's
                        crate::log(&format!("whatsapp: not sent: {e}"));
                        w.send_error = Some(e);
                    }
                }
                cx.redraw();
            },
        );
    }

    /// Plays the voice note `id` of the open chat, pauses it or goes on; another stops the one
    /// that plays.
    fn play(&mut self, id: i64, cx: &mut Cx) {
        let now = time::now();
        if let Some(p) = self.playing.as_mut().filter(|p| p.id == id) {
            let position = p.position(now);
            (p.from, p.at, p.paused) = (position, now, !p.paused);
            if let Some(s) = &p.sound {
                if p.paused { s.pause() } else { s.play() }
            }
        } else {
            let Some(m) = self.opened().and_then(|c| c.messages.iter().find(|m| m.id == id)) else { return };
            let (Some(file), length) = (m.file.clone(), m.seconds) else { return };
            let sound = match &self.mock {
                Some(_) => None,
                None => match Sound::open(&file) {
                    Some(s) => {
                        s.play();
                        Some(s)
                    }
                    None => {
                        self.send_error = Some(t("macOS non riesce a riprodurre questo vocale.", "macOS can't play this voice note.").into());
                        return;
                    }
                },
            };
            let length = sound.as_ref().map_or(length, |s| s.length()).max(1.0);
            self.playing = Some(Playing { id, sound, from: 0.0, at: now, length, paused: false });
        }
        self.share(cx);
        cx.redraw();
        self.tick(id, cx);
    }

    /// While the voice note `id` plays in the open popup, the popup follows it; at its end it stops.
    fn tick(&mut self, id: i64, cx: &mut Cx) {
        cx.after(0.25, move |w: &mut WhatsApp, cx| {
            let now = time::now();
            let Some(p) = w.playing.as_ref().filter(|p| p.id == id) else { return };
            if p.paused {
                return;
            }
            let ended = match &p.sound {
                Some(s) => !s.playing(),
                None => p.position(now) >= p.length,
            };
            if ended || !cx.popup_open {
                w.playing = None;
                w.share(cx);
            } else {
                w.tick(id, cx);
            }
            cx.redraw();
        });
    }

    /// A message that comes with the made-up data: the next of `incoming`.
    fn mock_message(&mut self, cx: &mut Cx) {
        let Some(mock) = self.mock.as_mut() else { return };
        if mock.incoming.is_empty() {
            return;
        }
        let Incoming { jid, mut message } = mock.incoming.remove(0);
        let mut chats = self.chats.clone();
        let Some(c) = chats.iter_mut().find(|c| c.jid == jid) else { return };
        message.id = chats_newest(&self.chats) + 1;
        message.at = time::now();
        let c_newest = message.id;
        c.messages.push(message);
        c.newest = c_newest;
        c.unread += 1;
        c.at = time::now();
        self.loaded(chats, cx);
    }
}

fn chats_newest(chats: &[Chat]) -> i64 {
    chats.iter().map(|c| c.newest).max().unwrap_or(0)
}

impl Widget for WhatsApp {
    fn start(&mut self, cx: &mut Cx) {
        if let Some(mock) = &mut self.mock {
            // Made up: files beside the mock, and times as seconds ago
            let dir = std::env::var_os("RUSTYBAR_MOCK").map(std::path::PathBuf::from).unwrap_or_default();
            let now = time::now();
            let file = |path: &mut Option<String>| {
                if let Some(p) = path.as_mut().filter(|p| !p.starts_with('/')) {
                    *p = dir.join(&*p).to_string_lossy().into_owned();
                }
            };
            let mut message = |m: &mut Message| {
                m.at = now - m.at;
                file(&mut m.file);
                file(&mut m.thumb);
            };
            for c in &mut mock.chats {
                c.at = now - c.at;
                file(&mut c.picture);
                c.messages.iter_mut().for_each(&mut message);
            }
            mock.incoming.iter_mut().for_each(|i| message(&mut i.message));
            let chats = mock.chats.clone();
            self.loaded(chats, cx);
            return;
        }
        if self.service == Service::Telegram {
            // The connection tells the chats each time they change
            let handle = cx.handle::<WhatsApp>();
            self.telegram = Some(tg::start(self.recent, self.channels, move |chats| {
                handle.send(move |w, cx| match chats {
                    Ok(chats) => w.loaded(chats, cx),
                    Err(e) => {
                        let state = match e {
                            tg::Error::SignedOut => State::SignedOut,
                            tg::Error::Other(e) => State::Failed(e),
                        };
                        // What was read stays on show while the connection comes back
                        if w.state != state && (w.chats.is_empty() || state == State::SignedOut) {
                            w.state = state;
                            w.chats.clear();
                            w.share(cx);
                            cx.redraw();
                        }
                    }
                });
            }));
            return;
        }
        // Each write of the app, once it has been quiet for a moment: it writes in bursts
        let handle = cx.handle::<WhatsApp>();
        let ticket = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let changed = move || {
            let mine = ticket.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let (ticket, handle) = (ticket.clone(), handle.clone());
            crate::runtime::spawn("whatsapp-quiet", move || {
                std::thread::sleep(std::time::Duration::from_millis(400));
                if ticket.load(std::sync::atomic::Ordering::Relaxed) == mine && handle.alive() {
                    handle.send(|w, cx| w.load(cx));
                }
            });
        };
        self.watcher = match self.service.notified() {
            Some(_) => wa::watch_files(&nc::database(), nc::FILES, changed),
            None => wa::watch(&wa::container(), changed),
        };
        self.load(cx);
    }

    fn ready(&self) -> bool {
        self.state != State::Loading
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        // What's new, in green; with none of it, what was read here and WhatsApp still counts, dimmed
        let (n, color) = tally(th, self.service, self.total(), self.held_total());
        let blocked = self.state == State::Denied;
        if self.notch && n > 0 {
            // The notch has it: as the island, or in the island of another widget
            if let (Some(island), Some(chat)) = (rx.island, self.waiting().first()) {
                return Some(self.island_bar(rx, island, chat));
            }
            if rx.notch_island {
                return None;
            }
        }
        if n == 0 && !self.always && !blocked {
            return None;
        }
        let mut children = vec![glyph(self.service, th.icon_size + 2.0).opacity(if n > 0 && color != self.service.color() { 0.6 } else { 1.0 })];
        if self.playing.as_ref().is_some_and(|p| !p.paused) {
            children.push(listening(self.service));
        }
        if blocked {
            children.push(Node::text("!", th.font_with(Weight::Bold, th.font_size), th.yellow).key("blocked"));
        } else if n > 0 {
            children.push(Node::text(count(n), th.font_with(Weight::Bold, th.font_size), color).key("count"));
        }
        let pill = rx.pill(children).click(rx.toggle_popup());
        Some(if self.hover && n > 0 { pill.hover(Rgba(0.0, 0.0, 0.0, 0.0)) } else { pill })
    }

    /// Under the mouse: the chats that wait, three at most, each with its last message; a click
    /// on one opens it.
    fn peek(&self, rx: &Rx) -> Option<Node> {
        if !self.hover {
            return None;
        }
        let waiting = self.waiting();
        if waiting.is_empty() {
            return None;
        }
        let island = rx.island.filter(|_| self.notch);
        let theme = island.map(|_| notch_theme(rx.theme, self.service));
        let own = theme.as_ref().map(|theme| Rx { theme, ..*rx });
        let rx = own.as_ref().unwrap_or(rx);
        let th = rx.theme;
        let width = if island.is_some() { WIDTH - 28.0 } else { 250.0 };
        let mut rows = Vec::new();
        for chat in waiting.iter().take(3) {
            let top = Node::row(vec![
                Node::text(&chat.name, th.font_with(Weight::Semibold, th.font_size), th.text).max_width(width - 90.0).key("name"),
                Node::spacer(),
                {
                    let (n, color) = tally(th, self.service, self.unread(chat), self.held(chat));
                    Node::text(count(n), th.font_with(Weight::Bold, 10.0), color).key("n")
                },
            ])
            .key("top");
            let lines = Node::column(vec![top, self.preview_line(th, chat, width - 50.0, th.subtext)]).gap(1.0).grow().key("lines");
            rows.push(
                Node::row(vec![avatar(th, chat, 26.0), lines])
                    .gap(8.0)
                    .padding(4.0, 6.0, 4.0, 6.0)
                    .radius(th.row_radius)
                    .click(rx.action(&format!("chat:{}", chat.jid)))
                    .hover(th.highlight)
                    .key(format!("chat:{}", chat.jid)),
            );
        }
        if waiting.len() > 3 {
            rows.push(rx.detail(&format!("{} {}", waiting.len() - 3, t("altre chat", "more chats"))).key("more"));
        }
        let peek = Node::column(rows).gap(2.0).padding(2.0, 6.0, 8.0, 6.0);
        Some(if island.is_some() { peek.padding(4.0, 10.0, 10.0, 10.0).min_width(WIDTH) } else { peek.min_width(width) })
    }

    fn notice(&self, rx: &Rx) -> Option<Node> {
        let a = self.arrival.as_ref()?;
        let chat = self.chat(&a.jid)?;
        let m = chat.messages.iter().find(|m| m.id == a.id)?;
        // On the notch's black, as wide as the popup; or under the pill, in the theme's colors
        match rx.island.filter(|_| self.notch) {
            Some(_) => notice_node(&notch_theme(rx.theme, self.service), self.service, rx.name, chat, m, self.preview, WIDTH),
            None => notice_node(rx.theme, self.service, rx.name, chat, m, self.preview, 300.0).map(|n| n.padding(2.0, 12.0, 12.0, 12.0)),
        }
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        // On the island, on the notch's black
        let theme = rx.island.filter(|_| self.notch).map(|_| notch_theme(rx.theme, self.service));
        let own = theme.as_ref().map(|theme| Rx { theme, ..*rx });
        let rx = own.as_ref().unwrap_or(rx);
        Some(match self.opened() {
            Some(chat) => self.conversation(rx, chat),
            None => self.list(rx),
        })
    }

    fn input(&mut self, name: &str, text: &str, submit: bool, cx: &mut Cx) {
        let Some(jid) = self.open.clone().filter(|_| name == "draft") else { return };
        if !self.sending {
            self.drafts.insert(jid, text.to_string());
        }
        if submit {
            self.send(cx);
        }
    }

    fn action(&mut self, name: &str, value: Option<f64>, cx: &mut Cx) {
        match name.split_once(':') {
            Some(("chat", jid)) => {
                self.open = Some(jid.to_string());
                self.send_error = None;
                self.see(jid);
                self.show_chat(jid);
                self.share(cx);
                if !cx.popup_open {
                    self.keep = true;
                    cx.toggle_popup();
                }
            }
            Some(("play", id)) => {
                if let Ok(id) = id.parse() {
                    self.play(id, cx);
                }
            }
            Some(("seek", id)) => {
                let now = time::now();
                if let (Some(v), Some(p)) = (value, self.playing.as_mut().filter(|p| id.parse() == Ok(p.id))) {
                    let position = v.clamp(0.0, 1.0) * p.length;
                    (p.from, p.at) = (position, now);
                    if let Some(s) = &p.sound {
                        s.seek(position);
                    }
                }
            }
            // In the app: the chat with a person by its number; a group's has no link, so through
            // Accessibility, as a reply
            Some(("fetch", id)) => {
                let (Ok(id), Some(jid), Some(telegram)) = (id.parse::<i64>(), self.open.clone(), &self.telegram) else { return };
                if !self.fetching.insert(id) {
                    return;
                }
                let handle = cx.handle::<WhatsApp>();
                let done = Box::new(move |result: Result<String, String>| {
                    handle.send(move |w, cx| {
                        w.fetching.remove(&id);
                        match result {
                            Ok(file) => crate::runtime::open(&file),
                            Err(e) => w.send_error = Some(e),
                        }
                        cx.redraw();
                    });
                });
                telegram.send(tg::Cmd::Fetch { jid, id, done });
            }
            Some((verb @ ("show" | "read"), jid)) if self.service == Service::Telegram => {
                if self.mock.is_some() {
                    return;
                }
                if verb == "show" {
                    crate::runtime::open(&tg::link(jid));
                    cx.close_popup();
                } else if let Some(telegram) = &self.telegram {
                    let handle = cx.handle::<WhatsApp>();
                    let done = Box::new(move |result: Result<(), String>| {
                        if let Err(e) = result {
                            handle.send(move |w, cx| {
                                w.send_error = Some(e);
                                cx.redraw();
                            });
                        }
                    });
                    telegram.send(tg::Cmd::Read { jid: jid.to_string(), done });
                }
            }
            // Slack and Teams: the conversation where the notification leads, or just the app
            Some(("show" | "read", jid)) if self.service.notified().is_some() => {
                if self.mock.is_none() {
                    match self.chat(jid).and_then(|c| c.link.clone()) {
                        Some(link) => crate::runtime::open(&link),
                        None => crate::runtime::open_app(self.service.bundle()),
                    }
                }
                cx.close_popup();
            }
            Some((verb @ ("show" | "read"), jid)) => {
                if self.mock.is_some() {
                    return;
                }
                let Some(chat) = self.chat(jid) else { return };
                let (name, phone, front) = (chat.name.clone(), chat.phone().map(String::from), verb == "show");
                if front {
                    cx.close_popup();
                }
                cx.background(move || wa::show(&name, phone.as_deref(), front), |w: &mut WhatsApp, result: Result<(), String>, cx| {
                    if let Err(e) = result {
                        w.send_error = Some(e);
                        cx.redraw();
                    }
                });
            }
            _ => match name {
                "back" => {
                    self.open = None;
                    self.send_error = None;
                    self.playing = None;
                    self.share(cx);
                }
                "send" => self.send(cx),
                "app" => {
                    if self.mock.is_none() {
                        crate::runtime::open_app(self.service.bundle());
                    }
                    cx.close_popup();
                }
                _ => {}
            },
        }
        cx.redraw();
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        match event {
            Event::PopupOpened => {
                if !std::mem::take(&mut self.keep) {
                    self.open = None;
                }
                self.send_error = None;
            }
            Event::PopupClosed => {
                if let Some(telegram) = &self.telegram {
                    telegram.send(tg::Cmd::Close);
                }
                self.playing = None;
                self.share(cx);
            }
            Event::Wake => self.load(cx),
            Event::Trigger { name, .. } if name.strip_suffix("_message") == Some(self.service.key()) => self.mock_message(cx),
            // `msg trigger slack_dump`: what the app's notifications are made of, in a file
            Event::Trigger { name, .. } if self.service.notified().is_some() && self.mock.is_none() && name.strip_suffix("_dump") == Some(self.service.key()) => {
                let service = self.service;
                cx.background::<WhatsApp, _>(
                    move || {
                        let dir = crate::config::home().join("Library/Caches/rustybar");
                        let _ = std::fs::create_dir_all(&dir);
                        let path = dir.join(format!("notes-{}.txt", service.key()));
                        let text = nc::dump(&nc::database(), service.bundle());
                        let first = text.lines().next().unwrap_or_default().to_string();
                        let _ = std::fs::write(&path, text);
                        format!("{}: {first} in {}", service.key(), path.display())
                    },
                    |_, line, _| crate::log(&line),
                );
            }
            // The Dock's badges changed: read in the app, what waited doesn't any more
            Event::Trigger { name, .. } if name == "dock" && self.service.notified().is_some() => self.load(cx),
            // After `rustybar telegram login`, or a logout
            Event::Trigger { name, .. } if name == "telegram_login" && self.service == Service::Telegram => self.load(cx),
            // `msg trigger whatsapp_chat chat=<jid, or how its name starts>`: the popup on that
            // chat, or on the list without one
            Event::Trigger { name, args } if name.strip_suffix("_chat") == Some(self.service.key()) => {
                let jid = args.get("chat").and_then(|n| self.chats.iter().find(|c| c.jid == *n || c.name.starts_with(n.as_str()))).map(|c| c.jid.clone());
                match jid {
                    Some(jid) => self.action(&format!("chat:{jid}"), None, cx),
                    None if !cx.popup_open => cx.toggle_popup(),
                    None => self.open = None,
                }
                cx.redraw();
            }
            _ => {}
        }
    }

    crate::any_widget!();
}

// ---- voice notes -------------------------------------------------------------------------------

/// A sound file that plays: AVFoundation's player, which reads WhatsApp's voice notes (Opus in
/// Ogg). Its framework is loaded with the first one played, not with the bar.
struct Sound(Retained<AnyObject>);

impl Sound {
    fn open(path: &str) -> Option<Sound> {
        static LOADED: std::sync::Once = std::sync::Once::new();
        LOADED.call_once(|| unsafe {
            libc::dlopen(c"/System/Library/Frameworks/AVFAudio.framework/AVFAudio".as_ptr(), libc::RTLD_LAZY);
        });
        let class = AnyClass::get(c"AVAudioPlayer")?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        unsafe {
            let alloc: *mut AnyObject = msg_send![class, alloc];
            let player: *mut AnyObject = msg_send![alloc, initWithContentsOfURL: &*url, error: std::ptr::null_mut::<*mut AnyObject>()];
            let player = Retained::from_raw(player)?;
            let responds: bool = msg_send![&*player, respondsToSelector: sel!(prepareToPlay)];
            responds.then(|| Sound(player))
        }
    }

    fn play(&self) {
        let _: bool = unsafe { msg_send![&*self.0, play] };
    }

    fn pause(&self) {
        let _: () = unsafe { msg_send![&*self.0, pause] };
    }

    fn playing(&self) -> bool {
        unsafe { msg_send![&*self.0, isPlaying] }
    }

    fn position(&self) -> f64 {
        unsafe { msg_send![&*self.0, currentTime] }
    }

    fn length(&self) -> f64 {
        unsafe { msg_send![&*self.0, duration] }
    }

    fn seek(&self, secs: f64) {
        let _: () = unsafe { msg_send![&*self.0, setCurrentTime: secs] };
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        let _: () = unsafe { msg_send![&*self.0, stop] };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(jid: &str, unread: i64, newest: i64) -> Chat {
        let messages = (1..=newest).map(|id| Message { id, text: format!("m{id}"), ..Default::default() }).collect();
        Chat { jid: jid.into(), name: jid.into(), unread, newest, messages, at: newest as f64, ..Default::default() }
    }

    fn widget() -> WhatsApp {
        let mut w = WhatsApp::new(&Options::default(), Service::WhatsApp);
        w.mock = Some(Mock::default());
        w.seen.clear();
        w.state = State::Ready;
        w
    }

    #[test]
    fn an_entry_of_apps_is_a_bundle_or_the_name_of_an_app() {
        assert_eq!(bundle_of("com.example.chat").as_deref(), Some("com.example.chat"));
        assert_eq!(bundle_of("Calendar").as_deref(), Some("com.apple.iCal"));
        assert_eq!(bundle_of("No Such App Anywhere"), None);
        // Slack's and Teams' are the widgets made for them, any other app's its own, once
        assert_eq!(Service::of(crate::widgets::call::SLACK), Service::Slack);
        let chat = Service::of("com.example.chat");
        assert_eq!((chat.key(), chat.bundle(), chat.name()), ("app-com.example.chat", "com.example.chat", "chat"));
        assert_eq!(Service::of("com.example.chat"), chat);
        assert!(Service::all().contains(&chat) && chat.notified() == Some(nc::App::Other));
    }

    #[test]
    fn a_chat_read_here_waits_no_more_until_another_message() {
        let mut w = widget();
        w.chats = vec![chat("a", 2, 5), chat("b", 0, 3)];
        assert_eq!(w.total(), 2);
        w.see("a");
        // Nothing new, but WhatsApp still counts two: held, and still among those that wait
        assert_eq!((w.total(), w.held_total(), w.waiting().len()), (0, 2, 1));
        let th = Theme::from_config(&crate::config::ThemeConfig::default());
        assert_eq!(tally(&th, w.service, w.total(), w.held_total()), (2, th.subtext));
        // One more message: only that one waits, though WhatsApp counts three
        w.chats = vec![chat("a", 3, 6), chat("b", 0, 3)];
        assert_eq!((w.total(), w.held_total()), (1, 0));
        assert_eq!(tally(&th, w.service, w.total(), w.held_total()), (1, w.service.color()));
        // A chat with something new comes before one read here, though written to before it
        w.chats = vec![chat("a", 3, 6), chat("c", 1, 2)];
        w.see("a");
        assert_eq!(w.waiting().iter().map(|c| c.jid.as_str()).collect::<Vec<_>>(), ["c", "a"]);
        // Read in WhatsApp too: gone
        w.chats = vec![chat("a", 0, 6)];
        assert_eq!((w.total(), w.held_total(), w.waiting().len()), (0, 0, 0));
    }

    #[test]
    fn muted_chats_and_groups_count_only_if_asked() {
        let mut w = widget();
        let mut muted = chat("a", 4, 4);
        muted.muted = true;
        let mut group = chat("g", 1, 1);
        group.group = true;
        w.chats = vec![muted, group];
        assert_eq!(w.total(), 1);
        w.muted = true;
        assert_eq!(w.total(), 5);
        w.groups = false;
        assert_eq!(w.total(), 4);
    }

    #[test]
    fn the_unread_are_in_the_notch() {
        use objc2_core_foundation::{CGPoint, CGRect, CGSize};
        let theme = Theme::from_config(&crate::config::ThemeConfig::default());
        let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1512.0, 982.0));
        let display = crate::display::DisplayInfo { id: 1, name: "Test".into(), frame, scale: 2.0, notch: Some((646.0, 866.0)), notch_height: 38.0, large: false };
        let keys = |n: &Node| n.children.iter().filter_map(|c| c.key.clone()).collect::<Vec<_>>();
        let mut w = widget();
        w.chats = vec![chat("a", 2, 5), chat("b", 0, 3)];
        let island = Island { notch: 220.0, height: 38.0, ear: 6.0, radius: 10.0, left: 0.0, right: 0.0 };
        // The island itself: who wrote last and the count
        let mut rx = Rx::new("whatsapp", &theme, &display, crate::fit::Form::Full, false);
        rx.island = Some(island);
        let n = w.bar(&rx).expect("an island");
        assert!(n.style.notch.is_some());
        assert_eq!(keys(&n.children[0].children[0]), ["avatar", "name"]);
        assert_eq!(keys(&n.children[2].children[0]), ["icon", "count"]);
        assert!(w.popup(&rx).is_some() && w.peek(&rx).is_some());
        // Compact, the picture alone
        let mut compact = Rx::new("whatsapp", &theme, &display, crate::fit::Form::Compact, false);
        compact.island = Some(island);
        assert_eq!(keys(&w.bar(&compact).expect("an island").children[0].children[0]), ["avatar"]);
        // Beside the island of another widget, which has its sign: nothing; else a pill
        let mut beside = Rx::new("whatsapp", &theme, &display, crate::fit::Form::Full, false);
        beside.notch_island = true;
        assert!(w.bar(&beside).is_none());
        let plain = Rx::new("whatsapp", &theme, &display, crate::fit::Form::Full, false);
        assert_eq!(keys(&w.bar(&plain).expect("a pill")), ["icon", "count"]);
        // Read here, and WhatsApp still counts it: still in the notch, dimmed
        w.see("a");
        let dimmed = w.bar(&rx).expect("an island");
        assert_eq!(dimmed.children[0].children[0].children[0].style.opacity, 0.6);
        assert!(w.bar(&beside).is_none() && w.bar(&plain).is_some());
        // Nothing unread for WhatsApp either: no island, and no pill unless asked
        w.chats = vec![chat("a", 0, 5), chat("b", 0, 3)];
        assert!(w.bar(&rx).is_none() && w.bar(&plain).is_none() && w.peek(&plain).is_none());
        w.always = true;
        assert_eq!(keys(&w.bar(&plain).expect("a pill")), ["icon"]);
        // `notch = false`: a pill there too
        w.always = false;
        w.seen.clear();
        w.chats = vec![chat("a", 2, 5), chat("b", 0, 3)];
        w.notch = false;
        assert!(w.bar(&rx).expect("a pill").style.notch.is_none() && w.bar(&beside).is_some());
    }

    #[test]
    fn a_new_message_shows_for_a_moment_and_a_reply_goes_in_the_chat() {
        let mut timers = Vec::new();
        let mut cx = Cx { name: "whatsapp", generation: 0, popup_open: false, peek_open: false, timers: &mut timers, effects: Vec::new() };
        let noticed = |cx: &mut Cx| std::mem::take(&mut cx.effects).iter().any(|e| matches!(e, super::super::Effect::Notice));
        let mut w = widget();
        // What's there at the start is no news
        w.state = State::Loading;
        w.loaded(vec![chat("a", 2, 5), chat("b", 0, 3)], &mut cx);
        assert!(!noticed(&mut cx) && w.arrival.is_none() && w.newest == Some(5));
        // A message of mine, or one in a chat with nothing unread, neither
        let mut mine = chat("b", 0, 6);
        mine.messages.last_mut().unwrap().mine = true;
        w.loaded(vec![chat("a", 2, 5), mine.clone()], &mut cx);
        assert!(!noticed(&mut cx));
        // One that waits is
        w.loaded(vec![chat("a", 3, 7), mine.clone()], &mut cx);
        assert!(noticed(&mut cx));
        assert_eq!(w.arrival.as_ref().map(|a| (a.jid.as_str(), a.id)), Some(("a", 7)));
        // The same chats again: nothing
        w.loaded(vec![chat("a", 3, 7), mine], &mut cx);
        assert!(!noticed(&mut cx));
        // Opened here, it's read here; typed and sent (made up), the reply is its last message
        w.action("chat:a", None, &mut cx);
        assert_eq!((w.total(), w.open.as_deref()), (0, Some("a")));
        w.input("draft", "  on my way ", false, &mut cx);
        assert_eq!(w.chat("a").unwrap().messages.len(), 7);
        w.input("draft", "  on my way ", true, &mut cx);
        let a = w.chat("a").unwrap();
        assert_eq!(a.messages.last().map(|m| (m.mine, m.text.as_str())), Some((true, "on my way")));
        assert!(w.drafts.is_empty());
        // Nothing typed sends nothing
        w.input("draft", "   ", true, &mut cx);
        assert_eq!(w.chat("a").unwrap().messages.len(), 8);
    }

    #[test]
    fn links_and_times() {
        assert_eq!(link("look https://www.example.com/a?b now"), Some("https://www.example.com/a?b"));
        assert_eq!(link("no link here"), None);
        assert_eq!(host("https://www.example.com/a?b"), "example.com");
        assert_eq!(host("http://example.org"), "example.org");
        assert_eq!(length(74.4), "1:14");
        assert_eq!(count(120), "99+");
    }

    #[test]
    fn a_message_in_a_line() {
        let voice = Message { kind: Is::Voice, seconds: 14.0, ..Default::default() };
        assert_eq!(summary(&voice), (Some("mic.fill"), "0:14".into()));
        let photo = Message { kind: Is::Image, text: "the place\nsecond line".into(), ..Default::default() };
        assert_eq!(summary(&photo), (Some("camera.fill"), "the place".into()));
        let text = Message { text: "hello".into(), ..Default::default() };
        assert_eq!(summary(&text), (None, "hello".into()));
    }
}
