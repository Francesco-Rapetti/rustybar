//! Two counts at the edge of the bar. Green: the Dock badges of the apps in `apps` (Slack, Teams…)
//! and the crashes of RustyBar not seen yet. Red: the Homebrew packages with an update, and
//! AeroSpace when it runs an older version than the one installed. The popup lists them; a click
//! opens the app, the crash report, or updates the package (or all of them).
//!
//! The badges come from the Dock through Accessibility, every 2 seconds, since nothing tells when
//! they change; every badged app, listed or not, gets a dot on its windows in `aerospace`. Without
//! the permission a yellow `!` shows instead, and the popup has a row that asks for it again (macOS
//! asks by itself only once).
//! A ✕ on a badge or the crashes dismisses them without opening anything, as `Dismiss All` does
//! for all of them: a badge comes back when it goes up (Slack's dot that becomes a number too),
//! the crashes with a new one.
//! Homebrew is asked every hour and on wake. Options: `apps` (bundle ids or Dock names),
//! `brewfile` (the packages to follow) or `brew = true` (all of them).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSRunningApplication;
use objc2_foundation::{NSArray, NSBundle, NSDictionary, NSString, NSURL, NSUserDefaults, ns_string};
use serde::Deserialize;

use super::{Cx, Event, Rx, Widget};
use crate::config::Options;
use crate::gfx::Weight;
use crate::i18n::t;
use crate::iokit::{CFTypeRef, cf, owned};
use crate::runtime;
use crate::ui::{Justify, Node};

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Badge {
    bundle: String,
    /// As the Dock calls the app.
    name: String,
    /// `3`, `99+`, or Slack's dot for unread channels.
    label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Package {
    cask: bool,
    name: String,
    installed: String,
    new: String,
}

impl Package {
    /// `felixkratz/formulae/borders` → `borders`
    fn short(&self) -> &str {
        self.name.rsplit('/').next().unwrap_or(&self.name)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Outdated {
    packages: Vec<Package>,
    /// The version AeroSpace should restart into.
    restart: Option<String>,
}

/// An update or a restart under way: one at a time.
#[derive(Clone, Debug, PartialEq)]
enum Job {
    Restart,
    /// The names of the packages.
    Upgrade(Vec<String>),
}

/// Made-up data (see `mock`).
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    badges: Vec<Badge>,
    crashes: Vec<f64>,
    #[serde(flatten)]
    outdated: Outdated,
}

pub struct Notifications {
    apps: Vec<String>,
    brew: Option<Brew>,
    /// None: no Accessibility permission.
    badges: Option<Vec<Badge>>,
    /// The badges dismissed from the popup, by bundle, with the label they have now: hidden until
    /// it goes up, forgotten when the badge goes away. In the defaults, so that a reload or a
    /// restart doesn't bring them back (not with the made-up data).
    dismissed: HashMap<String, String>,
    /// The badged apps last told to the workspaces.
    told: Option<Vec<String>>,
    /// The times of the crash reports not seen yet, newest last.
    crashes: Vec<f64>,
    outdated: Outdated,
    job: Option<Job>,
    checking: bool,
    mock: Option<Mock>,
}

/// Which packages to follow.
#[derive(Clone)]
enum Brew {
    All,
    File(PathBuf),
}

/// The number of a badge: its digits (`99+` is 99), none for one without (a dot).
fn level(label: &str) -> Option<u64> {
    let digits: String = label.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// The count of a badge: its number, or 1 for a dot.
fn count(label: &str) -> u64 {
    level(label).unwrap_or(1)
}

/// A dismissed badge that's back: it went up since (a dot is less than any number).
fn went_up(now: &str, dismissed: &str) -> bool {
    level(now) > level(dismissed)
}

fn defaults() -> Option<Retained<NSUserDefaults>> {
    NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(ns_string!("rustybar")))
}

/// The defaults key of the dismissed badges: bundle → label.
const DISMISSED: &str = "badges.dismissed";

fn load_dismissed() -> HashMap<String, String> {
    defaults().and_then(|d| d.dictionaryForKey(ns_string!(DISMISSED))).map(|d| from_dictionary(&d)).unwrap_or_default()
}

fn to_dictionary(map: &HashMap<String, String>) -> Retained<NSDictionary<NSString, AnyObject>> {
    let keys: Vec<Retained<NSString>> = map.keys().map(|k| NSString::from_str(k)).collect();
    let values: Vec<Retained<NSString>> = map.values().map(|v| NSString::from_str(v)).collect();
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<&AnyObject> = values.iter().map(|v| &**v as &AnyObject).collect();
    NSDictionary::from_slices(&keys, &values)
}

/// Strings to strings: anything else in the defaults is left out.
fn from_dictionary(dictionary: &NSDictionary<NSString, AnyObject>) -> HashMap<String, String> {
    let (keys, values) = dictionary.to_vecs();
    keys.iter().zip(values).filter_map(|(k, v)| Some((k.to_string(), v.downcast::<NSString>().ok()?.to_string()))).collect()
}

fn now() -> f64 {
    objc2_foundation::NSDate::now().timeIntervalSince1970()
}

impl Notifications {
    pub fn new(options: &Options) -> Notifications {
        let brew = match (options.str("brewfile"), options.bool("brew")) {
            (Some(path), _) => Some(Brew::File(PathBuf::from(path.replacen('~', &crate::config::home().display().to_string(), 1)))),
            (None, Some(true)) => Some(Brew::All),
            _ => None,
        };
        let mock = crate::mock::load::<Mock>("notifications");
        Notifications {
            apps: options.strings("apps"),
            brew,
            badges: mock.as_ref().map(|m| m.badges.clone()),
            dismissed: if mock.is_some() { HashMap::new() } else { load_dismissed() },
            told: None,
            crashes: mock.as_ref().map(|m| m.crashes.clone()).unwrap_or_default(),
            outdated: mock.as_ref().map(|m| m.outdated.clone()).unwrap_or_default(),
            job: None,
            checking: false,
            mock,
        }
    }

    /// The listed apps with a badge not dismissed, in the order of `apps`.
    fn app_badges(&self) -> Vec<&Badge> {
        let Some(badges) = &self.badges else { return Vec::new() };
        self.apps.iter().flat_map(|app| badges.iter().filter(move |b| (b.bundle == *app || b.name == *app) && self.shown(b))).collect()
    }

    fn shown(&self, badge: &Badge) -> bool {
        self.dismissed.get(&badge.bundle).is_none_or(|label| went_up(&badge.label, label))
    }

    /// Hides a badge until it goes up.
    fn dismiss(&mut self, bundle: &str) {
        let Some(badge) = self.badges.iter().flatten().find(|b| b.bundle == bundle) else { return };
        self.dismissed.insert(badge.bundle.clone(), badge.label.clone());
    }

    /// The crashes as seen, as when one is opened.
    fn dismiss_crashes(&mut self) {
        if self.mock.is_none()
            && let Some(d) = defaults()
        {
            d.setDouble_forKey(now(), ns_string!("crashes.seen"));
        }
        self.crashes.clear();
    }

    /// The dismissed badges after the Dock changed: one that went up is shown again, one that went
    /// down stays dismissed at its new label (so that it comes back if it goes up again), one that
    /// went away is forgotten.
    fn settle(&mut self) {
        let Some(badges) = &self.badges else { return };
        let before = self.dismissed.clone();
        self.dismissed.retain(|bundle, label| match badges.iter().find(|b| b.bundle == *bundle) {
            Some(b) if !went_up(&b.label, label) => {
                label.clone_from(&b.label);
                true
            }
            _ => false,
        });
        if self.dismissed != before {
            self.save_dismissed();
        }
    }

    fn save_dismissed(&self) {
        if self.mock.is_some() {
            return;
        }
        let Some(d) = defaults() else { return };
        if self.dismissed.is_empty() {
            d.removeObjectForKey(ns_string!(DISMISSED));
            return;
        }
        unsafe { d.setObject_forKey(Some(&to_dictionary(&self.dismissed)), ns_string!(DISMISSED)) };
    }

    /// After a dismissal: the workspaces lose the dots of the dismissed apps, and a popup with
    /// nothing left closes before the badges leave the bar.
    fn dismissed_some(&mut self, cx: &mut Cx) {
        self.save_dismissed();
        self.tell_workspaces();
        if !self.needs_access() && self.unread() == 0 && self.updates() == 0 {
            cx.close_popup();
        }
        cx.redraw();
    }

    fn unread(&self) -> u64 {
        self.crashes.len() as u64 + self.app_badges().iter().map(|b| count(&b.label)).sum::<u64>()
    }

    /// The listed apps can't be read: no Accessibility permission.
    fn needs_access(&self) -> bool {
        self.mock.is_none() && !self.apps.is_empty() && self.badges.is_none()
    }

    fn updates(&self) -> usize {
        self.outdated.packages.len() + self.outdated.restart.is_some() as usize
    }

    /// The Dock and the crash reports, read every 2 seconds on a thread of their own, which wakes
    /// the widget only when something changed. The reports are listed again only when their
    /// folder changes.
    fn watch(&mut self, cx: &mut Cx) {
        let handle = cx.handle::<Notifications>();
        runtime::spawn("dock", move || {
            let mut last: Option<(Option<Vec<Badge>>, Vec<f64>)> = None;
            let mut listed = None;
            let mut crashes = Vec::new();
            let defaults = defaults();
            while handle.alive() {
                let key = (reports_modified(), seen(defaults.as_deref()).to_bits());
                if listed != Some(key) {
                    listed = Some(key);
                    crashes = crash_reports(f64::from_bits(key.1));
                }
                let now = (dock_badges(), crashes.clone());
                if last.as_ref() != Some(&now) {
                    last = Some(now.clone());
                    handle.send(move |w, cx| {
                        (w.badges, w.crashes) = now;
                        w.settle();
                        w.tell_workspaces();
                        cx.redraw();
                    });
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    }

    /// The badged apps, for the dots on their windows: not those dismissed.
    fn tell_workspaces(&mut self) {
        let bundles: Vec<String> = self.badges.iter().flatten().filter(|b| self.shown(b)).map(|b| b.bundle.clone()).collect();
        if self.told.as_ref() == Some(&bundles) {
            return;
        }
        self.told = Some(bundles.clone());
        let args = HashMap::from([("bundles".to_string(), bundles.join(","))]);
        runtime::after(0.0, move |app| app.broadcast(&Event::Trigger { name: "badges".into(), args }));
    }

    /// Asks Homebrew; `update` first refreshes its list of versions, which `brew upgrade` then
    /// installs, exactly the ones the popup shows.
    fn check(&mut self, update: bool, cx: &mut Cx) {
        let Some(brew) = self.brew.clone() else { return };
        if self.checking || self.job.is_some() || self.mock.is_some() {
            return;
        }
        self.checking = true;
        cx.background::<Notifications, Option<Outdated>>(
            move || outdated(&brew, update),
            |w, outdated, cx| {
                w.checking = false;
                if let Some(o) = outdated
                    && o != w.outdated
                {
                    w.outdated = o;
                    cx.redraw();
                }
            },
        );
    }

    fn start_job(&mut self, job: Job, cx: &mut Cx) {
        if self.job.is_some() || self.mock.is_some() {
            return;
        }
        self.job = Some(job.clone());
        cx.redraw();
        let packages = self.outdated.packages.clone();
        cx.background::<Notifications, ()>(
            move || match job {
                Job::Restart => restart_aerospace(),
                Job::Upgrade(names) => upgrade(packages.into_iter().filter(|p| names.contains(&p.name)).collect()),
            },
            |w, _, cx| {
                w.job = None;
                w.check(false, cx);
                cx.redraw();
            },
        );
    }
}

impl Widget for Notifications {
    fn start(&mut self, cx: &mut Cx) {
        if self.mock.is_some() {
            self.tell_workspaces();
            return;
        }
        if !self.apps.is_empty() && !ax_trusted() {
            ask_accessibility();
        }
        self.watch(cx);
        self.check(true, cx);
        cx.every::<Notifications>(3600.0, |w, cx| w.check(true, cx));
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let crust = th.color("crust").unwrap_or(th.base);
        let badge = |child: Node, color, key: &str| {
            Node::row(vec![child])
                .height(18.0)
                .min_width(18.0)
                .padding(0.0, 6.0, 0.0, 6.0)
                .justify(Justify::Center)
                .radius(9.0)
                .background(color)
                .key(key)
        };
        let count = |n: String| Node::text(n, th.font_with(Weight::Bold, 11.0), crust);
        let mut badges = Vec::new();
        if self.needs_access() {
            badges.push(badge(Node::symbol("exclamationmark", 10.0, Weight::Bold, crust), th.yellow, "access"));
        }
        if self.unread() > 0 {
            badges.push(badge(count(self.unread().to_string()), th.green, "unread"));
        }
        if self.updates() > 0 {
            badges.push(badge(count(self.updates().to_string()), if self.job.is_some() { th.yellow } else { th.red }, "updates"));
        }
        if badges.is_empty() {
            return None;
        }
        // No pill, as for apple: the hover highlight is a capsule as tall as one, around the badges
        // as far as above and below them, so it's concentric with them (a circle around just one)
        let ring = ((th.pill_height - 18.0) / 2.0).max(0.0);
        Some(
            Node::row(badges)
                .gap(4.0)
                .height(th.pill_height)
                .padding(0.0, ring, 0.0, ring)
                .radius(th.pill_height / 2.0)
                .key(rx.name)
                .click(rx.toggle_popup()),
        )
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let bold = th.font();
        let busy = self.job.is_some();
        let measure = |s: &str| crate::gfx::measure_text(s, &bold).width;
        let mut rows = vec![Node::default().height(4.0).key("top")];

        // Apps and crashes: an icon, a name, a value in a column, a ✕ that dismisses them. A click
        // anywhere else on the row opens them.
        struct Item {
            bundle: String,
            name: String,
            value: String,
            open: String,
            dismiss: String,
        }
        let mut apps: Vec<Item> = Vec::new();
        if let Some(&last) = self.crashes.last() {
            let n = self.crashes.len();
            let name = if n == 1 { t("Crash di RustyBar", "RustyBar Crashed").to_string() } else { t(&format!("{n} crash di RustyBar"), &format!("RustyBar Crashed {n} Times")).to_string() };
            apps.push(Item { bundle: "com.apple.Console".into(), name, value: when(last), open: "crash".into(), dismiss: "dismiss-crash".into() });
        }
        for b in self.app_badges() {
            apps.push(Item { bundle: b.bundle.clone(), name: b.name.clone(), value: b.label.clone(), open: format!("app:{}", b.bundle), dismiss: format!("dismiss:{}", b.bundle) });
        }
        let access = self.needs_access();
        let for_badges = t("per i badge del Dock", "for Dock badges");
        let packages = &self.outdated.packages;
        let updating = t("aggiornamento…", "updating…");
        let values = apps.iter().map(|a| measure(&a.value)).chain(packages.iter().map(|p| measure(&versions(p)))).chain((!packages.is_empty()).then(|| measure(updating))).chain(access.then(|| measure(for_badges)));
        let value_width = values.fold(0.0, f64::max);
        let value = |text: String, color| Node::row(vec![Node::text(text, bold.clone(), color)]).min_width(value_width).key("value");
        // The ✕ inside the row's corner, concentric with it; the same room on the rows without
        // one, so that the values stay in a column
        const CLOSE: f64 = 20.0;
        let inset = (24.0 - CLOSE) / 2.0;
        let close = |action: Option<&str>| match action {
            Some(action) => Node::row(vec![Node::symbol("xmark", 9.0, Weight::Bold, th.subtext)])
                .size(CLOSE, CLOSE)
                .justify(Justify::Center)
                .radius((th.row_radius - inset).max(0.0))
                .click(rx.action(action))
                .key("close"),
            None => Node::default().size(CLOSE, CLOSE).key("close"),
        };
        let app_row = |icon: Node, name: &str, v: String, open: &str, dismiss: Option<&str>| {
            Node::row(vec![icon.key("icon"), Node::text(name, bold.clone(), th.text).key("name"), Node::spacer(), value(v, th.subtext), close(dismiss)])
                .gap(8.0)
                .padding(0.0, inset, 0.0, 8.0)
                .height(24.0)
                .radius(th.row_radius)
                .click(rx.action(open))
                .key(open)
        };
        let listed = access || !apps.is_empty();
        if access {
            let icon = Node::row(vec![Node::symbol("accessibility", 14.0, Weight::Semibold, th.yellow)]).width(16.0).justify(Justify::Center);
            rows.push(app_row(icon, t("Concedi l'Accessibilità", "Allow Accessibility"), for_badges.into(), "accessibility", None));
        }
        for a in &apps {
            rows.push(app_row(Node::app_icon(&a.bundle, 16.0), &a.name, a.value.clone(), &a.open, Some(&a.dismiss)));
        }
        if apps.len() > 1 {
            rows.push(
                Node::row(vec![Node::text(t("Ignora tutte", "Dismiss All"), bold.clone(), th.primary)])
                    .padding(0.0, 8.0, 0.0, 8.0)
                    .height(24.0)
                    .radius(th.row_radius)
                    .click(rx.action("dismiss-all"))
                    .key("dismiss-all"),
            );
        }
        if listed && self.updates() > 0 {
            rows.push(Node::default().height(8.0).key("gap"));
        }
        // The packages' values in the same column as those above, which have the ✕ after them
        let trail = if listed { inset + CLOSE + 8.0 } else { 8.0 };

        // Homebrew: dimmed while a job runs, whose rows say so
        let dim = |color| if busy { th.overlay } else { color };
        let action_row = |text: &str, color, key: &str| {
            let mut row = Node::row(vec![Node::text(text, bold.clone(), color)]).padding(0.0, 8.0, 0.0, 8.0).height(24.0).radius(th.row_radius).key(key);
            if !busy {
                row = row.click(rx.action(key));
            }
            row
        };
        if self.outdated.restart.is_some() {
            rows.push(match self.job {
                Some(Job::Restart) => action_row(t("Riavvio di AeroSpace…", "Restarting AeroSpace…"), th.yellow, "restart"),
                _ => action_row(t("Riavvia AeroSpace", "Restart AeroSpace"), dim(th.primary), "restart"),
            });
        }
        for p in packages {
            let upgrading = matches!(&self.job, Some(Job::Upgrade(names)) if names.contains(&p.name));
            let v = if upgrading { value(updating.into(), th.yellow) } else { value(versions(p), dim(th.subtext)) };
            let key = format!("upgrade:{}", p.name);
            let mut row = Node::row(vec![Node::text(p.short(), bold.clone(), dim(th.text)).key("name"), Node::spacer(), v])
                .gap(16.0)
                .padding(0.0, trail, 0.0, 8.0)
                .height(24.0)
                .radius(th.row_radius)
                .key(key.clone());
            if !busy {
                row = row.click(rx.action(&key));
            }
            rows.push(row);
        }
        if packages.len() > 1 {
            rows.push(action_row(t("Aggiorna tutto", "Update All"), dim(th.primary), "upgrade-all"));
        }
        if rows.len() == 1 {
            return None;
        }
        rows.push(Node::default().height(4.0).key("bottom"));
        Some(rx.popup(0.0, rows).padding(0.0, 4.0, 0.0, 4.0).gap(0.0))
    }

    fn ready(&self) -> bool {
        self.mock.is_some() || self.badges.is_some() || self.apps.is_empty() || !ax_trusted()
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        match name {
            "crash" => {
                cx.close_popup();
                // The newest report, in Console
                if self.mock.is_none()
                    && let Some(path) = newest_report()
                {
                    runtime::fire("/usr/bin/open", &["-a", "Console", &path]);
                }
                self.dismiss_crashes();
                cx.redraw();
            }
            "dismiss-crash" => {
                self.dismiss_crashes();
                self.dismissed_some(cx);
            }
            "dismiss-all" => {
                self.dismiss_crashes();
                let bundles: Vec<String> = self.app_badges().iter().map(|b| b.bundle.clone()).collect();
                for bundle in bundles {
                    self.dismiss(&bundle);
                }
                self.dismissed_some(cx);
            }
            "accessibility" if self.mock.is_none() => {
                cx.close_popup();
                prompt_accessibility();
                runtime::open("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
            }
            "restart" => self.start_job(Job::Restart, cx),
            "upgrade-all" => {
                let names = self.outdated.packages.iter().map(|p| p.name.clone()).collect();
                self.start_job(Job::Upgrade(names), cx);
            }
            _ => {
                if let Some(bundle) = name.strip_prefix("app:") {
                    cx.close_popup();
                    if self.mock.is_none() {
                        runtime::open_app(bundle);
                    }
                } else if let Some(bundle) = name.strip_prefix("dismiss:") {
                    self.dismiss(bundle);
                    self.dismissed_some(cx);
                } else if let Some(package) = name.strip_prefix("upgrade:") {
                    self.start_job(Job::Upgrade(vec![package.to_string()]), cx);
                }
            }
        }
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        match event {
            Event::Wake => self.check(true, cx),
            // After an update from a terminal: `rustybar msg trigger brew_update`
            Event::Trigger { name, .. } if name == "brew_update" => self.check(false, cx),
            _ => {}
        }
    }

    crate::any_widget!();
}

/// `1.8.4 → 1.9.0`, without the build casks add after a comma.
fn versions(p: &Package) -> String {
    let short = |v: &str| v.split(',').next().unwrap_or(v).to_string();
    format!("{} → {}", short(&p.installed), short(&p.new))
}

/// The time of a crash today, else its day, as the Mac writes them.
fn when(t: f64) -> String {
    let (then, today) = (crate::time::local(t), crate::time::local(now()));
    let template = if (then.tm_year, then.tm_yday) == (today.tm_year, today.tm_yday) { "jmm" } else { "dM" };
    super::clock::formatter(template, None).stringFromDate(&objc2_foundation::NSDate::dateWithTimeIntervalSince1970(t)).to_string()
}

// The Dock, through Accessibility

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: CFTypeRef) -> u8;
    fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    fn AXUIElementCopyAttributeValue(element: CFTypeRef, attribute: CFTypeRef, value: *mut CFTypeRef) -> i32;
    fn AXUIElementSetMessagingTimeout(element: CFTypeRef, seconds: f32) -> i32;
}

fn ax_trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

/// macOS's own dialog that leads to the setting, once.
fn ask_accessibility() {
    let Some(d) = defaults() else { return };
    if d.boolForKey(ns_string!("accessibility.asked")) {
        crate::log("notifications: the Dock badges need Accessibility (System Settings → Privacy & Security → Accessibility, or the popup of notifications)");
        return;
    }
    d.setBool_forKey(true, ns_string!("accessibility.asked"));
    prompt_accessibility();
}

/// macOS's dialog, which also puts RustyBar in the list of the setting, where it only has to be
/// switched on.
fn prompt_accessibility() {
    let options = objc2_foundation::NSDictionary::from_slices(&[ns_string!("AXTrustedCheckOptionPrompt")], &[&*objc2_foundation::NSNumber::new_bool(true) as &AnyObject]);
    unsafe { AXIsProcessTrustedWithOptions(cf(&*options)) };
}

fn attribute(element: CFTypeRef, name: &NSString) -> Option<Retained<AnyObject>> {
    let mut value: CFTypeRef = std::ptr::null();
    if unsafe { AXUIElementCopyAttributeValue(element, cf(name), &mut value) } != 0 {
        return None;
    }
    unsafe { owned(value) }
}

/// The badges of the Dock, in its order; None without the permission. The Dock is the only one
/// that shows all of them, also those that iOS apps (WhatsApp) set through their notifications.
fn dock_badges() -> Option<Vec<Badge>> {
    if !ax_trusted() {
        return None;
    }
    let dock = NSRunningApplication::runningApplicationsWithBundleIdentifier(ns_string!("com.apple.dock")).firstObject();
    let Some(dock) = dock else { return Some(Vec::new()) };
    let app: Retained<AnyObject> = unsafe { owned(AXUIElementCreateApplication(dock.processIdentifier())) }?;
    unsafe { AXUIElementSetMessagingTimeout(cf(&*app), 1.0) };
    let children = |e: &AnyObject| attribute(cf(e), ns_string!("AXChildren")).and_then(|c| c.downcast::<NSArray>().ok());
    let mut badges = Vec::new();
    for list in children(&app).iter().flat_map(|l| l.iter()) {
        for item in children(&list).iter().flat_map(|l| l.iter()) {
            let Some(label) = attribute(cf(&*item), ns_string!("AXStatusLabel")).and_then(|l| l.downcast::<NSString>().ok()).map(|l| l.to_string()).filter(|l| !l.is_empty()) else { continue };
            // Separators, folders and the trash have no app
            let Some(url) = attribute(cf(&*item), ns_string!("AXURL")).and_then(|u| u.downcast::<NSURL>().ok()) else { continue };
            let Some(bundle) = NSBundle::bundleWithURL(&url).and_then(|b| b.bundleIdentifier()).map(|b| b.to_string()) else { continue };
            let name = attribute(cf(&*item), ns_string!("AXTitle")).and_then(|t| t.downcast::<NSString>().ok()).map(|t| t.to_string()).unwrap_or_else(|| bundle.clone());
            badges.push(Badge { bundle, name, label });
        }
    }
    Some(badges)
}

// Crashes: the reports macOS writes, newer than the last one seen

fn reports() -> Vec<(f64, PathBuf)> {
    let dir = crate::config::home().join("Library/Logs/DiagnosticReports");
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with("rustybar") && n.ends_with(".ips")))
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64();
            Some((modified, e.path()))
        })
        .collect()
}

fn reports_modified() -> Option<std::time::SystemTime> {
    std::fs::metadata(crate::config::home().join("Library/Logs/DiagnosticReports")).and_then(|m| m.modified()).ok()
}

/// The time of the last crash report seen: from the first start on, not those from before.
fn seen(defaults: Option<&NSUserDefaults>) -> f64 {
    let Some(d) = defaults else { return now() };
    match d.doubleForKey(ns_string!("crashes.seen")) {
        0.0 => {
            d.setDouble_forKey(now(), ns_string!("crashes.seen"));
            now()
        }
        seen => seen,
    }
}

fn crash_reports(seen: f64) -> Vec<f64> {
    let mut times: Vec<f64> = reports().into_iter().map(|r| r.0).filter(|&t| t > seen).collect();
    times.sort_by(f64::total_cmp);
    times
}

fn newest_report() -> Option<String> {
    reports().into_iter().max_by(|a, b| a.0.total_cmp(&b.0)).map(|r| r.1.display().to_string())
}

// Homebrew

/// brew through `env`: no auto-update, so that `brew upgrade` installs the versions shown, and
/// no upgrade of the packages that depend on the chosen ones.
fn brew(args: &[&str], timeout: Duration) -> runtime::Output {
    let brew = runtime::which("brew").unwrap_or_else(|| "brew".into());
    let mut full = vec!["HOMEBREW_NO_AUTO_UPDATE=1", "HOMEBREW_NO_ENV_HINTS=1", "HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK=1", brew.as_str()];
    full.extend_from_slice(args);
    runtime::run("/usr/bin/env", &full, None, timeout)
}

/// The packages of a Brewfile: its `brew "…"` and `cask "…"` lines.
fn brewfile(path: &PathBuf) -> (Vec<String>, Vec<String>) {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let name = |line: &str, prefix: &str| line.strip_prefix(prefix).and_then(|r| r.split('"').next()).map(String::from);
    let formulae = text.lines().filter_map(|l| name(l, "brew \"")).collect();
    let casks = text.lines().filter_map(|l| name(l, "cask \"")).collect();
    (formulae, casks)
}

/// `name (1.0, 1.1) < 1.2` for formulae, `!=` for casks: the last version installed.
fn parse_outdated(line: &str, cask: bool) -> Option<Package> {
    let (name, rest) = line.split_once(" (")?;
    let (installed, rest) = rest.split_once(") ")?;
    let new = rest.strip_prefix("< ").or_else(|| rest.strip_prefix("!= "))?;
    Some(Package { cask, name: name.to_string(), installed: installed.rsplit(", ").next()?.to_string(), new: new.trim().to_string() })
}

fn outdated(which: &Brew, update: bool) -> Option<Outdated> {
    runtime::which("brew")?;
    if update {
        brew(&["update"], Duration::from_secs(300));
    }
    let mut packages = Vec::new();
    let lists = match which {
        Brew::All => vec![(false, Vec::new()), (true, Vec::new())],
        Brew::File(path) => {
            let (formulae, casks) = brewfile(path);
            // With no names brew would list every outdated package
            [(false, formulae), (true, casks)].into_iter().filter(|(_, names)| !names.is_empty()).collect()
        }
    };
    for (cask, names) in lists {
        let mut args = vec!["outdated", "--verbose", if cask { "--cask" } else { "--formula" }];
        args.extend(names.iter().map(String::as_str));
        let out = brew(&args, Duration::from_secs(120));
        packages.extend(out.stdout.lines().filter_map(|l| parse_outdated(l, cask)));
    }
    Some(Outdated { packages, restart: aerospace_restart() })
}

/// AeroSpace keeps running the old version after an update: the installed one, if it differs.
fn aerospace_restart() -> Option<String> {
    let out = runtime::run("aerospace", &["--version"], None, Duration::from_secs(5));
    let versions: Vec<String> = out.stdout.lines().filter_map(|l| l.split_once(" version: ")).filter_map(|(_, v)| v.split_whitespace().next().map(String::from)).collect();
    match versions.as_slice() {
        [cli, server, ..] if cli != server => Some(cli.clone()),
        _ => None,
    }
}

/// A notification of `terminal-notifier`, if installed; a click runs `execute`.
pub(crate) fn notify(group: &str, title: &str, message: &str, execute: &str) {
    if runtime::which("terminal-notifier").is_some() {
        runtime::fire("terminal-notifier", &["-group", group, "-title", title, "-message", message, "-execute", execute]);
    }
}

fn upgrade(packages: Vec<Package>) {
    let log = crate::config::home().join("Library/Logs/rustybar-brew.log");
    let mut output = String::new();
    let (mut upgraded, mut failed) = (Vec::new(), Vec::new());
    for p in &packages {
        let out = brew(&["upgrade", if p.cask { "--cask" } else { "--formula" }, &p.name], Duration::from_secs(1800));
        output.push_str(&format!("$ brew upgrade {}\n{}{}\n", p.name, out.stdout, out.stderr));
        if out.ok() { upgraded.push(p.short().to_string()) } else { failed.push(p.short().to_string()) }
    }
    let _ = std::fs::write(&log, output);
    if failed.is_empty() {
        runtime::fire("terminal-notifier", &["-remove", "rustybar-brew-failed"]);
    } else {
        let title = t(&format!("Aggiornamento non riuscito: {}", failed.join(" ")), &format!("Update failed: {}", failed.join(" "))).to_string();
        notify("rustybar-brew-failed", &title, t("Fai clic per vedere l'output di brew", "Click to see the output of brew"), &format!("open '{}'", log.display()));
    }
    for short in &upgraded {
        restart_daemon(short);
    }
    if upgraded.iter().any(|s| s == "aerospace")
        && let Some(version) = aerospace_restart()
    {
        let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "rustybar".into());
        notify(
            "rustybar-brew-aerospace",
            t(&format!("AeroSpace aggiornato alla {version}"), &format!("AeroSpace updated to {version}")),
            t("Riavvialo per usarla: fai clic per aprire il menu", "Restart it to use it: click to open the menu"),
            &format!("'{exe}' msg popup notifications"),
        );
    }
}

/// A daemon that AeroSpace starts (`after-startup-command`, e.g. borders) runs the old version
/// after an update: restarted with its own command line.
fn restart_daemon(short: &str) {
    if !runtime::run("pgrep", &["-x", short], None, Duration::from_secs(5)).ok() {
        return;
    }
    let config = runtime::run("aerospace", &["config", "--config-path"], None, Duration::from_secs(5));
    let Ok(text) = std::fs::read_to_string(config.stdout.trim()) else { return };
    let commands: Vec<String> = text
        .lines()
        .filter_map(|l| l.trim().trim_end_matches(',').strip_prefix("'exec-and-forget ")?.strip_suffix('\'').map(String::from))
        .filter(|c| c == short || c.starts_with(&format!("{short} ")))
        .collect();
    if commands.is_empty() {
        return;
    }
    runtime::run("pkill", &["-x", short], None, Duration::from_secs(5));
    for _ in 0..25 {
        if !runtime::run("pgrep", &["-x", short], None, Duration::from_secs(5)).ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    for c in commands {
        runtime::shell(&c);
    }
}

/// Quit and open again. Disabled first: AeroSpace puts the windows of hidden workspaces back on
/// screen only when it quits from its menu or is disabled.
fn restart_aerospace() {
    runtime::run("aerospace", &["enable", "off"], None, Duration::from_secs(5));
    std::thread::sleep(Duration::from_secs(1));
    runtime::run("pkill", &["-x", "AeroSpace"], None, Duration::from_secs(5));
    for _ in 0..25 {
        if !runtime::run("pgrep", &["-x", "AeroSpace"], None, Duration::from_secs(5)).ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    runtime::run("/usr/bin/open", &["-a", "AeroSpace"], None, Duration::from_secs(10));
    for _ in 0..50 {
        let out = runtime::run("aerospace", &["--version"], None, Duration::from_secs(5));
        if out.stdout.lines().count() > 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    runtime::fire("terminal-notifier", &["-remove", "rustybar-brew-aerospace"]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        assert_eq!(count("3"), 3);
        assert_eq!(count("99+"), 99);
        assert_eq!(count("•"), 1);
        assert_eq!(count("1,234"), 1234);
        let p = parse_outdated("felixkratz/formulae/borders (1.8.4) < 1.9.0", false).unwrap();
        assert_eq!((p.short(), p.installed.as_str(), p.new.as_str()), ("borders", "1.8.4", "1.9.0"));
        let p = parse_outdated("foo (1.0, 1.1) < 1.2", false).unwrap();
        assert_eq!(p.installed, "1.1");
        let c = parse_outdated("nikitabobko/tap/aerospace (0.20.2-Beta,abc) != 0.21.3-Beta,def", true).unwrap();
        assert_eq!(versions(&c), "0.20.2-Beta → 0.21.3-Beta");
        assert!(parse_outdated("==> Downloading", false).is_none());
    }

    #[test]
    fn dismissed_badges() {
        let badge = |label: &str| Badge { bundle: "com.example.chat".into(), name: "Chat".into(), label: label.into() };
        // With the made-up data, so that nothing goes in the defaults
        let mut w = Notifications {
            apps: vec!["com.example.chat".into()],
            brew: None,
            badges: Some(vec![badge("3")]),
            dismissed: HashMap::new(),
            told: None,
            crashes: Vec::new(),
            outdated: Outdated::default(),
            job: None,
            checking: false,
            mock: Some(Mock::default()),
        };
        let with = |w: &mut Notifications, label: Option<&str>| {
            w.badges = Some(label.map(badge).into_iter().collect());
            w.settle();
            w.unread()
        };
        w.dismiss("com.example.chat");
        assert_eq!(w.unread(), 0);
        // Down, then up from there
        assert_eq!(with(&mut w, Some("2")), 0);
        assert_eq!(with(&mut w, Some("3")), 3);
        // A dot, then a number
        w.dismiss("com.example.chat");
        assert_eq!(with(&mut w, Some("•")), 0);
        assert_eq!(with(&mut w, Some("•")), 0);
        assert_eq!(with(&mut w, Some("1")), 1);
        // Gone, then back even lower
        w.dismiss("com.example.chat");
        assert_eq!(with(&mut w, None), 0);
        assert!(w.dismissed.is_empty());
        assert_eq!(with(&mut w, Some("1")), 1);
        // As they go in the defaults and come back
        let map = HashMap::from([("com.example.chat".to_string(), "•".to_string()), ("com.example.mail".to_string(), "99+".to_string())]);
        assert_eq!(from_dictionary(&to_dictionary(&map)), map);
    }
}
