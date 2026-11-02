//! Today's next event from the calendars of the Mac: its start time, colored by how close it is
//! (yellow from 30 minutes, peach from 15, red from 5, the accent while it's on), then its title
//! with a countdown, or "until" while it's on. With no timed event left, the all-day ones; with
//! nothing left today, nothing. The popup lists all of today's events; a click on one joins its
//! Google Meet call or shows it in Calendar.
//!
//! EventKit runs on a thread of its own and is asked again only when the calendars change, on wake
//! and at midnight: the countdowns are worked out every minute from the events already read.
//! Options: `soon` (30), `sooner` (15), `imminent` (5), in minutes.

use std::ptr::NonNull;
use std::sync::mpsc::{Receiver, Sender, channel};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2_event_kit::{EKAuthorizationStatus, EKEntityType, EKEvent, EKEventStatus, EKEventStore, EKEventStoreChangedNotification, EKParticipantStatus};
use objc2_foundation::{NSCurrentLocaleDidChangeNotification, NSDate, NSDateFormatter, NSDateFormatterStyle, NSError, NSLocale, NSNotification, NSNotificationCenter, NSObjectProtocol};

use super::{Cx, Event, Handle, Rx, Widget};
use crate::config::Options;
use crate::gfx::{Rgba, Weight};
use crate::i18n::t;
use crate::runtime;
use crate::ui::Node;

const MEETINGBAR: &str = "/Applications/MeetingBar.app";

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
/// "start": 20, "end": 50, "meet": true}`, `{"title": "Holiday", "all_day": true}`.
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

    fn shown(&self) -> Shown {
        match self.access {
            Access::Waiting | Access::Asking => return Shown::Hidden,
            Access::Denied => return Shown::Denied,
            Access::Granted => {}
        }
        let now = now();
        let timed: Vec<&Ev> = self.events.iter().filter(|e| !e.all_day && e.end > now).collect();
        let next = timed.iter().find(|e| e.start > now);
        // The one that ends first, until it ends or the next one starts
        let ongoing = timed.iter().filter(|e| e.start <= now).min_by(|a, b| a.end.total_cmp(&b.end));
        if let Some(on) = ongoing
            && next.is_none_or(|n| on.end <= n.start)
        {
            let text = t(
                &format!("{} · fino alle {} (ancora {})", cut(&on.title, 30), self.time(on.end), left(minutes_to(on.end, now))),
                &format!("{} · until {} ({} left)", cut(&on.title, 30), self.time(on.end), left(minutes_to(on.end, now))),
            )
            .to_string();
            return Shown::Ongoing(self.time(on.start), text);
        }
        if let Some(n) = next {
            let m = minutes_to(n.start, now);
            let text = format!("{} · {}", cut(&n.title, 30), t(&format!("tra {}", left(m)), &format!("in {}", left(m))));
            return Shown::Upcoming(self.time(n.start), m, text);
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
}

impl Widget for Events {
    fn start(&mut self, cx: &mut Cx) {
        let (tx, rx) = channel();
        self.refresh = Some(tx.clone());
        let handle = cx.handle::<Events>();
        runtime::spawn("eventkit", move || eventkit(rx, tx, handle));
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
        let font = th.font();
        // All-day events first, then the others by start
        let mut order: Vec<usize> = (0..self.events.len()).collect();
        order.sort_by_key(|&i| !self.events[i].all_day);
        let hours = |e: &Ev| if e.all_day { t("Tutto il giorno", "All day").to_string() } else { format!("{} – {}", self.time(e.start), self.time(e.end)) };
        // The titles all start in the same column
        let column = order.iter().map(|&i| crate::gfx::measure_text(&hours(&self.events[i]), &font).width.ceil()).fold(0.0, f64::max);
        let any_meet = self.events.iter().any(|e| e.meet.is_some());
        let rows: Vec<Node> = order
            .iter()
            .map(|&i| {
                let e = &self.events[i];
                let (time_color, title_color) = if e.all_day {
                    (th.subtext, th.text)
                } else if e.end <= now {
                    (th.overlay, th.overlay)
                } else if e.start <= now {
                    (th.primary, th.text)
                } else {
                    (th.subtext, th.text)
                };
                let mut row = vec![
                    Node::row(vec![Node::text(hours(e), font.clone(), time_color)]).min_width(column).key("hours"),
                    Node::text(cut(&e.title, 40), font.clone(), title_color).key("title"),
                    Node::spacer(),
                ];
                if any_meet {
                    row.push(match &e.meet {
                        Some(_) if std::path::Path::new(MEETINGBAR).exists() => Node::image(format!("{MEETINGBAR}#google_meet_icon"), 16.0, 16.0, None).key("meet"),
                        Some(_) => Node::symbol("video.fill", 12.0, Weight::Semibold, th.green).key("meet"),
                        None => Node::default().size(16.0, 16.0).key("meet"),
                    });
                }
                Node::row(row).gap(16.0).height(24.0).padding(0.0, 8.0, 0.0, 8.0).radius(th.row_radius).click(rx.action(&format!("open:{i}"))).key(format!("e{i}"))
            })
            .collect();
        Some(rx.popup(220.0, rows).padding(4.0, 4.0, 4.0, 4.0).gap(0.0))
    }

    fn ready(&self) -> bool {
        self.access != Access::Waiting
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        if name == "calendar" {
            runtime::open_app("com.apple.iCal");
            return;
        }
        let Some(e) = name.strip_prefix("open:").and_then(|i| i.parse::<usize>().ok()).and_then(|i| self.events.get(i)) else { return };
        // As in a menu: the popup closes, then the link opens
        cx.close_popup();
        runtime::open(e.meet.as_deref().unwrap_or(&e.link));
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
fn eventkit(rx: Receiver<()>, tx: Sender<()>, handle: Handle<Events>) {
    let store = unsafe { EKEventStore::new() };
    let mock = crate::mock::load::<Vec<MockEv>>("events");
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
        let fields = [e.location().map(|s| s.to_string()), e.URL().and_then(|u| u.absoluteString()).map(|s| s.to_string()), e.notes().map(|s| s.to_string())];
        let id = e.eventIdentifier().map(|s| s.to_string()).unwrap_or_default();
        Ev {
            title: e.title().to_string(),
            start: e.startDate().timeIntervalSince1970(),
            end: e.endDate().timeIntervalSince1970(),
            all_day: e.isAllDay(),
            meet: fields.iter().flatten().find_map(|f| meet_link(f)),
            link: format!("ical://ekevent/{}?method=show&options=more", encode(&id)),
        }
    }
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
    }
}
