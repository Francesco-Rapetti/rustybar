//! The date and time, in the language and format of the Mac, and a calendar that scrolls through
//! the months without end: today highlighted, the week starting on the day of the region, the
//! weekends dimmer. A click on a day opens it in Calendar.

use objc2::rc::Retained;
use objc2_foundation::{NSCalendar, NSDate, NSDateFormatter, NSLocale, NSString, NSTimeZone};

use super::{Cx, Event, Rx, Widget};
use crate::config::Options;
use crate::gfx::Weight;
use crate::i18n::t;
use crate::runtime;
use crate::ui::{Align, Justify, Kind, Node};

/// Months kept around the visible ones; scrolling near an end adds more.
const STEP: i32 = 6;
const MAX_MONTHS: i32 = 60;
const CELL: f64 = 30.0;
const CELL_H: f64 = 26.0;
const GAP: f64 = 2.0;

pub struct Clock {
    date_fmt: Retained<NSDateFormatter>,
    time_fmt: Retained<NSDateFormatter>,
    month_fmt: Retained<NSDateFormatter>,
    date: String,
    time: String,
    /// The months in the calendar, as year * 12 + month - 1.
    first: i32,
    last: i32,
    /// Changes when "Today" is clicked, so the calendar scrolls back to today.
    reset: u32,
    first_weekday: u32,
    weekdays: Vec<String>,
    /// Only the latest chain of minute timers runs: a wake starts a new one.
    tick: u64,
}

fn today() -> (i32, u32, u32) {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        (tm.tm_year + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32)
    }
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(y) => 29,
        _ => 28,
    }
}

/// 1 = Sunday … 7 = Saturday, as NSCalendar counts.
pub fn weekday(y: i32, m: u32, d: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    let w = (y + y / 4 - y / 100 + y / 400 + T[m as usize - 1] + d as i32).rem_euclid(7);
    w as u32 + 1
}

fn month_index(y: i32, m: u32) -> i32 {
    y * 12 + m as i32 - 1
}

fn month_of(index: i32) -> (i32, u32) {
    (index.div_euclid(12), index.rem_euclid(12) as u32 + 1)
}

fn formatter(template: &str, tz: Option<&NSTimeZone>) -> Retained<NSDateFormatter> {
    let f = NSDateFormatter::new();
    f.setLocale(Some(&NSLocale::autoupdatingCurrentLocale()));
    if let Some(tz) = tz {
        f.setTimeZone(Some(tz));
    }
    f.setLocalizedDateFormatFromTemplate(&NSString::from_str(template));
    f
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

impl Clock {
    pub fn new(options: &Options) -> Clock {
        let tz = options.str("timezone").and_then(|n| NSTimeZone::timeZoneWithName(&NSString::from_str(n)));
        let date_fmt = match options.str("date") {
            Some(pattern) => {
                let f = formatter("", tz.as_deref());
                f.setDateFormat(Some(&NSString::from_str(pattern)));
                f
            }
            None => formatter("EEEddMM", tz.as_deref()),
        };
        // "j" is the hour in the region's 12 or 24 hour format
        let time_fmt = formatter("jmm", tz.as_deref());
        let month_fmt = formatter("LLLLy", tz.as_deref());
        let cal = NSCalendar::autoupdatingCurrentCalendar();
        let first_weekday = cal.firstWeekday() as u32;
        let symbols = month_fmt.veryShortStandaloneWeekdaySymbols();
        let weekdays = (0..7).map(|i| symbols.objectAtIndex((i + first_weekday as usize - 1) % 7).to_string()).collect();
        let (y, m, _) = today();
        let now = month_index(y, m);
        let mut c = Clock {
            date_fmt,
            time_fmt,
            month_fmt,
            date: String::new(),
            time: String::new(),
            first: now - 3,
            last: now + 8,
            reset: 0,
            first_weekday,
            weekdays,
            tick: 0,
        };
        c.tick();
        c
    }

    fn tick(&mut self) {
        let now = NSDate::now();
        self.date = self.date_fmt.stringFromDate(&now).to_string().replace('.', "");
        self.time = self.time_fmt.stringFromDate(&now).to_string();
    }

    /// Updates now and again at the start of the next minute.
    fn schedule(&mut self, cx: &mut Cx) {
        self.tick();
        cx.redraw();
        let secs = unsafe {
            let t = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&t, &mut tm);
            60 - tm.tm_sec
        };
        self.tick += 1;
        let tick = self.tick;
        cx.after::<Clock>(secs as f64 + 0.05, move |w, cx| {
            if w.tick == tick {
                w.schedule(cx);
            }
        });
    }

    fn month_title(&self, index: i32) -> String {
        let (y, m) = month_of(index);
        // The 15th, so no time zone moves it to another month
        let comps = format!("{y:04}-{m:02}-15T12:00:00Z");
        let iso = objc2_foundation::NSISO8601DateFormatter::new();
        match iso.dateFromString(&NSString::from_str(&comps)) {
            Some(date) => capitalize(&self.month_fmt.stringFromDate(&date).to_string()),
            None => format!("{m}/{y}"),
        }
    }

    fn month(&self, rx: &Rx, index: i32, today: (i32, u32, u32)) -> Node {
        let th = rx.theme;
        let (y, m) = month_of(index);
        let first = weekday(y, m, 1);
        let lead = (first + 7 - self.first_weekday) % 7;
        let mut cells: Vec<Node> = (0..lead).map(|i| Node::default().size(CELL, CELL_H).key(format!("pad{i}"))).collect();
        for d in 1..=days_in_month(y, m) {
            let wd = weekday(y, m, d);
            let weekend = wd == 1 || wd == 7;
            let is_today = (y, m, d) == today;
            let color = if is_today {
                th.on_primary
            } else if weekend {
                th.overlay
            } else {
                th.text
            };
            let mut cell = Node::row(vec![Node::text(d.to_string(), th.font_with(Weight::Bold, 12.0), color)])
                .key(format!("d{d}"))
                .size(CELL, CELL_H)
                .justify(Justify::Center)
                .radius(CELL_H / 2.0)
                .click(rx.action(&format!("day:{y}-{m}-{d}")));
            if is_today {
                cell = cell.background(th.primary);
            }
            cells.push(cell);
        }
        let title = Node::row(vec![Node::text(self.month_title(index), th.font(), th.text), Node::spacer()])
            .key("title")
            .pad(4.0, 6.0)
            .radius(th.row_radius)
            .click(rx.action("open"));
        Node::column(vec![title, Node::row(cells).columns(7).gap(GAP).key("grid")])
            .key(format!("m{index}"))
            .gap(4.0)
            .padding(4.0, 0.0, 8.0, 0.0)
    }
}

impl Widget for Clock {
    fn start(&mut self, cx: &mut Cx) {
        self.schedule(cx);
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let mut children = Vec::new();
        if !rx.compact {
            children.push(Node::text(&self.date, th.font(), th.text).key("date"));
        }
        children.push(Node::text(&self.time, th.font(), th.text).key("time"));
        Some(rx.pill(children).gap(10.0).click(rx.toggle_popup()))
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let today = today();
        let width = 7.0 * CELL + 6.0 * GAP;
        let header = Node::row(
            self.weekdays
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    let wd = (i as u32 + self.first_weekday - 1) % 7 + 1;
                    let color = if wd == 1 || wd == 7 { th.overlay } else { th.subtext };
                    Node::row(vec![Node::text(w, th.small_font(), color)]).width(CELL).justify(Justify::Center).key(format!("w{i}"))
                })
                .collect(),
        )
        .gap(GAP)
        .key("weekdays");
        let months: Vec<Node> = (self.first..=self.last).map(|i| self.month(rx, i, today)).collect();
        let (y, m, _) = today;
        let calendar = Node::new(Kind::Scroll { max_height: 2.0 * 6.0 * (CELL_H + GAP) + 60.0, start: Some(format!("m{}", month_index(y, m))) })
            .key(format!("months{}", self.reset))
            .width(width)
            .click(rx.action("edge"))
            .push(Node::column(months));
        let top = Node::row(vec![
            Node::text(&self.date, th.font(), th.text),
            Node::spacer(),
            Node::row(vec![Node::text(t("Oggi", "Today"), th.small_font(), th.primary)])
                .pad(3.0, 8.0)
                .radius(th.row_radius)
                .click(rx.action("today")),
        ])
        .padding(2.0, 0.0, 6.0, 6.0)
        .key("top");
        Some(
            Node::column(vec![top, header, calendar])
                .padding(10.0, 10.0, 6.0, 10.0)
                .gap(6.0)
                .align(Align::Stretch),
        )
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        match name {
            "top" => {
                self.first -= STEP;
                if self.last - self.first > MAX_MONTHS {
                    self.last = self.first + MAX_MONTHS;
                }
                cx.redraw();
            }
            "bottom" => {
                self.last += STEP;
                if self.last - self.first > MAX_MONTHS {
                    self.first = self.last - MAX_MONTHS;
                }
                cx.redraw();
            }
            "today" => {
                let (y, m, _) = today();
                let now = month_index(y, m);
                self.first = now - 3;
                self.last = now + 8;
                self.reset += 1;
                cx.redraw();
            }
            "open" => {
                cx.close_popup();
                runtime::open("file:///System/Applications/Calendar.app");
            }
            day if day.starts_with("day:") => {
                let parts: Vec<u32> = day[4..].split('-').filter_map(|p| p.parse().ok()).collect();
                if let [y, m, d] = parts[..] {
                    cx.close_popup();
                    // Locale-independent: set the parts of a date instead of parsing one
                    let script = format!(
                        "tell application \"Calendar\"\nactivate\nset d to current date\nset day of d to 1\nset year of d to {y}\nset month of d to {m}\nset day of d to {d}\nswitch view to day view\nview calendar at d\nend tell"
                    );
                    runtime::fire("/usr/bin/osascript", &["-e", &script]);
                }
            }
            _ => {}
        }
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        match event {
            Event::Wake => self.schedule(cx),
            Event::PopupOpened => {
                // Open on today, with the months around it
                let (y, m, _) = today();
                let now = month_index(y, m);
                if now < self.first + 1 || now > self.last - 1 || self.last - self.first > 24 {
                    self.first = now - 3;
                    self.last = now + 8;
                }
                self.reset += 1;
            }
            _ => {}
        }
    }

    crate::any_widget!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekdays_and_lengths() {
        assert_eq!(weekday(2026, 10, 30), 6); // a Friday
        assert_eq!(weekday(2024, 2, 29), 5); // a Thursday
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(month_of(month_index(2026, 1) - 1), (2025, 12));
    }
}
