//! Claude: how much of the plan's limits each Claude account has used.
//!
//! - Bar: the Claude spark (the Claude app's menu bar icon, in the account's color; `✻` without
//!   the app) with the 5-hour session and the week, `42% · 31%`, yellow from `warning`, red from
//!   `critical`, gray while the numbers are stale (the last reading failed, e.g. without network).
//!   The account of `~/.claude` is orange; `accounts` adds others, each with its Claude Code config
//!   dir (`CLAUDE_CONFIG_DIR`) and color. With more than one, each has its own pill, the others on
//!   the left, with the time the session resets (`42% · 31% · 17:07`) and a popup of its own;
//!   slim (when the bar has no room for them: after the volume sliders of `audio`, see
//!   `slim_first`) or compact, one pill, the account with the busiest session, whose popup has
//!   them all, each under its name in its color.
//! - Popup: the name (or the email) and the plan (`Claude Max`), the email and the organization;
//!   for the session and the week the percentage, a bar in the account's color (yellow and red as
//!   in the bar) and the reset (`Reset tra 2 h 57 min · 01:10`, `Reset tra 3 g 5 h · ven 19:10`,
//!   `Parte con il prossimo messaggio` before the window starts). `Esci`, which asks for a second
//!   click (`Fai clic di nuovo per uscire`). While it reads again, `Aggiornamento…` in place of the
//!   plan. Logged out: `Nessun account`, the config dir, a hint and `Accedi` (`Accedi nel browser…`
//!   while Claude Code waits for the browser; a notification if it fails, whose click opens the
//!   output in `~/Library/Logs/rustybar-claude.log`, `-2.log` for the second account…). An
//!   account without plan limits (an API key) or whose numbers can't be read says so in yellow;
//!   without Claude Code, `Claude Code non trovato` in red.
//! - Data: Claude Code itself, the only way to these numbers: `claude auth status --json` and
//!   `claude -p` answering the `get_usage` control request (the data of `/usage`) without sending
//!   a message, so without using the plan; Claude Code renews an expired login by itself and asks
//!   the server only when its numbers are more than a minute old. Every 5 minutes, on wake, and
//!   when the popup opens if the numbers are more than a minute old. Nothing of the account's
//!   setup is loaded (CLAUDE.md, MCP servers, plugins, hooks) and no session is saved.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

use super::{Cx, Event, Rx, Widget};
use crate::config::Options;
use crate::gfx::{Rgba, Weight};
use crate::i18n::t;
use crate::runtime;
use crate::ui::{Kind, Node};

const ICON: &str = "/Applications/Claude.app/Contents/Resources/TrayIconTemplate@3x.png";
const ORANGE: &str = "#d97757";
const EVERY: f64 = 300.0;
/// Numbers younger than this aren't read again when the popup opens.
const FRESH: Duration = Duration::from_secs(60);

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

/// Made-up data (see `mock`): the accounts, when the config has none, and what they read; the
/// resets are in minutes from now.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    accounts: Vec<Setup>,
    data: Vec<Account>,
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
        Some(Ok((session, week))) => (account.session, account.week) = (Some(session), Some(week)),
        Some(Err(())) => account.no_limits = true,
        None => {
            if let Some(p) = previous.filter(|p| p.email == account.email && p.session.is_some()) {
                (account.session, account.week, account.stale) = (p.session, p.week, true);
            }
        }
    }
    account
}

/// `Reset tra 2 h 57 min · 01:10`, with the day when it's more than a day away.
fn reset_text(reset: Option<f64>) -> String {
    let Some(at) = reset else { return t("Parte con il prossimo messaggio", "Starts with the next message").into() };
    let left = at - crate::time::now();
    if left < 60.0 {
        return t("Reset tra meno di un minuto", "Resets in less than a minute").into();
    }
    let tm = crate::time::local(at);
    let mut when = format!("{:02}:{:02}", tm.tm_hour, tm.tm_min);
    if left >= 86_400.0 {
        let days = if crate::i18n::italian() { ["dom", "lun", "mar", "mer", "gio", "ven", "sab"] } else { ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"] };
        when = format!("{} {when}", days[tm.tm_wday.clamp(0, 6) as usize]);
    }
    let span = crate::time::span(left);
    format!("{} · {when}", t(&format!("Reset tra {span}"), &format!("Resets in {span}")))
}

/// `team` → `Claude Team`.
fn plan_name(plan: &str) -> String {
    let mut chars = plan.chars();
    match chars.next() {
        Some(c) => format!("Claude {}{}", c.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
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
                    Some(a)
                })
                .collect();
        }
        let n = accounts.len();
        Claude {
            accounts,
            data,
            fetched: vec![None; n],
            refreshing: vec![false; n],
            logging_in: vec![false; n],
            confirm_logout: None,
            focus: None,
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

    /// The account with the busiest session, the first at a tie and without numbers.
    fn busiest(&self) -> usize {
        let session = |i: usize| self.data[i].as_ref().and_then(|a| a.session.as_ref()).map(|s| s.percent);
        (0..self.accounts.len()).fold(0, |best, i| if session(i) > session(best) { i } else { best })
    }

    fn pill(&self, rx: &Rx, i: usize, time: bool, action: crate::ui::Action) -> Node {
        let th = rx.theme;
        let color = self.color(rx, i);
        let icon = if std::path::Path::new(ICON).exists() { Node::image(ICON, 22.0, 22.0, Some(color)) } else { Node::text("✻", th.font_with(Weight::Bold, 15.0), color) };
        let mut children = vec![icon.key("icon")];
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

    /// The rows of account `i`, its name in `name_color`.
    fn section(&self, rx: &Rx, i: usize, name_color: Rgba, rows: &mut Vec<Node>) {
        let th = rx.theme;
        let bold = th.font();
        let key = |k: &str| format!("{i}-{k}");
        let line = |left: Node, right: Option<Node>, k: &str| {
            let mut c = vec![left, Node::spacer()];
            c.extend(right);
            Node::row(c).gap(16.0).padding(0.0, 8.0, 0.0, 8.0).key(key(k))
        };
        let small = |text: &str, k: &str, top: f64| Node::text(text, th.small_font(), th.subtext).padding(top, 8.0, 0.0, 8.0).key(key(k));
        let action = |text: &str, color: Rgba, k: &str, clickable: bool| {
            let mut row = Node::row(vec![Node::text(text, bold.clone(), color)]).padding(0.0, 8.0, 0.0, 8.0).height(26.0).radius(th.row_radius).key(key(k));
            if clickable {
                row = row.click(rx.action(&format!("{k}:{i}")));
            }
            row
        };
        let updating = self.refreshing[i].then(|| Node::text(t("Aggiornamento…", "Updating…"), th.small_font(), th.subtext).key("plan"));
        let Some(a) = &self.data[i] else {
            rows.push(line(Node::text(t("Lettura…", "Reading…"), bold.clone(), name_color), None, "name"));
            return;
        };
        if self.missing {
            rows.push(line(Node::text(t("Claude Code non trovato", "Claude Code not found"), bold.clone(), th.red), None, "name"));
            return;
        }
        let dir = self.accounts[i].dir.clone();
        if !a.logged_in {
            rows.push(line(Node::text(t("Nessun account", "No account"), bold.clone(), name_color), updating, "name"));
            if let Some(d) = &dir {
                rows.push(small(d, "dir", 2.0));
            }
            rows.push(small(t("Accedi per vedere quanto hai usato Claude", "Sign in to see how much you've used Claude"), "hint", 2.0));
            rows.push(Node::default().height(4.0).key(key("gap")));
            rows.push(if self.logging_in[i] { action(t("Accedi nel browser…", "Sign in in the browser…"), th.yellow, "login", false) } else { action(t("Accedi", "Sign In"), th.primary, "login", true) });
            return;
        }
        let name = if a.name.is_empty() { &a.email } else { &a.name };
        let plan = updating.unwrap_or_else(|| Node::text(plan_name(&a.plan), th.small_font(), th.subtext).key("plan"));
        rows.push(line(Node::text(name, bold.clone(), name_color), Some(plan), "name"));
        let details: Vec<&str> = [if a.name.is_empty() { "" } else { a.email.as_str() }, a.org.as_str()].into_iter().filter(|s| !s.is_empty()).collect();
        if !details.is_empty() {
            rows.push(small(&details.join(" · "), "details", 2.0));
        }
        match (&a.session, &a.week) {
            (Some(s), Some(w)) => {
                for (title, window, k) in [(t("Sessione (5 ore)", "Session (5 hours)"), s, "session"), (t("Settimanale", "Weekly"), w, "week")] {
                    let level = |normal| if a.stale { th.overlay } else { th.level(window.percent, self.warning, self.critical, normal) };
                    rows.push(Node::default().height(8.0).key(key(&format!("{k}-gap"))));
                    rows.push(line(Node::text(title, bold.clone(), th.text), Some(Node::text(format!("{:.0}%", window.percent), bold.clone(), level(th.text))), k));
                    let bar = Node::new(Kind::Progress { value: window.percent / 100.0, color: level(self.color(rx, i)), track: th.surface }).height(6.0).grow();
                    rows.push(Node::row(vec![bar]).padding(6.0, 8.0, 4.0, 8.0).key(key(&format!("{k}-bar"))));
                    rows.push(small(&reset_text(window.reset), &format!("{k}-reset"), 0.0));
                }
            }
            _ => {
                let text = if a.no_limits { t("Il tuo account non ha limiti del piano", "Your account has no plan limits") } else { t("Impossibile leggere l'uso di Claude", "Can't read the usage of Claude") };
                rows.push(Node::default().height(6.0).key(key("status-gap")));
                rows.push(line(Node::text(text, bold.clone(), th.yellow), None, "status"));
            }
        }
        rows.push(Node::default().height(6.0).key(key("logout-gap")));
        rows.push(if self.confirm_logout == Some(i) { action(t("Fai clic di nuovo per uscire", "Click again to sign out"), th.red, "logout", true) } else { action(t("Esci", "Sign Out"), th.primary, "logout", true) });
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
            return Some(self.pill(rx, self.busiest(), false, rx.toggle_popup()).key(rx.name));
        }
        // The others on the left of the one of ~/.claude
        let order = (1..self.accounts.len()).chain([0]);
        let pills = order.map(|i| self.pill(rx, i, true, rx.action(&format!("open:{i}")))).collect();
        Some(Node::row(pills).gap(th.item_gap).height(th.pill_height).key(rx.name))
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let mut rows = vec![Node::default().height(4.0).key("top")];
        let shown: Vec<usize> = match self.focus {
            Some(i) if self.pills(rx) => vec![i],
            _ => (0..self.accounts.len()).collect(),
        };
        for (n, &i) in shown.iter().enumerate() {
            if n > 0 {
                rows.push(Node::default().height(1.0).background(th.surface).padding(0.0, 8.0, 0.0, 8.0).key(format!("{i}-divider")));
            }
            let color = if shown.len() > 1 { self.color(rx, i) } else { th.text };
            let mut section = Vec::new();
            self.section(rx, i, color, &mut section);
            rows.push(Node::column(section).gap(2.0).padding(if n > 0 { 6.0 } else { 0.0 }, 0.0, 6.0, 0.0).key(format!("account-{i}")));
        }
        Some(rx.popup(260.0, rows).padding(4.0, 4.0, 4.0, 4.0).gap(4.0))
    }

    fn ready(&self) -> bool {
        self.data[0].is_some()
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
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
            Event::PopupOpened => match self.focus {
                Some(i) => self.refresh(i, true, cx),
                None => self.refresh_all(true, cx),
            },
            Event::PopupClosed => {
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
        assert_eq!(plan_name("max"), "Claude Max");
        assert_eq!(plan_name(""), "");
    }
}
