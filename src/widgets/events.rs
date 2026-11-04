//! Today's next event from the calendars of the Mac: its start time, colored by how close it is
//! (yellow from 30 minutes, peach from 15, red from 5, the accent while it's on), then its title
//! with a countdown, or "until" while it's on. With no timed event left, the all-day ones; with
//! nothing left today, nothing.
//!
//! The popup is a dashboard. On top, two tiles wide, the event the bar shows, tinted with the color
//! of its countdown: when it is, the title, its calendar and place, how many guests accepted, how
//! much of it is gone while it's on, and Join for its Google Meet call; a click shows it in
//! Calendar. Then the all-day events as chips, and all of today's timed ones, each with the color
//! of its calendar, the past ones dimmed and a red line at the time it is now; a click on one joins
//! its call or shows it in Calendar. Open Calendar… at the bottom.
//!
//! EventKit runs on a thread of its own and is asked again only when the calendars change, on wake
//! and at midnight: the countdowns are worked out every minute from the events already read.
//! Options: `soon` (30), `sooner` (15), `imminent` (5), in minutes.

use std::ptr::NonNull;
use std::sync::mpsc::{Receiver, Sender, channel};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2_app_kit::{NSColor, NSColorSpace};
use objc2_event_kit::{EKAuthorizationStatus, EKEntityType, EKEvent, EKEventStatus, EKEventStore, EKEventStoreChangedNotification, EKParticipantStatus};
use objc2_foundation::{NSCurrentLocaleDidChangeNotification, NSDate, NSDateFormatter, NSDateFormatterStyle, NSError, NSLocale, NSNotification, NSNotificationCenter, NSObjectProtocol};

use super::{Cx, Event, Handle, Rx, Widget};
use crate::config::Options;
use crate::gfx::{Rgba, Weight};
use crate::i18n::t;
use crate::runtime;
use crate::ui::{Align, Justify, Kind, Node};

const MEETINGBAR: &str = "/Applications/MeetingBar.app";

/// The popup is as wide as two tiles of the other dashboards.
const WIDE: f64 = 160.0 * 2.0 + GAP;
const GAP: f64 = 8.0;
/// Inside the tile on top.
const INNER: f64 = WIDE - 24.0;
/// The column of the calendar's dot and the guests' icon, in the tile on top.
const LEAD: f64 = 14.0;
/// A row of the day: the start over the end, the title over the calendar and place.
const ROW: f64 = 38.0;

#[derive(Clone, Debug)]
struct Ev {
    title: String,
    /// Seconds since 1970.
    start: f64,
    end: f64,
    all_day: bool,
    meet: Option<String>,
    /// Shows the event in Calendar.
    link: String,
    /// The name and the color of its calendar.
    calendar: String,
    color: Option<Rgba>,
    /// Where, when it's a place and not a link.
    location: Option<String>,
    /// The guests, the user too, and how many of them accepted: none without guests.
    guests: Option<(usize, usize)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Access {
    /// Not read yet.
    Waiting,
    /// macOS is asking for access.
    Asking,
    Granted,
    Denied,
}

/// A made-up event (see `mock`), with its times in minutes from now: `{"title": "Design review",
/// "start": 20, "end": 50, "meet": true, "calendar": "Work", "color": "#1badf8", "location":
/// "Room 4B", "guests": 4, "accepted": 3}`, `{"title": "Holiday", "all_day": true}`.
#[derive(serde::Deserialize)]
struct MockEv {
    title: String,
    #[serde(default)]
    start: i64,
    #[serde(default)]
    end: i64,
    #[serde(default)]
    all_day: bool,
    #[serde(default)]
    meet: bool,
    #[serde(default)]
    calendar: String,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    guests: usize,
    #[serde(default)]
    accepted: usize,
}

/// What the bar shows.
enum Shown {
    Hidden,
    Denied,
    /// Start time, minutes to it, title.
    Upcoming(String, i64, String),
    /// Start time, title.
    Ongoing(String, String),
    AllDay(String),
}

pub struct Events {
    soon: i64,
    sooner: i64,
    imminent: i64,
    /// Made-up events, whose clicks do nothing.
    mock: bool,
    access: Access,
    events: Vec<Ev>,
    time_fmt: Retained<NSDateFormatter>,
    /// Asks the EventKit thread to read the events again; dropping it ends the thread.
    refresh: Option<Sender<()>>,
    /// The day the events are of, so the first minute of the next one reads them again.
    day: i32,
    /// Only the latest chain of minute timers runs: a wake starts a new one.
    tick: u64,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

fn now() -> f64 {
    NSDate::now().timeIntervalSince1970()
}

fn today() -> i32 {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm.tm_year * 1000 + tm.tm_yday
    }
}

/// Minutes to `t`, rounded up: at 9:29 an event at 9:30 is 1 min away.
fn minutes_to(t: f64, now: f64) -> i64 {
    ((t - now) / 60.0).ceil() as i64
}

/// e.g. 20 min, 1 h, 1 h 20 min
fn left(minutes: i64) -> String {
    let (h, m) = (minutes / 60, minutes % 60);
    match (h, m) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// On one line, cut with … after `len` characters.
fn cut(title: &str, len: usize) -> String {
    let one: String = title.chars().map(|c| if c == '\n' || c == '\r' || c == '\u{1f}' { ' ' } else { c }).collect();
    let one = one.trim_matches(|c| c == ' ' || c == '\t');
    if one.is_empty() {
        return t("Senza titolo", "Untitled").to_string();
    }
    if one.chars().count() > len {
        one.chars().take(len - 1).collect::<String>() + "…"
    } else {
        one.to_string()
    }
}

impl Events {
    pub fn new(options: &Options) -> Events {
        let time_fmt = NSDateFormatter::new();
        time_fmt.setLocale(Some(&NSLocale::autoupdatingCurrentLocale()));
        time_fmt.setDateStyle(NSDateFormatterStyle::NoStyle);
        time_fmt.setTimeStyle(NSDateFormatterStyle::ShortStyle);
        let minutes = |k: &str, d: i64| options.f64(k).map(|v| v as i64).unwrap_or(d);
        Events {
            soon: minutes("soon", 30),
            sooner: minutes("sooner", 15),
            imminent: minutes("imminent", 5),
            mock: false,
            access: Access::Waiting,
            events: Vec::new(),
            time_fmt,
            refresh: None,
            day: today(),
            tick: 0,
            observers: Vec::new(),
        }
    }

    /// The time in the region's format, 12 or 24 hours.
    fn time(&self, t: f64) -> String {
        self.time_fmt.stringFromDate(&NSDate::dateWithTimeIntervalSince1970(t)).to_string()
    }

    fn read(&self) {
        if let Some(tx) = &self.refresh {
            let _ = tx.send(());
        }
    }

    /// Redraws now and again at the start of the next minute, when events start and end and the
    /// countdowns change.
    fn schedule(&mut self, cx: &mut Cx) {
        self.tick += 1;
        let tick = self.tick;
        if today() != self.day {
            self.day = today();
            self.read();
        }
        cx.redraw();
        let secs = 60.0 - (now() % 60.0);
        cx.after::<Events>(secs + 0.05, move |w, cx| {
            if w.tick == tick {
                w.schedule(cx);
            }
        });
    }

    /// The event the bar shows, and whether it's on: of those going on the one that ends first,
    /// until it ends or the next one starts; else the next one.
    fn featured(&self, now: f64) -> Option<(usize, bool)> {
        let timed = || self.events.iter().enumerate().filter(|(_, e)| !e.all_day && e.end > now);
        let next = timed().find(|(_, e)| e.start > now);
        let ongoing = timed().filter(|(_, e)| e.start <= now).min_by(|(_, a), (_, b)| a.end.total_cmp(&b.end));
        if let Some((i, on)) = ongoing
            && next.is_none_or(|(_, n)| on.end <= n.start)
        {
            return Some((i, true));
        }
        next.map(|(i, _)| (i, false))
    }

    fn shown(&self) -> Shown {
        match self.access {
            Access::Waiting | Access::Asking => return Shown::Hidden,
            Access::Denied => return Shown::Denied,
            Access::Granted => {}
        }
        let now = now();
        match self.featured(now) {
            Some((i, true)) => {
                let on = &self.events[i];
                let text = t(
                    &format!("{} · fino alle {} (ancora {})", cut(&on.title, 30), self.time(on.end), left(minutes_to(on.end, now))),
                    &format!("{} · until {} ({} left)", cut(&on.title, 30), self.time(on.end), left(minutes_to(on.end, now))),
                )
                .to_string();
                return Shown::Ongoing(self.time(on.start), text);
            }
            Some((i, false)) => {
                let n = &self.events[i];
                let m = minutes_to(n.start, now);
                let text = format!("{} · {}", cut(&n.title, 30), t(&format!("tra {}", left(m)), &format!("in {}", left(m))));
                return Shown::Upcoming(self.time(n.start), m, text);
            }
            None => {}
        }
        let all_day: Vec<String> = self.events.iter().filter(|e| e.all_day).map(|e| cut(&e.title, 30)).collect();
        if all_day.is_empty() { Shown::Hidden } else { Shown::AllDay(all_day.join(" · ")) }
    }

    fn time_color(&self, rx: &Rx, minutes: i64) -> Rgba {
        let th = rx.theme;
        if minutes <= self.imminent {
            th.red
        } else if minutes <= self.sooner {
            th.peach
        } else if minutes <= self.soon {
            th.yellow
        } else {
            th.text
        }
    }

    // Drawing

    /// The event the bar shows, two tiles wide, in the color of its countdown.
    fn header(&self, rx: &Rx, i: usize, ongoing: bool, now: f64) -> Node {
        let th = rx.theme;
        let e = &self.events[i];
        let (when, color) = if ongoing {
            let l = left(minutes_to(e.end, now));
            (t(&format!("In corso · ancora {l}"), &format!("Now · {l} left")).to_string(), th.primary)
        } else {
            let m = minutes_to(e.start, now);
            let l = left(m);
            let color = self.time_color(rx, m);
            (t(&format!("Prossimo · tra {l}"), &format!("Next · in {l}")).to_string(), if color == th.text { th.subtext } else { color })
        };
        let lit = color != th.subtext;
        let hours = format!("{} – {}", self.time(e.start), self.time(e.end));
        let mut column = vec![
            Node::row(vec![
                Node::text(when, th.font_with(Weight::Bold, th.small_size), color).key("when"),
                Node::spacer(),
                Node::text(hours, th.small_font(), th.subtext).key("hours"),
            ])
            .gap(8.0)
            .width(INNER)
            .key("top"),
            Node::text(cut(&e.title, 100), th.font_with(Weight::Bold, 16.0), th.text).max_width(INNER).key("title"),
        ];
        if let Some(about) = about(e) {
            column.push(
                Node::row(vec![lead(dot(e.color.unwrap_or(th.primary), 8.0)).key("dot"), Node::text(about, th.small_font(), th.subtext).max_width(INNER - LEAD - 6.0).key("text")])
                    .gap(6.0)
                    .key("about"),
            );
        }
        if let Some((n, yes)) = e.guests {
            let accepted = t(&format!("{yes} di {n} {}", if yes == 1 { "ha accettato" } else { "hanno accettato" }), &format!("{yes} of {n} accepted")).to_string();
            column.push(
                Node::row(vec![
                    lead(Node::symbol("person.2.fill", 9.0, Weight::Semibold, th.subtext)).key("icon"),
                    Node::text(accepted, th.small_font(), th.subtext).key("text"),
                ])
                .gap(6.0)
                .key("guests"),
            );
        }
        if ongoing {
            let gone = ((now - e.start) / (e.end - e.start).max(1.0)).clamp(0.0, 1.0);
            let progress = Node::new(Kind::Progress { value: gone, color: th.primary, track: th.surface }).size(INNER, 4.0).key("progress");
            column.push(Node::row(vec![progress]).padding(6.0, 0.0, 0.0, 0.0).key("bar"));
        }
        if e.meet.is_some() {
            let join = Node::row(vec![meet_icon(rx, 14.0).key("icon"), Node::text(t("Partecipa a Google Meet", "Join Google Meet"), th.font_with(Weight::Bold, 12.0), th.text).key("label")])
                .gap(6.0)
                .justify(Justify::Center)
                .size(INNER, 30.0)
                .radius(15.0)
                .background(th.surface.faded(0.7))
                .click(rx.action(&format!("meet:{i}")))
                .key("join");
            column.push(Node::row(vec![join]).padding(8.0, 0.0, 0.0, 0.0).key("join-row"));
        }
        Node::column(column)
            .gap(4.0)
            .align(Align::Start)
            .padding(10.0, 12.0, 12.0, 12.0)
            .width(WIDE)
            .radius(10.0)
            .background(th.tile(color, lit))
            .click(rx.action(&format!("show:{i}")))
            .key("header")
    }

    /// The all-day events as chips with the color of their calendar, in rows as wide as the popup.
    fn chips(&self, rx: &Rx) -> Vec<Node> {
        let th = rx.theme;
        let font = th.font_with(Weight::Bold, 12.0);
        let mut rows = Vec::new();
        let mut line: Vec<Node> = Vec::new();
        let mut used = 0.0;
        for (i, e) in self.events.iter().enumerate().filter(|(_, e)| e.all_day) {
            let name = cut(&e.title, 100);
            // The padding, the dot and the gap
            let width = (crate::gfx::measure_text(&name, &font).width.ceil() + 22.0 + 8.0 + 6.0).min(WIDE);
            let chip = Node::row(vec![dot(e.color.unwrap_or(th.primary), 8.0).key("dot"), Node::text(name, font.clone(), th.text).max_width(WIDE - 36.0).key("name")])
                .gap(6.0)
                .padding(6.0, 12.0, 6.0, 10.0)
                .radius(14.0)
                .background(th.surface.faded(0.7))
                .click(rx.action(&format!("show:{i}")))
                .key(format!("day-{i}"));
            if !line.is_empty() && used + GAP + width > WIDE {
                rows.push(Node::row(std::mem::take(&mut line)).gap(GAP).width(WIDE).key(format!("chips-{}", rows.len())));
                used = 0.0;
            }
            used += if line.is_empty() { width } else { GAP + width };
            line.push(chip);
        }
        if !line.is_empty() {
            rows.push(Node::row(line).gap(GAP).width(WIDE).key(format!("chips-{}", rows.len())));
        }
        rows
    }

    /// Today's timed events, each with the color of its calendar, the past ones dimmed and a line
    /// at the time it is now, after the ones that started.
    fn day(&self, rx: &Rx, now: f64) -> Option<Node> {
        let th = rx.theme;
        let (font, small) = (th.font(), th.small_font());
        let timed: Vec<usize> = (0..self.events.len()).filter(|&i| !self.events[i].all_day).collect();
        if timed.is_empty() {
            return None;
        }
        // The titles all start in the same column
        let hours = timed.iter().map(|&i| crate::gfx::measure_text(&self.time(self.events[i].start), &font).width.ceil()).fold(0.0, f64::max);
        let any_meet = timed.iter().any(|&i| self.events[i].meet.is_some());
        // The padding, the bar, the hours and the gaps, and the call
        let names = WIDE - 16.0 - 3.0 - hours - 20.0 - if any_meet { 26.0 } else { 0.0 };
        let mut rows = Vec::new();
        let mut line = false;
        for &i in &timed {
            let e = &self.events[i];
            if !line && e.start > now {
                if !rows.is_empty() {
                    rows.push(self.now_line(rx, now));
                }
                line = true;
            }
            let past = e.end <= now;
            let color = e.color.unwrap_or(th.primary);
            let (bar, start, title, detail) = if past {
                (color.faded(0.35), th.overlay, th.overlay, th.overlay)
            } else if e.start <= now {
                (color, th.primary, th.text, th.subtext)
            } else {
                (color, th.text, th.text, th.subtext)
            };
            let mut text = vec![Node::text(cut(&e.title, 100), font.clone(), title).max_width(names).key("title")];
            if let Some(about) = about(e) {
                text.push(Node::text(about, small.clone(), detail).max_width(names).key("about"));
            }
            let mut row = vec![
                Node::default().size(3.0, 28.0).radius(1.5).background(bar).key("bar"),
                Node::column(vec![Node::text(self.time(e.start), font.clone(), start).key("start"), Node::text(self.time(e.end), small.clone(), detail).key("end")])
                    .gap(1.0)
                    .align(Align::Start)
                    .width(hours)
                    .key("hours"),
                Node::column(text).gap(1.0).align(Align::Start).key("text"),
                Node::spacer(),
            ];
            if any_meet {
                row.push(match &e.meet {
                    Some(_) => meet_icon(rx, 16.0).opacity(if past { 0.4 } else { 1.0 }).key("meet"),
                    None => Node::default().size(16.0, 16.0).key("meet"),
                });
            }
            rows.push(Node::row(row).gap(10.0).padding(0.0, 8.0, 0.0, 8.0).size(WIDE, ROW).radius(th.row_radius).click(rx.action(&format!("open:{i}"))).key(format!("e{i}")));
        }
        // Everything started already
        if !line {
            rows.push(self.now_line(rx, now));
        }
        Some(Node::column(rows).gap(2.0).key("day"))
    }

    /// A red dot on the bars of the events, a line and the time.
    fn now_line(&self, rx: &Rx, now: f64) -> Node {
        let th = rx.theme;
        Node::row(vec![
            dot(th.red, 7.0).key("dot"),
            Node::default().height(1.0).grow().background(th.red).key("line"),
            Node::text(self.time(now), th.small_font(), th.red).key("time"),
        ])
        .gap(4.0)
        .padding(0.0, 8.0, 0.0, 6.0)
        .size(WIDE, 14.0)
        .key("now")
    }
}

/// The calendar, then the place.
fn about(e: &Ev) -> Option<String> {
    let parts: Vec<&str> = [Some(e.calendar.as_str()).filter(|c| !c.is_empty()), e.location.as_deref()].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The calendar's dot and the guests' icon, in a column of their own before their lines.
fn lead(node: Node) -> Node {
    Node::row(vec![node]).width(LEAD).justify(Justify::Center)
}

fn dot(color: Rgba, size: f64) -> Node {
    Node::default().size(size, size).radius(size / 2.0).background(color)
}

/// MeetingBar's Google Meet logo, when it's there, else a green camera.
fn meet_icon(rx: &Rx, size: f64) -> Node {
    if std::path::Path::new(MEETINGBAR).exists() {
        Node::image(format!("{MEETINGBAR}#google_meet_icon"), size, size, None)
    } else {
        Node::row(vec![Node::symbol("video.fill", size * 0.75, Weight::Semibold, rx.theme.green)]).size(size, size).justify(Justify::Center)
    }
}

impl Widget for Events {
    fn start(&mut self, cx: &mut Cx) {
        let (tx, rx) = channel();
        self.refresh = Some(tx.clone());
        let handle = cx.handle::<Events>();
        let mock = crate::mock::load::<Vec<MockEv>>("events");
        self.mock = mock.is_some();
        runtime::spawn("eventkit", move || eventkit(rx, tx, handle, mock));
        // The calendars changed (edits, syncs), or the language or the region
        let center = NSNotificationCenter::defaultCenter();
        for name in [unsafe { EKEventStoreChangedNotification }, unsafe { NSCurrentLocaleDidChangeNotification }] {
            let h = cx.handle::<Events>();
            let block = RcBlock::new(move |_: NonNull<NSNotification>| h.send(|w: &mut Events, _| w.read()));
            self.observers.push(unsafe { center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block) });
        }
        self.read();
        self.schedule(cx);
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let (color, time, title, title_color) = match self.shown() {
            Shown::Hidden => return None,
            Shown::Denied => (th.red, None, t("Nessun accesso al calendario", "No access to calendars").to_string(), th.red),
            Shown::Upcoming(time, m, title) => (self.time_color(rx, m), Some(time), title, th.text),
            Shown::Ongoing(time, title) => (th.primary, Some(time), title, th.text),
            Shown::AllDay(title) => (th.text, None, title, th.text),
        };
        let mut children = vec![Node::symbol("calendar", th.icon_size, Weight::Semibold, color).key("icon")];
        if let Some(time) = time {
            children.push(Node::text(time, th.font(), color).key("time"));
        }
        // Compact, only the time
        if !rx.compact || children.len() == 1 {
            children.push(Node::text(title, th.font(), title_color).key("title"));
        }
        // Without events (no access, say) the click opens Calendar
        let click = if self.events.is_empty() { rx.action("calendar") } else { rx.toggle_popup() };
        Some(rx.pill(children).click(click))
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        if self.events.is_empty() {
            return None;
        }
        let th = rx.theme;
        let now = now();
        let mut rows = Vec::new();
        if let Some((i, ongoing)) = self.featured(now) {
            rows.push(self.header(rx, i, ongoing, now));
        }
        rows.extend(self.chips(rx));
        rows.extend(self.day(rx, now));
        rows.push(rx.row_button(vec![Node::text(t("Apri Calendario…", "Open Calendar…"), th.font(), th.primary)], vec![], rx.action("calendar")).key("calendar"));
        Some(rx.popup(WIDE, rows).gap(GAP).padding(8.0, 8.0, 6.0, 8.0))
    }

    fn ready(&self) -> bool {
        self.access != Access::Waiting
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        // As in a menu: the popup closes, then the link opens
        cx.close_popup();
        if self.mock {
            return;
        }
        if name == "calendar" {
            runtime::open_app("com.apple.iCal");
            return;
        }
        let Some((what, i)) = name.split_once(':') else { return };
        let Some(e) = i.parse::<usize>().ok().and_then(|i| self.events.get(i)) else { return };
        // A row joins the call it has, the tile on top shows the event and has Join for that
        let link = match what {
            "open" => e.meet.as_deref().unwrap_or(&e.link),
            "meet" => match &e.meet {
                Some(m) => m,
                None => return,
            },
            _ => &e.link,
        };
        runtime::open(link);
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        if let Event::Wake = event {
            self.read();
            self.schedule(cx);
        }
    }

    crate::any_widget!();
}

impl Drop for Events {
    fn drop(&mut self) {
        let center = NSNotificationCenter::defaultCenter();
        for o in self.observers.drain(..) {
            unsafe { center.removeObserver(o.as_ref()) };
        }
    }
}

/// The EventKit thread: asks for access once, then reads today's events whenever asked, until
/// the widget goes.
fn eventkit(rx: Receiver<()>, tx: Sender<()>, handle: Handle<Events>, mock: Option<Vec<MockEv>>) {
    let store = unsafe { EKEventStore::new() };
    let status = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
    if mock.is_none() && status == EKAuthorizationStatus::NotDetermined {
        // The prompt shows the first time; the answer reads the events again
        let block = RcBlock::new(move |_: Bool, _: *mut NSError| {
            let _ = tx.send(());
        });
        let ptr = &*block as *const _ as *mut _;
        unsafe {
            if objc2::msg_send![&*store, respondsToSelector: objc2::sel!(requestFullAccessToEventsWithCompletion:)] {
                store.requestFullAccessToEventsWithCompletion(ptr);
            } else {
                #[allow(deprecated)]
                store.requestAccessToEntityType_completion(EKEntityType::Event, ptr);
            }
        }
    } else {
        drop(tx);
    }
    let mut last = Access::Waiting;
    while rx.recv().is_ok() {
        // Many changes at once read the events once
        while rx.try_recv().is_ok() {}
        if !handle.alive() {
            break;
        }
        let (access, events) = match &mock {
            Some(mock) => (Access::Granted, mock_events(mock)),
            None => read_events(&store),
        };
        if access != last && mock.is_none() {
            crate::log(&format!("calendars: {}", match access {
                Access::Asking => "asking for access",
                Access::Granted => "access granted",
                _ => "no access (System Settings → Privacy & Security → Calendars)",
            }));
            last = access;
        }
        handle.send(move |w, cx| {
            w.access = access;
            w.events = events;
            cx.redraw();
        });
    }
}

fn read_events(store: &EKEventStore) -> (Access, Vec<Ev>) {
    let status = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
    match status {
        EKAuthorizationStatus::FullAccess => {}
        EKAuthorizationStatus::NotDetermined => return (Access::Asking, Vec::new()),
        _ => return (Access::Denied, Vec::new()),
    }
    // From midnight to midnight: also the events that started yesterday and still go on
    let (midnight, next) = today_bounds();
    let found = unsafe {
        let predicate = store.predicateForEventsWithStartDate_endDate_calendars(
            &NSDate::dateWithTimeIntervalSince1970(midnight),
            &NSDate::dateWithTimeIntervalSince1970(next),
            None,
        );
        store.eventsMatchingPredicate(&predicate)
    };
    let mut events: Vec<Ev> = found.iter().filter(|e| keep(e)).map(|e| convert(&e)).collect();
    events.sort_by(|a, b| a.start.total_cmp(&b.start).then_with(|| a.title.cmp(&b.title)));
    (Access::Granted, events)
}

/// Today's midnight and the next one, in seconds since 1970.
fn today_bounds() -> (f64, f64) {
    let now = now();
    let midnight = {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let t = now as libc::time_t;
        unsafe { libc::localtime_r(&t, &mut tm) };
        tm.tm_hour = 0;
        tm.tm_min = 0;
        tm.tm_sec = 0;
        tm.tm_isdst = -1;
        unsafe { libc::mktime(&mut tm) as f64 }
    };
    let next = {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let t = midnight as libc::time_t;
        unsafe { libc::localtime_r(&t, &mut tm) };
        tm.tm_mday += 1;
        tm.tm_isdst = -1;
        unsafe { libc::mktime(&mut tm) as f64 }
    };
    (midnight, next)
}

fn mock_events(mock: &[MockEv]) -> Vec<Ev> {
    let minute = (now() / 60.0).floor() * 60.0;
    let (midnight, next) = today_bounds();
    mock.iter()
        .enumerate()
        .map(|(i, m)| Ev {
            title: m.title.clone(),
            start: if m.all_day { midnight } else { minute + m.start as f64 * 60.0 },
            end: if m.all_day { next } else { minute + m.end as f64 * 60.0 },
            all_day: m.all_day,
            meet: m.meet.then(|| format!("https://meet.google.com/abc-defg-hi{i}")),
            link: "ical://".into(),
            calendar: m.calendar.clone(),
            color: m.color.as_deref().and_then(Rgba::parse),
            location: m.location.clone(),
            guests: (m.guests > 1).then_some((m.guests, m.accepted.min(m.guests))),
        })
        .collect()
}

/// Not cancelled, nor declined.
fn keep(e: &EKEvent) -> bool {
    unsafe {
        if e.status() == EKEventStatus::Canceled {
            return false;
        }
        !e.attendees().is_some_and(|a| a.iter().any(|p| p.isCurrentUser() && p.participantStatus() == EKParticipantStatus::Declined))
    }
}

fn convert(e: &EKEvent) -> Ev {
    unsafe {
        let location = e.location().map(|s| s.to_string());
        let fields = [location.clone(), e.URL().and_then(|u| u.absoluteString()).map(|s| s.to_string()), e.notes().map(|s| s.to_string())];
        let id = e.eventIdentifier().map(|s| s.to_string()).unwrap_or_default();
        let calendar = e.calendar();
        let guests = e.attendees().map(|a| (a.count(), a.iter().filter(|p| p.participantStatus() == EKParticipantStatus::Accepted).count())).filter(|(n, _)| *n > 1);
        Ev {
            title: e.title().to_string(),
            start: e.startDate().timeIntervalSince1970(),
            end: e.endDate().timeIntervalSince1970(),
            all_day: e.isAllDay(),
            meet: fields.iter().flatten().find_map(|f| meet_link(f)),
            link: format!("ical://ekevent/{}?method=show&options=more", encode(&id)),
            calendar: calendar.as_ref().map(|c| c.title().to_string()).unwrap_or_default(),
            color: calendar.as_ref().and_then(|c| srgb(&c.color())),
            location: location.as_deref().and_then(place),
            guests,
        }
    }
}

/// The color in sRGB.
fn srgb(color: &NSColor) -> Option<Rgba> {
    let c = color.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    Some(Rgba(c.redComponent(), c.greenComponent(), c.blueComponent(), c.alphaComponent()))
}

/// A location that is a place, on one line: not the link of a call.
fn place(location: &str) -> Option<String> {
    let one = cut(location, 200);
    let link = one.contains("://") || one.to_ascii_lowercase().contains("meet.google.com");
    (!location.trim().is_empty() && !link).then_some(one)
}

/// The Google Meet address in a text, without what follows it (`?authuser=…`). Google Calendar
/// puts it in the notes, other apps in the location or the URL.
fn meet_link(text: &str) -> Option<String> {
    const PREFIX: &str = "https://meet.google.com/";
    let lower = text.to_ascii_lowercase();
    let at = lower.find(PREFIX)?;
    let rest = &text[at + PREFIX.len()..];
    let len = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '/' || c == '-')).unwrap_or(rest.len());
    (len > 0).then(|| format!("{PREFIX}{}", &rest[..len]))
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b':' | b'@' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdowns_and_links() {
        assert_eq!(left(20), "20 min");
        assert_eq!(left(60), "1 h");
        assert_eq!(left(80), "1 h 20 min");
        assert_eq!(minutes_to(60.0, 1.0), 1);
        assert_eq!(meet_link("Join: https://meet.google.com/abc-defg-hij?authuser=1").as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        assert_eq!(meet_link("http://meet.google.com/abc"), None);
        assert_eq!(encode("A1/b:c@d e"), "A1%2Fb:c@d%20e");
        assert_eq!(cut("a\nb", 30), "a b");
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(place(" Room 4B\n").as_deref(), Some("Room 4B"));
        assert_eq!(place("https://meet.google.com/abc-defg-hij"), None);
        assert_eq!(place("  "), None);
    }
}
