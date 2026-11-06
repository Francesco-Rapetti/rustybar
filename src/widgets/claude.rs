//! Claude: how much of the plan's limits each Claude account has used.
//!
//! - Bar: the Claude spark (the Claude app's menu bar icon, in the account's color; `✻` without
//!   the app) with the 5-hour session and the week, `42% · 31%`, yellow from `warning`, red from
//!   `critical`, gray while the numbers are stale (the last reading failed, e.g. without network).
//!   The account of `~/.claude` is orange; `accounts` adds others, each with its Claude Code config
//!   dir (`CLAUDE_CONFIG_DIR`) and color. With more than one, each has its own pill, the others on
//!   the left, with the time the session resets (`42% · 31% · 17:07`) and a popup of its own;
//!   slim (when the bar has no room for them: after the volume sliders of `audio`, see
//!   `slim_first`) or compact, one pill, the account whose numbers went up last (the one in use;
//!   remembered in the defaults, so across reloads; the busiest session until one does, or when
//!   that account has no numbers), whose popup has them all, each under its name in its color.
//! - Popup: for each account a header with its spark, the name (or the email), the plan (`Max`),
//!   the email and the organization (not the personal one, named after the email), how old the
//!   numbers are (`3 min fa`, yellow when the last
//!   reading failed, `Aggiornamento…` while it reads), ↻ to read again and sign out, which asks for
//!   a second click (`Fai clic di nuovo per uscire` in place of the email). Under it two tiles, the
//!   session and the week: the percentage, a bar in the account's color (yellow and red as in the
//!   bar, and the tile tinted the same), when it resets (`14:12`, `ven 19:10` when it's more than
//!   a day away) and how long until then (`tra 2 h 57 min`, `al prossimo messaggio` before the
//!   window starts); while the popup is open the times follow the clock. Logged out:
//!   `Nessun account`, the config dir (or a hint) and `Accedi` (`Accedi nel browser…` while Claude
//!   Code waits for the browser; a notification if it fails, whose click opens the output in
//!   `~/Library/Logs/rustybar-claude.log`, `-2.log` for the second account…). An account without
//!   plan limits (an API key) or whose numbers can't be read says so in a yellow tile; without
//!   Claude Code, a red one. At the bottom, the usage page on claude.ai.
//! - Data: Claude Code itself, the only way to these numbers: `claude auth status --json` and
//!   `claude -p` answering the `get_usage` control request (the data of `/usage`) without sending
//!   a message, so without using the plan; Claude Code renews an expired login by itself and asks
//!   the server only when its numbers are more than a minute old. Every 5 minutes, on wake, with
//!   ↻, and when the popup opens if the numbers are more than a minute old. Nothing of the account's
//!   setup is loaded (CLAUDE.md, MCP servers, plugins, hooks) and no session is saved.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_foundation::{NSString, NSUserDefaults, ns_string};
use serde::Deserialize;
use serde_json::Value;

use super::{Cx, Event, Rx, Widget};
use crate::config::Options;
use crate::gfx::{Rgba, Weight};
use crate::i18n::t;
use crate::runtime;
use crate::ui::{Action, Align, Justify, Kind, Node};

const ICON: &str = "/Applications/Claude.app/Contents/Resources/TrayIconTemplate@3x.png";
const ORANGE: &str = "#d97757";
const USAGE: &str = "https://claude.ai/settings/usage";
const EVERY: f64 = 300.0;
/// Numbers younger than this aren't read again when the popup opens.
const FRESH: Duration = Duration::from_secs(60);
/// How often the times in the open popup follow the clock.
const TICK: f64 = 30.0;

const TILE: f64 = 160.0;
const GAP: f64 = 8.0;
const WIDE: f64 = TILE * 2.0 + GAP;
/// Inside the padding of a tile.
const INNER: f64 = TILE - 20.0;
/// The spark of a header, and the gap after it.
const SPARK: f64 = 26.0 + 8.0;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Window {
    percent: f64,
    /// When it resets, in seconds since 1970; None before it starts, with the next message.
    reset: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Account {
    logged_in: bool,
    email: String,
    org: String,
    plan: String,
    name: String,
    session: Option<Window>,
    week: Option<Window>,
    /// A login without plan limits, e.g. an API key.
    no_limits: bool,
    /// The last reading failed: these are the numbers of the one before.
    stale: bool,
    /// When these numbers were read, in seconds since 1970; 0 without numbers.
    at: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
struct Setup {
    /// The config dir of its Claude Code; None for `~/.claude`.
    dir: Option<String>,
    color: String,
}

impl Default for Setup {
    fn default() -> Self {
        Setup { dir: None, color: ORANGE.into() }
    }
}

impl Setup {
    /// What the defaults remember it by: its config dir, which stays when the accounts change.
    fn id(&self) -> &str {
        self.dir.as_deref().unwrap_or("~/.claude")
    }
}

/// Made-up data (see `mock`): the accounts, when the config has none, what they read and the one
/// whose numbers went up last; the resets are in minutes from now, `at` in minutes ago.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    accounts: Vec<Setup>,
    data: Vec<Account>,
    latest: Option<usize>,
}

fn defaults() -> Option<Retained<NSUserDefaults>> {
    NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(ns_string!("rustybar")))
}

/// The defaults key of the account whose numbers went up last, by `Setup::id`.
const LATEST: &str = "claude.latest";

/// Whether the account was used between two readings: the session or the week went up, or one
/// started (a window starts with a message, so it has no reset before, and a later one after it
/// ran out). Not when the numbers are those of the reading before (stale) or of another login.
fn used(old: &Account, new: &Account) -> bool {
    if new.stale || !new.logged_in || old.email != new.email {
        return false;
    }
    let up = |old: &Option<Window>, new: &Option<Window>| {
        let (Some(o), Some(n)) = (old, new) else { return false };
        n.percent > o.percent
            || match (o.reset, n.reset) {
                (None, Some(_)) => true,
                // A new window has fewer percent than the old one; the slack is for a reset the
                // server moves by a few seconds
                (Some(was), Some(is)) => is > was + 60.0 && n.percent > 0.0,
                _ => false,
            }
    };
    up(&old.session, &new.session) || up(&old.week, &new.week)
}

/// Where the installers put `claude`, also outside the PATH: nvm's folders.
fn find_claude() -> Option<String> {
    if let Some(path) = runtime::which("claude") {
        return Some(path);
    }
    let nvm = crate::config::home().join(".nvm/versions/node");
    let mut found: Vec<PathBuf> = std::fs::read_dir(nvm).ok()?.flatten().map(|e| e.path().join("bin/claude")).filter(|p| p.is_file()).collect();
    found.sort();
    found.pop().map(|p| p.display().to_string())
}

fn expand(dir: &str) -> PathBuf {
    match dir.strip_prefix("~/") {
        Some(rest) => crate::config::home().join(rest),
        None => PathBuf::from(dir),
    }
}

/// The environment of Claude Code for the account: its config dir (none for `~/.claude`, also when
/// RustyBar was started with one), and no updates, telemetry or error reports.
fn env(dir: &Option<String>) -> Vec<(&'static str, String)> {
    vec![
        ("CLAUDE_CONFIG_DIR", dir.as_deref().map(|d| expand(d).display().to_string()).unwrap_or_default()),
        ("DISABLE_AUTOUPDATER", "1".into()),
        ("DISABLE_TELEMETRY", "1".into()),
        ("DISABLE_ERROR_REPORTING", "1".into()),
    ]
}

fn claude(path: &str, dir: &Option<String>, args: &[&str], stdin: Option<&str>, timeout: Duration) -> runtime::Output {
    let env = env(dir);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    runtime::run_with(path, args, stdin, timeout, &env, Some(&crate::config::home()))
}

/// The session and the week from the answer to `get_usage`, or whether the login has no limits.
fn parse_usage(stdout: &str) -> Option<Result<(Window, Window), ()>> {
    let window = |w: &Value| Window {
        percent: w["utilization"].as_f64().unwrap_or(0.0).round(),
        reset: w["resets_at"].as_str().and_then(crate::time::iso8601),
    };
    stdout.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find_map(|v| {
        if v["type"] != "control_response" || v["response"]["request_id"] != "usage" {
            return None;
        }
        let r = &v["response"]["response"];
        if r["rate_limits_available"] == false {
            return Some(Err(()));
        }
        let limits = r.get("rate_limits")?;
        Some(Ok((window(&limits["five_hour"]), window(&limits["seven_day"]))))
    })
}

/// The account and its numbers; a failed reading keeps those of `previous`, stale.
fn read(path: &str, dir: &Option<String>, previous: Option<Account>) -> Account {
    let status = claude(path, dir, &["auth", "status", "--json"], None, Duration::from_secs(20));
    let status: Value = serde_json::from_str(&status.stdout).unwrap_or(Value::Null);
    let text = |k: &str| status[k].as_str().unwrap_or_default().to_string();
    if status["loggedIn"] != true {
        return Account::default();
    }
    let config = match dir {
        Some(d) => expand(d).join(".claude.json"),
        None => crate::config::home().join(".claude.json"),
    };
    let config: Value = std::fs::read(config).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
    let name = config["oauthAccount"]["displayName"].as_str().or(config["oauthAccount"]["fullName"].as_str()).unwrap_or_default().to_string();
    let mut account = Account { logged_in: true, email: text("email"), org: text("orgName"), plan: text("subscriptionType"), name, ..Default::default() };
    let request = r#"{"type":"control_request","request_id":"usage","request":{"subtype":"get_usage","skip_behaviors":true}}"#;
    let args = ["-p", "--safe-mode", "--strict-mcp-config", "--no-session-persistence", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"];
    let out = claude(path, dir, &args, Some(&format!("{request}\n")), Duration::from_secs(30));
    match parse_usage(&out.stdout) {
        Some(Ok((session, week))) => (account.session, account.week, account.at) = (Some(session), Some(week), crate::time::now()),
        Some(Err(())) => account.no_limits = true,
        None => {
            if let Some(p) = previous.filter(|p| p.email == account.email && p.session.is_some()) {
                (account.session, account.week, account.at, account.stale) = (p.session, p.week, p.at, true);
            }
        }
    }
    account
}

/// When a window resets, as on a clock: `14:12`, with the day when it's more than a day away
/// (`ven 19:10`); nothing before it starts.
fn reset_clock(reset: Option<f64>) -> String {
    let Some(at) = reset else { return String::new() };
    let tm = crate::time::local(at);
    let time = format!("{:02}:{:02}", tm.tm_hour, tm.tm_min);
    if at - crate::time::now() < 86_400.0 {
        return time;
    }
    let days = if crate::i18n::italian() { ["dom", "lun", "mar", "mer", "gio", "ven", "sab"] } else { ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"] };
    format!("{} {time}", days[tm.tm_wday.clamp(0, 6) as usize])
}

/// How long until a window resets: `tra 2 h 57 min`, or that it starts with the next message.
fn reset_in(reset: Option<f64>) -> String {
    let Some(at) = reset else { return t("al prossimo messaggio", "with the next message").into() };
    let left = at - crate::time::now();
    if left < 60.0 {
        return t("tra meno di un minuto", "in under a minute").into();
    }
    let span = crate::time::span(left);
    t(&format!("tra {span}"), &format!("in {span}")).into()
}

/// How old numbers read at `at` are: `adesso`, `3 min fa`.
fn age(at: f64) -> String {
    let age = crate::time::now() - at;
    if age < 60.0 {
        return t("adesso", "just now").into();
    }
    let span = crate::time::span(age);
    t(&format!("{span} fa"), &format!("{span} ago")).into()
}

/// Under the name: the email (unless it's the name already) and the organization, but not the
/// personal one, which Claude names after the email (`alex@example.com's Organization`).
fn details(a: &Account) -> String {
    let email = if a.name.is_empty() { "" } else { a.email.as_str() };
    let org = if !a.email.is_empty() && a.org.contains(&a.email) { "" } else { a.org.as_str() };
    [email, org].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
}

/// `team` → `Team`.
fn plan_name(plan: &str) -> String {
    let mut chars = plan.chars();
    match chars.next() {
        Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// A small button with a symbol, as on Docker's tiles; disabled without `action`.
fn button(symbol: &str, color: Rgba, background: Rgba, action: Option<Action>, key: &str) -> Node {
    let b = Node::row(vec![Node::symbol(symbol, 11.0, Weight::Bold, color)]).size(28.0, 22.0).justify(Justify::Center).radius(6.0).background(background).key(key);
    match action {
        Some(a) => b.click(a),
        None => b,
    }
}

/// A tile two wide that says something in `color`, with a detail under it.
fn note(rx: &Rx, symbol: &str, color: Rgba, text: &str, detail: Option<&str>, key: &str) -> Node {
    let th = rx.theme;
    let mut lines = vec![Node::text(text, th.font(), color).max_width(WIDE - 20.0 - SPARK).key("text")];
    lines.extend(detail.map(|d| Node::text(d, th.small_font(), th.subtext).max_width(WIDE - 20.0 - SPARK).key("detail")));
    Node::row(vec![Node::row(vec![Node::symbol(symbol, 16.0, Weight::Semibold, color)]).width(26.0).justify(Justify::Center).key("icon"), Node::column(lines).gap(2.0).key("lines")])
        .gap(8.0)
        .align(Align::Center)
        .padding(10.0, 10.0, 10.0, 10.0)
        .width(WIDE)
        .radius(10.0)
        .background(th.tile(color, true))
        .key(key)
}

pub struct Claude {
    accounts: Vec<Setup>,
    data: Vec<Option<Account>>,
    fetched: Vec<Option<Instant>>,
    refreshing: Vec<bool>,
    logging_in: Vec<bool>,
    confirm_logout: Option<usize>,
    /// The account whose pill opened the popup, when each has its own.
    focus: Option<usize>,
    /// The account whose numbers went up last, the one the single pill shows.
    latest: Option<usize>,
    /// Changes when the popup opens and closes, which stops the ticks of the one before.
    ticking: u64,
    path: Option<String>,
    missing: bool,
    warning: f64,
    critical: f64,
    mock: bool,
}

impl Claude {
    pub fn new(options: &Options) -> Claude {
        let mut accounts = vec![Setup { dir: None, color: options.str("color").unwrap_or(ORANGE).to_string() }];
        if let Some(list) = options.0.get("accounts").cloned() {
            match list.try_into::<Vec<Setup>>() {
                Ok(list) => accounts.extend(list),
                Err(e) => crate::log(&format!("claude: accounts: {e}")),
            }
        }
        let mock = crate::mock::load::<Mock>("claude");
        let mut data = vec![None; accounts.len()];
        if let Some(m) = &mock {
            if accounts.len() == 1 {
                accounts.extend(m.accounts.iter().cloned());
            }
            let now = crate::time::now();
            data = (0..accounts.len())
                .map(|i| {
                    let mut a = m.data.get(i).cloned().unwrap_or_default();
                    for w in [&mut a.session, &mut a.week].into_iter().flatten() {
                        w.reset = w.reset.map(|minutes| now + minutes * 60.0);
                    }
                    if a.session.is_some() {
                        a.at = now - a.at * 60.0;
                    }
                    Some(a)
                })
                .collect();
        }
        let n = accounts.len();
        let latest = match &mock {
            Some(m) => m.latest,
            None => defaults().and_then(|d| d.stringForKey(ns_string!(LATEST))).and_then(|id| accounts.iter().position(|a| a.id() == id.to_string())),
        };
        Claude {
            latest: latest.filter(|i| *i < n),
            accounts,
            data,
            fetched: vec![None; n],
            refreshing: vec![false; n],
            logging_in: vec![false; n],
            confirm_logout: None,
            focus: None,
            ticking: 0,
            path: None,
            missing: false,
            warning: options.f64("warning").unwrap_or(75.0),
            critical: options.f64("critical").unwrap_or(90.0),
            mock: mock.is_some(),
        }
    }

    fn color(&self, rx: &Rx, i: usize) -> Rgba {
        rx.theme.color(&self.accounts[i].color).unwrap_or(rx.theme.primary)
    }

    /// Reads account `i` again, unless it's being read or `fresh` and read less than a minute ago.
    fn refresh(&mut self, i: usize, fresh: bool, cx: &mut Cx) {
        if self.mock || self.refreshing[i] || self.logging_in[i] || (fresh && self.fetched[i].is_some_and(|f| f.elapsed() < FRESH)) {
            return;
        }
        self.refreshing[i] = true;
        cx.redraw();
        let (dir, previous, path) = (self.accounts[i].dir.clone(), self.data[i].clone(), self.path.clone());
        cx.background::<Claude, Option<(String, Account)>>(
            move || {
                let path = path.or_else(find_claude)?;
                let account = read(&path, &dir, previous);
                Some((path, account))
            },
            move |w, result, cx| {
                w.refreshing[i] = false;
                w.fetched[i] = Some(Instant::now());
                match result {
                    Some((path, account)) => {
                        w.path = Some(path);
                        w.missing = false;
                        if w.data[i].as_ref().is_some_and(|old| used(old, &account)) {
                            w.remember(i);
                        }
                        w.data[i] = Some(account);
                    }
                    None => {
                        w.missing = true;
                        w.data[i] = Some(Account::default());
                    }
                }
                cx.redraw();
            },
        );
    }

    fn refresh_all(&mut self, fresh: bool, cx: &mut Cx) {
        for i in 0..self.accounts.len() {
            self.refresh(i, fresh, cx);
        }
    }

    fn log_path(i: usize) -> PathBuf {
        let name = if i == 0 { "rustybar-claude.log".to_string() } else { format!("rustybar-claude-{}.log", i + 1) };
        crate::config::home().join("Library/Logs").join(name)
    }

    /// `claude auth login` opens the sign-in page in the browser and waits for it; after 5 minutes
    /// it's stopped. `logout` too, at once. Then the account is read again.
    fn auth(&mut self, i: usize, login: bool, cx: &mut Cx) {
        let Some(path) = self.path.clone().filter(|_| !self.mock) else { return };
        if login {
            self.logging_in[i] = true;
        }
        let dir = self.accounts[i].dir.clone();
        cx.background::<Claude, ()>(
            move || {
                let (args, timeout) = if login { (["auth", "login"], 300) } else { (["auth", "logout"], 30) };
                let out = claude(&path, &dir, &args, None, Duration::from_secs(timeout));
                let log = Claude::log_path(i);
                let _ = std::fs::write(&log, format!("$ claude {}\n{}{}", args.join(" "), out.stdout, out.stderr));
                if !out.ok() {
                    let title = if login { t("Accesso a Claude non riuscito", "Claude sign-in failed") } else { t("Uscita da Claude non riuscita", "Claude sign-out failed") };
                    super::notifications::notify(&format!("rustybar-claude-{i}"), title, t("Fai clic per vedere l'output di claude", "Click to see the output of claude"), &format!("open '{}'", log.display()));
                }
            },
            move |w, _, cx| {
                w.logging_in[i] = false;
                w.refresh(i, false, cx);
            },
        );
        cx.redraw();
    }

    /// The account whose numbers went up last, in the defaults too (not with the made-up data).
    fn remember(&mut self, i: usize) {
        if self.latest == Some(i) {
            return;
        }
        self.latest = Some(i);
        if !self.mock
            && let Some(d) = defaults()
        {
            let id = NSString::from_str(self.accounts[i].id());
            unsafe { d.setObject_forKey(Some(&id), ns_string!(LATEST)) };
        }
    }

    /// The account with the busiest session, the first at a tie and without numbers.
    fn busiest(&self) -> usize {
        let session = |i: usize| self.data[i].as_ref().and_then(|a| a.session.as_ref()).map(|s| s.percent);
        (0..self.accounts.len()).fold(0, |best, i| if session(i) > session(best) { i } else { best })
    }

    /// The account of the single pill: the one whose numbers went up last, also while it's still
    /// being read; the busiest when none has yet, or when that one has no numbers (logged out).
    fn single(&self) -> usize {
        self.latest.filter(|&i| self.data[i].as_ref().is_none_or(|a| a.session.is_some())).unwrap_or_else(|| self.busiest())
    }

    /// The spark of the Claude app in the account's color, `✻` without the app.
    fn spark(&self, rx: &Rx, i: usize) -> Node {
        let color = self.color(rx, i);
        let icon = if std::path::Path::new(ICON).exists() { Node::image(ICON, 22.0, 22.0, Some(color)) } else { Node::text("✻", rx.theme.font_with(Weight::Bold, 15.0), color) };
        icon.key("icon")
    }

    /// While the popup is open, the times in it (how old the numbers are, how long until they
    /// reset) follow the clock.
    fn tick(&mut self, ticking: u64, cx: &mut Cx) {
        cx.after::<Claude>(TICK, move |w, cx| {
            if w.ticking == ticking && cx.popup_open {
                cx.redraw();
                w.tick(ticking, cx);
            }
        });
    }

    fn pill(&self, rx: &Rx, i: usize, time: bool, action: Action) -> Node {
        let th = rx.theme;
        let mut children = vec![self.spark(rx, i)];
        if let Some(a) = &self.data[i]
            && let (Some(s), Some(w)) = (&a.session, &a.week)
        {
            let mut label = format!("{:.0}% · {:.0}%", s.percent, w.percent);
            if time && let Some(reset) = s.reset {
                let tm = crate::time::local(reset);
                label.push_str(&format!(" · {:02}:{:02}", tm.tm_hour, tm.tm_min));
            }
            let c = if a.stale { th.overlay } else { th.level(s.percent.max(w.percent), self.warning, self.critical, th.text) };
            children.push(Node::text(label, th.font(), c).key("label"));
        }
        // The image has room around the spark: less padding on its side
        let mut pill = rx.pill(children).gap(4.0).key(format!("account-{i}")).click(action);
        pill.style.padding[3] = (th.pill_padding - 4.0).max(0.0);
        pill
    }

    /// The header of account `i`: its spark, name and plan, the email and the organization (or
    /// what's going on instead), how old the numbers are, ↻ and sign out.
    fn header(&self, rx: &Rx, i: usize) -> Node {
        let th = rx.theme;
        let small = th.small_font();
        let a = self.data[i].as_ref();
        let logged_in = a.is_some_and(|a| a.logged_in);
        let (name, plan, details) = match a {
            None => (t("Lettura…", "Reading…").to_string(), None, String::new()),
            Some(a) if !a.logged_in => {
                let hint = t("Accedi per vedere quanto hai usato Claude", "Sign in to see how much you've used Claude");
                (t("Nessun account", "No account").to_string(), None, self.accounts[i].dir.clone().unwrap_or_else(|| hint.into()))
            }
            Some(a) => (if a.name.is_empty() { a.email.clone() } else { a.name.clone() }, Some(plan_name(&a.plan)).filter(|p| !p.is_empty()), details(a)),
        };
        let confirm = self.confirm_logout == Some(i);
        let (details, details_color) = if confirm { (t("Fai clic di nuovo per uscire", "Click again to sign out").to_string(), th.red) } else { (details, th.subtext) };

        // Name, plan, ↻ and sign out
        let mut top = vec![Node::text(name, th.font(), th.text).max_width(WIDE - 150.0).key("name")];
        if let Some(plan) = plan {
            let color = self.color(rx, i);
            top.push(Node::row(vec![Node::text(plan, small.clone(), color)]).padding(0.0, 5.0, 0.0, 5.0).height(16.0).radius(4.0).background(color.faded(0.16)).key("plan"));
        }
        top.push(Node::spacer());
        let busy = a.is_none() || self.refreshing[i] || self.logging_in[i];
        let (color, background) = if busy { (th.overlay, th.surface.faded(0.35)) } else { (th.text, th.surface.faded(0.7)) };
        top.push(button("arrow.clockwise", color, background, (!busy).then(|| rx.action(&format!("refresh:{i}"))), "refresh"));
        if logged_in {
            let (color, background) = if confirm { (th.red, th.red.faded(0.22)) } else { (th.text, th.surface.faded(0.7)) };
            top.push(button("rectangle.portrait.and.arrow.right", color, background, Some(rx.action(&format!("logout:{i}"))), "logout"));
        }
        let mut lines = vec![Node::row(top).gap(6.0).align(Align::Center).width(WIDE - SPARK).key("top")];

        // The details, and how old the numbers are
        let age = if self.refreshing[i] {
            Some((t("Aggiornamento…", "Updating…").to_string(), th.subtext))
        } else {
            a.filter(|a| a.session.is_some() && a.at > 0.0).map(|a| (age(a.at), if a.stale { th.yellow } else { th.subtext }))
        };
        if !details.is_empty() || age.is_some() {
            let mut bottom = vec![Node::text(details, small.clone(), details_color).max_width(WIDE - SPARK - 90.0).key("details"), Node::spacer()];
            bottom.extend(age.map(|(text, color)| Node::text(text, small.clone(), color).key("age")));
            lines.push(Node::row(bottom).gap(6.0).width(WIDE - SPARK).key("bottom"));
        }
        Node::row(vec![Node::row(vec![self.spark(rx, i)]).width(26.0).justify(Justify::Center).key("spark"), Node::column(lines).gap(2.0).key("lines")])
            .gap(8.0)
            .align(Align::Center)
            .key("header")
    }

    /// The session or the week: how much is used, a bar, when it resets and how long until then.
    fn window(&self, rx: &Rx, i: usize, a: &Account, w: &Window, week: bool) -> Node {
        let th = rx.theme;
        let small = th.small_font();
        let level = |normal| if a.stale { th.overlay } else { th.level(w.percent, self.warning, self.critical, normal) };
        let color = level(self.color(rx, i));
        let label = if week { t("Settimana", "Week") } else { t("Sessione", "Session") };
        Node::column(vec![
            Node::row(vec![Node::text(label, small.clone(), th.subtext).key("label"), Node::spacer(), Node::text(reset_clock(w.reset), small.clone(), th.subtext).key("clock")]).width(INNER).key("top"),
            Node::text(format!("{:.0}%", w.percent), th.font_with(Weight::Semibold, 20.0), level(th.text)).key("percent"),
            Node::new(Kind::Progress { value: w.percent / 100.0, color, track: th.surface }).size(INNER, 6.0).key("bar"),
            Node::text(reset_in(w.reset), small, th.subtext).max_width(INNER).key("left"),
        ])
        .gap(4.0)
        .padding(10.0, 10.0, 10.0, 10.0)
        .width(TILE)
        .radius(10.0)
        .background(th.tile(color, !a.stale))
        .key(if week { "week" } else { "session" })
    }

    /// Account `i`: its header, and the tiles of its numbers or the sign-in button.
    fn account(&self, rx: &Rx, i: usize) -> Node {
        let th = rx.theme;
        let mut rows = vec![self.header(rx, i)];
        match &self.data[i] {
            None => {}
            Some(a) if !a.logged_in => {
                let (symbol, label, color, action) = if self.logging_in[i] {
                    ("hourglass", t("Accedi nel browser…", "Sign in in the browser…"), th.yellow, None)
                } else {
                    ("person.crop.circle", t("Accedi", "Sign In"), th.primary, Some(rx.action(&format!("login:{i}"))))
                };
                let mut sign_in = Node::row(vec![Node::symbol(symbol, 11.0, Weight::Bold, color).key("icon"), Node::text(label, th.font_with(Weight::Bold, 12.0), if action.is_some() { th.text } else { color }).key("label")])
                    .gap(6.0)
                    .justify(Justify::Center)
                    .size(WIDE, 30.0)
                    .radius(15.0)
                    .background(th.surface.faded(0.7))
                    .key("login");
                if let Some(action) = action {
                    sign_in = sign_in.click(action);
                }
                rows.push(sign_in);
            }
            Some(a) => match (&a.session, &a.week) {
                (Some(s), Some(w)) => rows.push(Node::row(vec![self.window(rx, i, a, s, false), self.window(rx, i, a, w, true)]).gap(GAP).align(Align::Stretch).key("windows")),
                _ if a.no_limits => rows.push(note(rx, "infinity", th.yellow, t("Il tuo account non ha limiti del piano", "Your account has no plan limits"), None, "status")),
                _ => rows.push(note(rx, "exclamationmark.triangle.fill", th.yellow, t("Impossibile leggere l'uso di Claude", "Can't read the usage of Claude"), Some(t("Riprova con ↻", "Try again with ↻")), "status")),
            },
        }
        Node::column(rows).gap(8.0).key(format!("account-{i}"))
    }

    /// One pill per account, when there's room.
    fn pills(&self, rx: &Rx) -> bool {
        !rx.slim && self.accounts.len() > 1
    }
}

impl Widget for Claude {
    fn start(&mut self, cx: &mut Cx) {
        if self.mock {
            return;
        }
        self.refresh_all(false, cx);
        cx.every::<Claude>(EVERY, |w, cx| w.refresh_all(false, cx));
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        if !self.pills(rx) {
            return Some(self.pill(rx, self.single(), false, rx.toggle_popup()).key(rx.name));
        }
        // The others on the left of the one of ~/.claude
        let order = (1..self.accounts.len()).chain([0]);
        let pills = order.map(|i| self.pill(rx, i, true, rx.action(&format!("open:{i}")))).collect();
        Some(Node::row(pills).gap(th.item_gap).height(th.pill_height).key(rx.name))
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let mut rows = Vec::new();
        if self.missing {
            let detail = t("RustyBar legge l'uso del piano da Claude Code", "RustyBar reads the plan's usage through Claude Code");
            rows.push(note(rx, "xmark.octagon.fill", th.red, t("Claude Code non trovato", "Claude Code not found"), Some(detail), "missing"));
        } else {
            let shown: Vec<usize> = match self.focus {
                Some(i) if self.pills(rx) => vec![i],
                _ => (0..self.accounts.len()).collect(),
            };
            rows.extend(shown.into_iter().map(|i| self.account(rx, i)));
        }
        rows.push(rx.row_button(vec![Node::text(t("Apri l'utilizzo su claude.ai", "Open Usage on claude.ai"), th.font(), th.primary)], vec![], rx.action("usage")).key("usage"));
        Some(rx.popup(WIDE, rows).gap(14.0).padding(10.0, 8.0, 6.0, 8.0))
    }

    fn ready(&self) -> bool {
        self.data[0].is_some()
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        if name == "usage" {
            cx.close_popup();
            if !self.mock {
                runtime::open(USAGE);
            }
            return;
        }
        let Some((kind, i)) = name.split_once(':').and_then(|(k, i)| Some((k, i.parse::<usize>().ok().filter(|i| *i < self.accounts.len())?))) else { return };
        match kind {
            // A pill of its own: the popup of its account, or the same popup switched to it
            "open" => {
                let switch = cx.popup_open && self.focus != Some(i);
                self.focus = Some(i);
                if switch {
                    self.refresh(i, true, cx);
                    cx.redraw();
                } else {
                    cx.toggle_popup();
                }
            }
            "refresh" => self.refresh(i, false, cx),
            "login" => self.auth(i, true, cx),
            "logout" if self.confirm_logout == Some(i) => {
                self.confirm_logout = None;
                self.auth(i, false, cx);
            }
            "logout" => {
                self.confirm_logout = Some(i);
                cx.redraw();
                cx.after::<Claude>(4.0, move |w, cx| {
                    if w.confirm_logout == Some(i) {
                        w.confirm_logout = None;
                        cx.redraw();
                    }
                });
            }
            _ => {}
        }
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        match event {
            Event::Wake => self.refresh_all(false, cx),
            Event::Trigger { name, .. } if name == "claude_update" => self.refresh_all(false, cx),
            Event::PopupOpened => {
                self.ticking += 1;
                self.tick(self.ticking, cx);
                match self.focus {
                    Some(i) => self.refresh(i, true, cx),
                    None => self.refresh_all(true, cx),
                }
            }
            Event::PopupClosed => {
                self.ticking += 1;
                self.focus = None;
                self.confirm_logout = None;
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
    fn usage() {
        let out = r#"{"type":"system"}
{"type":"control_response","response":{"subtype":"success","request_id":"usage","response":{"rate_limits":{"five_hour":{"utilization":41.6,"resets_at":"2026-10-30T18:30:00.000+00:00"},"seven_day":{"utilization":31,"resets_at":null}}}}}"#;
        let (s, w) = parse_usage(out).unwrap().unwrap();
        assert_eq!((s.percent, s.reset), (42.0, Some(1_793_385_000.0)));
        assert_eq!((w.percent, w.reset), (31.0, None));
        let none = r#"{"type":"control_response","response":{"request_id":"usage","response":{"rate_limits_available":false}}}"#;
        assert_eq!(parse_usage(none), Some(Err(())));
        assert_eq!(parse_usage("garbage"), None);
        assert_eq!(plan_name("max"), "Max");
        assert_eq!(plan_name(""), "");
    }

    #[test]
    fn details_skip_the_personal_organization() {
        let account = |name: &str, email: &str, org: &str| Account { name: name.into(), email: email.into(), org: org.into(), ..Default::default() };
        assert_eq!(details(&account("Alex Doe", "alex@example.com", "alex@example.com's Organization")), "alex@example.com");
        assert_eq!(details(&account("Alex Doe", "alex@work.example", "Example Inc.")), "alex@work.example · Example Inc.");
        // Without a name the email is the name already
        assert_eq!(details(&account("", "alex@example.org", "alex@example.org's Organization")), "");
        assert_eq!(details(&account("", "alex@example.org", "Example Inc.")), "Example Inc.");
        assert_eq!(details(&account("", "", "")), "");
    }

    #[test]
    fn used_when_the_numbers_go_up() {
        let window = |percent: f64, reset: Option<f64>| Some(Window { percent, reset });
        let account = |session, week| Account { logged_in: true, email: "alex@example.com".into(), session, week, ..Default::default() };
        let old = account(window(42.0, Some(1000.0)), window(31.0, Some(9000.0)));
        assert!(!used(&old, &old.clone()));
        assert!(used(&old, &account(window(43.0, Some(1000.0)), window(31.0, Some(9000.0)))));
        assert!(used(&old, &account(window(42.0, Some(1000.0)), window(32.0, Some(9000.0)))));
        // The session ran out: going down isn't use, a new one with something in it is
        assert!(!used(&old, &account(window(0.0, None), window(31.0, Some(9000.0)))));
        assert!(used(&old, &account(window(3.0, Some(19_000.0)), window(31.0, Some(9000.0)))));
        assert!(used(&account(window(0.0, None), window(31.0, Some(9000.0))), &account(window(0.0, Some(19_000.0)), window(31.0, Some(9000.0)))));
        // The same reset a few seconds off
        assert!(!used(&old, &account(window(42.0, Some(1010.0)), window(31.0, Some(9000.0)))));
        // Stale numbers, another login, logged out
        let mut stale = account(window(50.0, Some(1000.0)), window(31.0, Some(9000.0)));
        stale.stale = true;
        assert!(!used(&old, &stale));
        let mut other = account(window(50.0, Some(1000.0)), window(31.0, Some(9000.0)));
        other.email = "alex@work.example".into();
        assert!(!used(&old, &other));
        assert!(!used(&old, &Account::default()));
    }

    #[test]
    fn times() {
        let now = crate::time::now();
        assert_eq!(reset_clock(None), "");
        assert_eq!(reset_clock(Some(now + 3600.0)).len(), 5);
        assert!(reset_clock(Some(now + 3.0 * 86_400.0)).contains(' '));
        assert!(reset_in(Some(now + 30.0)).contains("minut"));
        assert!(reset_in(Some(now + 2.0 * 3600.0 + 90.0)).ends_with("2 h 1 min"));
        assert!(age(now - 10.0) == "adesso" || age(now - 10.0) == "just now");
        assert!(age(now - 180.0).starts_with("3 min "));
    }
}
