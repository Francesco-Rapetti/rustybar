//! Home Assistant: the devices of the house, live, and a dashboard of tiles to control them.
//!
//! - Bar: the Home Assistant logo in its blue with how many devices of the popup are on (lights,
//!   switches, fans, the climate in any mode but off, the vacuum while it cleans), gray and without
//!   the count while Home Assistant can't be reached. The logo is the server's own mask icon
//!   (`/static/icons/mask-icon.svg`), saved once in `~/Library/Caches/rustybar`: it's a trademark,
//!   so it isn't in the repo, and until then there's a house. While the 3D printer prints, a pill on
//!   its left has a printer and the progress, yellow while paused.
//! - Popup: the groups of `groups`, each a title and its tiles, two in a row; a tile is the icon,
//!   the name (Home Assistant's, or the tile's own) and the state, in the color of the state.
//!   - Light: a bulb, yellow when on; `Accesa` or `Spenta`. A click toggles it; dimmable ones
//!     have a slider with the brightness, set when released (0: off).
//!   - Switch: a plug, or the tile's `icon`; fan and input_boolean alike. A click toggles it.
//!   - Cover: an open or closed blind; `Aperta`, `Chiusa`, `Si apre…`, `Si chiude…`. A click
//!     toggles it, a slider with the position sets it.
//!   - Climate, two tiles wide: the icon of its mode (a flame peach for heat, a snowflake sky for
//!     cool, a drop teal for dry, a fan teal, a thermometer otherwise), the mode and the room's
//!     temperature (`26,5°`); off, `Spento`. A click turns it on or off. While on, the modes as
//!     chips (a click switches, shown at once) and − target +, by its step within its range, sent
//!     a second after the last click and shown at once.
//!   - Vacuum: a robot, green while it cleans, red on error; `Pulisce`, `Torna alla base`, `Alla
//!     base`, `In pausa`, `Fermo`, `Errore`. A click sends it to clean, or back while it cleans.
//!   - The printer's plug (`printer.plug`), two tiles wide: while it prints `In stampa`, the
//!     progress and a bar, then `ancora 1 h 20 min` (`In pausa`, `Si prepara a stampare…`). A
//!     click turns it on; off only at a second click (`Fai clic per spegnere` in red, then
//!     `Spegnimento…`), through `printer.off`, a script that should do nothing while the printer
//!     works; while it prints a click only says `In stampa` in red.
//!   - Scripts and scenes: chips after the tiles of their group; a script shows an hourglass while
//!     it runs.
//!   - Unavailable: gray, `Non disponibile`, no actions. A service that fails: `Non riuscito` in
//!     red, until the entity changes.
//!   - Last, `Apri Home Assistant`, which opens `dashboard`. Unreachable: only a message and it.
//! - Data: the WebSocket API, `subscribe_entities` for the tiles, the printer's sensors and
//!   `watch`, with a ping every 30 s. A dropped connection is tried again after 1 s, then every
//!   10 s (gray only from the second failure, so a blip doesn't dim the bar), and at once on wake.
//!   Actions are `POST /api/services/<domain>/<service>`. The token, a long-lived access token, is
//!   in the login keychain as `rustybar-home-assistant` (`rustybar secret set home-assistant`, from
//!   stdin); SketchyBar's `sketchybar-home-assistant` is copied there the first time.
//! - Lua: `rb.home.state(id)`, `rb.home.online()`, `rb.home.call(domain, service, data)` and the
//!   event `home_assistant` when a state changes, for dashboards of your own; `watch` lists the
//!   entities they need besides the tiles.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{Cx, Event, Rx, Widget};
use crate::config::Options;
use crate::gfx::{Rgba, Weight};
use crate::i18n::t;
use crate::net::{self, Socket};
use crate::ui::{Align, Justify, Kind, Node};

const TILE: f64 = 150.0;
const GAP: f64 = 8.0;
const WIDE: f64 = TILE * 2.0 + GAP;
const PING: f64 = 30.0;
const RETRY: f64 = 10.0;
/// How long a value set from the popup shows before Home Assistant has it.
const PENDING: Duration = Duration::from_secs(15);
const PRINTING: [&str; 4] = ["running", "pause", "prepare", "slicing"];

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Entity {
    pub state: String,
    pub attributes: Map<String, Value>,
}

impl Entity {
    fn num(&self, key: &str) -> Option<f64> {
        self.attributes.get(key).and_then(Value::as_f64)
    }

    fn name(&self) -> Option<&str> {
        self.attributes.get("friendly_name").and_then(Value::as_str)
    }
}

/// An entity of a group: its id, or a table with a name and an SF Symbol of its own.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum TileDef {
    Id(String),
    Full {
        id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        icon: Option<String>,
    },
}

impl TileDef {
    fn id(&self) -> &str {
        match self {
            TileDef::Id(id) | TileDef::Full { id, .. } => id,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Group {
    title: String,
    entities: Vec<TileDef>,
}

/// A 3D printer, e.g. of the Bambu Lab integration: its plug among the tiles, the script that
/// turns it off, and the sensors of its status, progress (%) and time left (hours).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Printer {
    plug: String,
    off: String,
    status: String,
    progress: String,
    remaining: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Setup {
    server: String,
    /// A path on the server or a URL.
    dashboard: String,
    groups: Vec<Group>,
    printer: Printer,
    watch: Vec<String>,
}

/// Made-up data (see `mock`): the states, and the groups when the config has none.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    online: bool,
    #[serde(flatten)]
    setup: Setup,
    states: HashMap<String, Entity>,
}

/// What a tile says for a moment, over its state.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Note {
    Failed,
    ConfirmOff,
    TurningOff,
    Printing,
    Running,
}

/// What the Lua functions see: the widget keeps it current.
#[derive(Default)]
struct Shared {
    online: bool,
    states: HashMap<String, Entity>,
    base: String,
    token: Option<String>,
    mock: bool,
}

thread_local! {
    static SHARED: RefCell<Shared> = RefCell::new(Shared::default());
}

pub struct Home {
    setup: Setup,
    /// None until the first connection or failure.
    online: Option<bool>,
    no_token: bool,
    states: HashMap<String, Entity>,
    token: Option<String>,
    socket: Option<Socket>,
    /// The connection the callbacks belong to: those of a replaced one are ignored.
    conn: u64,
    next_id: u64,
    awaiting_pong: bool,
    /// In a row.
    failures: u32,
    notes: HashMap<String, Note>,
    /// Brightness, position or target temperature set from the popup, until Home Assistant has it.
    pending: HashMap<String, (f64, Instant)>,
    pending_mode: HashMap<String, String>,
    /// Clicks on − and +: the last one sends the target.
    nudges: u64,
    logo: Option<String>,
    mock: bool,
}

fn domain(id: &str) -> &str {
    id.split('.').next().unwrap_or("")
}

fn logo_path() -> PathBuf {
    crate::config::home().join("Library/Caches/rustybar/home-assistant.svg")
}

/// `26,5°` in Italian, `26.5°` in English; whole degrees without decimals.
fn degrees(v: f64) -> String {
    let r = (v * 10.0).round() / 10.0;
    let s = if r.fract() == 0.0 { format!("{r:.0}") } else { format!("{r:.1}") };
    format!("{}°", if crate::i18n::italian() { s.replace('.', ",") } else { s })
}

/// The look of a climate mode: symbol, color (by palette name) and name.
fn mode_look(mode: &str) -> (&'static str, &'static str, String) {
    let (symbol, color, name) = match mode {
        "off" => ("thermometer.medium", "subtext0", t("Spento", "Off")),
        "heat" => ("flame.fill", "peach", t("Caldo", "Heat")),
        "cool" => ("snowflake", "sky", t("Freddo", "Cool")),
        "dry" => ("humidity.fill", "teal", t("Deumidifica", "Dry")),
        "fan_only" => ("fan.fill", "teal", t("Ventola", "Fan")),
        "heat_cool" => ("thermometer.medium", "text", t("Caldo/freddo", "Heat/Cool")),
        "auto" => ("a.circle", "text", t("Automatico", "Auto")),
        other => return ("thermometer.medium", "text", other.to_string()),
    };
    (symbol, color, name.to_string())
}

/// The token from the keychain, or SketchyBar's, copied to RustyBar's item.
fn read_token() -> Option<String> {
    if let Some(token) = crate::secret::get("rustybar-home-assistant", None) {
        return Some(token);
    }
    let token = crate::secret::get("sketchybar-home-assistant", None)?;
    let user = std::env::var("USER").unwrap_or_else(|_| "rustybar".into());
    crate::secret::set("rustybar-home-assistant", &user, &token);
    Some(token)
}

fn post(base: &str, token: &str, domain: &str, service: &str, data: &Value, done: impl Fn(bool) + Send + Sync + 'static) {
    let url = format!("{base}/api/services/{domain}/{service}");
    let auth = format!("Bearer {token}");
    let body = data.to_string();
    let what = format!("{domain}.{service} {body}");
    net::request("POST", &url, &[("Authorization", &auth), ("Content-Type", "application/json")], Some(body.as_bytes()), 10.0, move |status, reply| {
        // The tile only says it failed: why goes in the log
        if status != 200 {
            let reply = String::from_utf8_lossy(&reply);
            crate::log(&format!("home: {what} → {status} {}", reply.chars().take(300).collect::<String>()));
        }
        done(status == 200)
    });
}

impl Home {
    pub fn new(options: &Options) -> Home {
        let mut setup: Setup = toml::Value::Table(options.0.clone()).try_into().unwrap_or_else(|e| {
            crate::log(&format!("home: {e}"));
            Setup::default()
        });
        let mock = crate::mock::load::<Mock>("home");
        let (mut online, mut states) = (None, HashMap::new());
        if let Some(m) = mock {
            if setup.groups.is_empty() {
                setup.groups = m.setup.groups;
                setup.printer = m.setup.printer;
            }
            (online, states) = (Some(m.online), m.states);
        }
        let mock = online.is_some();
        Home {
            setup,
            online,
            no_token: false,
            states,
            token: None,
            socket: None,
            conn: 0,
            next_id: 1,
            awaiting_pong: false,
            failures: 0,
            notes: HashMap::new(),
            pending: HashMap::new(),
            pending_mode: HashMap::new(),
            nudges: 0,
            logo: logo_path().exists().then(|| logo_path().display().to_string()),
            mock,
        }
    }

    fn base(&self) -> String {
        self.setup.server.trim().trim_end_matches('/').to_string()
    }

    fn is_online(&self) -> bool {
        self.online == Some(true)
    }

    /// The tiles, the printer's sensors and `watch`, once each.
    fn entities(&self) -> Vec<String> {
        let p = &self.setup.printer;
        let tiles = self.setup.groups.iter().flat_map(|g| g.entities.iter().map(|e| e.id().to_string()));
        let mut ids: Vec<String> = Vec::new();
        for id in tiles.chain([&p.status, &p.progress, &p.remaining].into_iter().cloned()).chain(self.setup.watch.iter().cloned()) {
            if !id.is_empty() && !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    fn share(&self) {
        SHARED.with(|s| {
            let mut s = s.borrow_mut();
            s.online = self.is_online();
            s.states = self.states.clone();
            s.base = self.base();
            s.token = self.token.clone();
            s.mock = self.mock;
        });
    }

    // The connection

    fn connect(&mut self, cx: &mut Cx) {
        self.conn += 1;
        self.socket = None;
        self.awaiting_pong = false;
        let conn = self.conn;
        if self.token.is_none() {
            cx.background::<Home, Option<String>>(read_token, move |w, token, cx| {
                if w.conn != conn {
                    return;
                }
                w.no_token = token.is_none();
                match token {
                    Some(token) => {
                        w.token = Some(token);
                        w.connect(cx);
                    }
                    None => {
                        crate::log("home: no token in the keychain: pbpaste | rustybar secret set home-assistant");
                        w.online = Some(false);
                        w.share();
                        cx.redraw();
                    }
                }
            });
            return;
        }
        let base = self.base();
        let url = match base.split_once("://") {
            Some(("https", rest)) => format!("wss://{rest}/api/websocket"),
            Some((_, rest)) => format!("ws://{rest}/api/websocket"),
            None => format!("ws://{base}/api/websocket"),
        };
        let handle = cx.handle::<Home>();
        self.socket = Socket::open(&url, move |text| {
            // Parsed here, off the main thread: the first message has every entity
            let message = text.map(|t| serde_json::from_str::<Value>(&t).unwrap_or(Value::Null));
            handle.send(move |w, cx| w.received(conn, message, cx));
        });
        if self.socket.is_none() {
            crate::log(&format!("home: not a server: {}", self.setup.server));
            self.online = Some(false);
            cx.redraw();
        }
    }

    fn send(&mut self, mut message: Value) {
        if message.get("type").is_some_and(|t| t != "auth") {
            message["id"] = json!(self.next_id);
            self.next_id += 1;
        }
        if let Some(socket) = &self.socket {
            socket.send(&message.to_string());
        }
    }

    fn received(&mut self, conn: u64, message: Option<Value>, cx: &mut Cx) {
        if conn != self.conn {
            return;
        }
        let Some(message) = message else { return self.disconnected(cx) };
        match message["type"].as_str() {
            Some("auth_required") => {
                let token = self.token.clone().unwrap_or_default();
                self.send(json!({"type": "auth", "access_token": token}));
            }
            Some("auth_ok") => {
                self.next_id = 1;
                let ids = self.entities();
                self.send(json!({"type": "subscribe_entities", "entity_ids": ids}));
                self.fetch_logo(cx);
            }
            // A wrong token: tried again on wake, or when a new one is saved
            Some("auth_invalid") => {
                crate::log("home: Home Assistant refused the token: pbpaste | rustybar secret set home-assistant");
                self.conn += 1;
                self.socket = None;
                self.token = None;
                self.online = Some(false);
                self.share();
                cx.redraw();
            }
            Some("result") if message["success"] == json!(false) => self.disconnected(cx),
            Some("event") => {
                let changed = self.apply(&message["event"]);
                let first = !self.is_online();
                self.online = Some(true);
                self.no_token = false;
                self.failures = 0;
                if first || !changed.is_empty() {
                    self.settle(&changed);
                    self.share();
                    cx.redraw();
                    crate::runtime::after(0.0, |app| app.broadcast(&Event::Trigger { name: "home_assistant".into(), args: HashMap::new() }));
                }
            }
            _ => {}
        }
    }

    /// The compressed states of `subscribe_entities`: `a` the entities added (all of them in the
    /// first event) with their state `s` and attributes `a`; `c` what changed, `+` the new state
    /// or attributes and `-` the attributes removed; `r` the entities removed. The ids that changed.
    fn apply(&mut self, event: &Value) -> Vec<String> {
        let mut changed = Vec::new();
        for (id, v) in event["a"].as_object().into_iter().flatten() {
            let e = Entity { state: v["s"].as_str().unwrap_or_default().to_string(), attributes: v["a"].as_object().cloned().unwrap_or_default() };
            if self.states.get(id) != Some(&e) {
                self.states.insert(id.clone(), e);
                changed.push(id.clone());
            }
        }
        for (id, change) in event["c"].as_object().into_iter().flatten() {
            let Some(e) = self.states.get_mut(id) else { continue };
            let before = e.clone();
            if let Some(added) = change.get("+") {
                if let Some(s) = added["s"].as_str() {
                    e.state = s.to_string();
                }
                for (k, v) in added["a"].as_object().into_iter().flatten() {
                    e.attributes.insert(k.clone(), v.clone());
                }
            }
            for k in change.get("-").and_then(|m| m["a"].as_array()).into_iter().flatten().filter_map(Value::as_str) {
                e.attributes.remove(k);
            }
            if *e != before {
                changed.push(id.clone());
            }
        }
        for id in event["r"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if self.states.remove(id).is_some() {
                changed.push(id.to_string());
            }
        }
        changed
    }

    /// What changed ends the notes on it, and the values set from the popup once it has them.
    fn settle(&mut self, changed: &[String]) {
        for id in changed {
            self.notes.remove(id);
            self.pending_mode.remove(id);
            if let Some(&(value, _)) = self.pending.get(id)
                && self.actual(id).is_some_and(|a| (a - value).abs() < 0.6)
            {
                self.pending.remove(id);
            }
        }
    }

    fn disconnected(&mut self, cx: &mut Cx) {
        self.conn += 1;
        self.socket = None;
        self.failures += 1;
        if self.failures > 1 && self.online != Some(false) {
            self.online = Some(false);
            self.share();
            cx.redraw();
        }
        let conn = self.conn;
        let delay = if self.failures > 1 { RETRY } else { 1.0 };
        cx.after::<Home>(delay, move |w, cx| {
            if w.conn == conn {
                w.connect(cx);
            }
        });
    }

    /// A ping without a pong by the next one means the connection is gone, e.g. after sleep.
    fn ping(&mut self, cx: &mut Cx) {
        let Some(socket) = &self.socket else { return };
        if self.awaiting_pong {
            return self.disconnected(cx);
        }
        self.awaiting_pong = true;
        let (conn, handle) = (self.conn, cx.handle::<Home>());
        socket.ping(move |ok| {
            handle.send(move |w, cx| {
                if w.conn != conn {
                    return;
                }
                if ok {
                    w.awaiting_pong = false;
                } else {
                    w.disconnected(cx);
                }
            })
        });
    }

    /// The server's logo, once.
    fn fetch_logo(&mut self, cx: &mut Cx) {
        if self.logo.is_some() {
            return;
        }
        let handle = cx.handle::<Home>();
        net::request("GET", &format!("{}/static/icons/mask-icon.svg", self.base()), &[], None, 10.0, move |status, body| {
            let path = logo_path();
            if status != 200 || !String::from_utf8_lossy(&body).contains("<svg") {
                return;
            }
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
            if std::fs::write(&path, &body).is_ok() {
                let path = path.display().to_string();
                handle.send(move |w, cx| {
                    w.logo = Some(path);
                    cx.redraw();
                });
            }
        });
    }

    // Actions

    fn service(&mut self, id: &str, domain: &str, service: &str, data: Value, cx: &mut Cx) {
        let Some(token) = self.token.clone().filter(|_| !self.mock) else { return };
        let (handle, id) = (cx.handle::<Home>(), id.to_string());
        post(&self.base(), &token, domain, service, &data, move |ok| {
            if ok {
                return;
            }
            let id = id.clone();
            handle.send(move |w, cx| {
                w.pending.remove(&id);
                w.pending_mode.remove(&id);
                w.notes.insert(id, Note::Failed);
                cx.redraw();
            });
        });
    }

    /// A note that goes by itself after `secs`, unless replaced.
    fn note_for(&mut self, id: &str, note: Note, secs: f64, cx: &mut Cx) {
        self.notes.insert(id.to_string(), note);
        let id = id.to_string();
        cx.after::<Home>(secs, move |w, cx| {
            if w.notes.get(&id) == Some(&note) {
                w.notes.remove(&id);
                cx.redraw();
            }
        });
    }

    fn printing(&self) -> Option<&str> {
        let status = self.states.get(&self.setup.printer.status)?.state.as_str();
        PRINTING.contains(&status).then_some(status)
    }

    /// A click on a tile or a chip.
    fn tap(&mut self, id: &str, cx: &mut Cx) {
        let Some(state) = self.states.get(id).map(|e| e.state.clone()) else { return };
        if !self.is_online() || state == "unavailable" {
            return;
        }
        let data = json!({"entity_id": id});
        if id == self.setup.printer.plug && state == "on" {
            if self.printing().is_some() {
                self.note_for(id, Note::Printing, 2.0, cx);
            } else if self.notes.get(id) == Some(&Note::ConfirmOff) {
                self.notes.insert(id.to_string(), Note::TurningOff);
                match self.setup.printer.off.clone() {
                    off if off.is_empty() => self.service(id, domain(id), "turn_off", data, cx),
                    off => self.service(id, "script", "turn_on", json!({"entity_id": off}), cx),
                }
            } else {
                self.note_for(id, Note::ConfirmOff, 4.0, cx);
            }
            return;
        }
        let d = domain(id).to_string();
        let service = match (d.as_str(), state.as_str()) {
            ("climate", "off") => "turn_on",
            ("climate", _) => "turn_off",
            ("vacuum", "cleaning") => "return_to_base",
            ("vacuum", _) => "start",
            ("script" | "scene", _) => {
                if d == "script" {
                    self.note_for(id, Note::Running, 10.0, cx);
                }
                "turn_on"
            }
            _ => "toggle",
        };
        self.service(id, &d, service, data, cx);
    }

    /// A slider released at `v` (0..1): a light's brightness, a blind's position.
    fn slide(&mut self, id: &str, v: f64, cx: &mut Cx) {
        if !self.is_online() {
            return;
        }
        let pct = (v * 100.0).round().clamp(0.0, 100.0);
        self.pending.insert(id.to_string(), (pct, Instant::now() + PENDING));
        self.expire(cx);
        match domain(id) {
            "light" if pct > 0.0 => self.service(id, "light", "turn_on", json!({"entity_id": id, "brightness_pct": pct}), cx),
            "light" => self.service(id, "light", "turn_off", json!({"entity_id": id}), cx),
            "cover" => self.service(id, "cover", "set_cover_position", json!({"entity_id": id, "position": pct}), cx),
            _ => {}
        }
    }

    /// − or + on a climate: the target moves at once, and goes to Home Assistant a second after
    /// the last click, so that an air conditioner gets one change.
    fn nudge(&mut self, id: &str, up: bool, cx: &mut Cx) {
        let Some(e) = self.states.get(id) else { return };
        let step = e.num("target_temp_step").filter(|s| *s > 0.0).unwrap_or(0.5);
        let (min, max) = (e.num("min_temp").unwrap_or(7.0), e.num("max_temp").unwrap_or(35.0));
        let Some(current) = self.shown(id) else { return };
        let target = (((current + if up { step } else { -step }) / step).round() * step).clamp(min, max);
        self.pending.insert(id.to_string(), (target, Instant::now() + PENDING));
        self.expire(cx);
        self.nudges += 1;
        let (seq, id) = (self.nudges, id.to_string());
        cx.after::<Home>(1.0, move |w, cx| {
            if w.nudges == seq {
                w.service(&id, "climate", "set_temperature", json!({"entity_id": id, "temperature": target}), cx);
            }
        });
    }

    fn set_mode(&mut self, id: &str, mode: &str, cx: &mut Cx) {
        if self.states.get(id).is_none_or(|e| e.state == mode) {
            return;
        }
        self.pending_mode.insert(id.to_string(), mode.to_string());
        self.service(id, "climate", "set_hvac_mode", json!({"entity_id": id, "hvac_mode": mode}), cx);
    }

    /// Redraws when the values set from the popup expire, if Home Assistant never had them.
    fn expire(&self, cx: &mut Cx) {
        cx.after::<Home>(PENDING.as_secs_f64() + 0.1, |w, cx| {
            let now = Instant::now();
            w.pending.retain(|_, (_, until)| *until > now);
            cx.redraw();
        });
    }

    /// The value that a pending one stands for: brightness %, position, target temperature.
    fn actual(&self, id: &str) -> Option<f64> {
        let e = self.states.get(id)?;
        match domain(id) {
            "light" => Some(if e.state == "on" { (e.num("brightness").unwrap_or(0.0) / 2.55).round() } else { 0.0 }),
            "cover" => e.num("current_position"),
            "climate" => e.num("temperature"),
            _ => None,
        }
    }

    fn shown(&self, id: &str) -> Option<f64> {
        match self.pending.get(id) {
            Some(&(v, until)) if until > Instant::now() => Some(v),
            _ => self.actual(id),
        }
    }

    /// How many devices are on, for the bar.
    fn on_count(&self) -> usize {
        let ids = self.setup.groups.iter().flat_map(|g| g.entities.iter().map(TileDef::id));
        ids.filter(|id| {
            let Some(e) = self.states.get(*id) else { return false };
            match domain(id) {
                "light" | "switch" | "fan" | "input_boolean" => e.state == "on",
                "climate" => !["off", "unavailable", "unknown"].contains(&e.state.as_str()),
                "vacuum" => e.state == "cleaning",
                _ => false,
            }
        })
        .count()
    }

    // Drawing

    fn printer_progress(&self) -> (f64, Option<f64>) {
        let num = |id: &str| self.states.get(id).and_then(|e| e.state.parse::<f64>().ok());
        (num(&self.setup.printer.progress).unwrap_or(0.0).clamp(0.0, 100.0), num(&self.setup.printer.remaining).map(|h| (h * 60.0).round()))
    }

    /// The tile of a light, switch, cover or vacuum.
    fn tile(&self, rx: &Rx, def: &TileDef) -> Node {
        let th = rx.theme;
        let id = def.id();
        let (name, icon) = match def {
            TileDef::Id(_) => (None, None),
            TileDef::Full { name, icon, .. } => (name.clone(), icon.clone()),
        };
        let e = self.states.get(id);
        let state = e.map(|e| e.state.as_str()).unwrap_or("unavailable");
        let name = name.or_else(|| e.and_then(|e| e.name()).map(String::from)).unwrap_or_else(|| id.to_string());
        let on = state == "on";
        let (masculine, mut symbol) = match domain(id) {
            "light" => (false, "lightbulb.fill".to_string()),
            "fan" => (true, "fan.fill".into()),
            "input_boolean" => (true, "togglepower".into()),
            "cover" => (false, if state == "closed" { "roller.shade.closed" } else { "roller.shade.open" }.into()),
            "vacuum" => (true, "robotic.vacuum.fill".into()),
            _ => (false, "powerplug.fill".into()),
        };
        if let Some(i) = icon {
            symbol = i;
        }
        // A bulb is lit only when on
        if symbol.trim_end_matches(".fill") == "lightbulb" {
            symbol = if on { "lightbulb.fill" } else { "lightbulb" }.into();
        }
        let (on_text, off_text) = if masculine { (t("Acceso", "On"), t("Spento", "Off")) } else { (t("Accesa", "On"), t("Spenta", "Off")) };
        let dimmable = e.and_then(|e| e.attributes.get("supported_color_modes")).and_then(Value::as_array).is_some_and(|m| m.iter().any(|m| m != "onoff"));
        let mut slider = None;
        let (color, text, text_color, lit) = match domain(id) {
            "cover" => {
                // The position is in the slider
                slider = Some(self.shown(id).unwrap_or(0.0) / 100.0);
                match state {
                    "opening" => (th.text, t("Si apre…", "Opening…").to_string(), th.text, true),
                    "closing" => (th.text, t("Si chiude…", "Closing…").to_string(), th.text, true),
                    "closed" => (th.subtext, t("Chiusa", "Closed").to_string(), th.subtext, false),
                    _ => (th.text, t("Aperta", "Open").to_string(), th.text, true),
                }
            }
            "vacuum" => match state {
                "cleaning" => (th.green, t("Pulisce", "Cleaning").to_string(), th.text, true),
                "returning" => (th.text, t("Torna alla base", "Returning").to_string(), th.text, true),
                "docked" => (th.subtext, t("Alla base", "Docked").to_string(), th.subtext, false),
                "paused" => (th.subtext, t("In pausa", "Paused").to_string(), th.subtext, false),
                "error" => (th.red, t("Errore", "Error").to_string(), th.red, false),
                _ => (th.subtext, t("Fermo", "Idle").to_string(), th.subtext, false),
            },
            _ => {
                // The brightness is in the slider
                if domain(id) == "light" && dimmable {
                    slider = Some(self.shown(id).unwrap_or(0.0) / 100.0);
                }
                if on { (th.yellow, on_text.to_string(), th.text, true) } else { (th.subtext, off_text.to_string(), th.subtext, false) }
            }
        };
        let unavailable = state == "unavailable" || !self.is_online();
        let (color, text, text_color, lit) = match self.notes.get(id) {
            _ if unavailable => (th.overlay, t("Non disponibile", "Unavailable").to_string(), th.overlay, false),
            Some(Note::Failed) => (color, t("Non riuscito", "Failed").to_string(), th.red, lit),
            _ => (color, text, text_color, lit),
        };
        let mut children = vec![
            Node::row(vec![Node::symbol(symbol, 18.0, Weight::Semibold, color)]).height(22.0).key("icon"),
            Node::text(name, th.font(), if unavailable { th.overlay } else { th.text }).key("name"),
            Node::text(text, th.small_font(), text_color).key("state"),
        ];
        if let Some(v) = slider.filter(|_| !unavailable) {
            children.push(rx.slider(v, rx.action(&format!("slider:{id}"))).height(22.0).padding(4.0, 0.0, 0.0, 0.0).key("slider"));
        }
        let mut tile = Node::column(children).gap(2.0).padding(10.0, 10.0, 10.0, 10.0).width(TILE).radius(10.0).background(th.tile(color, lit)).key(id);
        if !unavailable {
            tile = tile.click(rx.action(&format!("tap:{id}")));
        }
        tile
    }

    /// A climate, two tiles wide.
    fn climate(&self, rx: &Rx, def: &TileDef) -> Node {
        let th = rx.theme;
        let id = def.id();
        let e = self.states.get(id);
        let state = e.map(|e| e.state.clone()).unwrap_or_else(|| "unavailable".into());
        let mode = self.pending_mode.get(id).cloned().unwrap_or_else(|| state.clone());
        let name = custom_name(def).or_else(|| e.and_then(|e| e.name()).map(String::from)).unwrap_or_else(|| id.to_string());
        let unavailable = state == "unavailable" || !self.is_online();
        let (symbol, color, mode_name) = mode_look(&mode);
        let color = if unavailable { th.overlay } else { th.color(color).unwrap_or(th.text) };
        let current = e.and_then(|e| e.num("current_temperature")).map(degrees).unwrap_or_default();
        let (text, text_color) = match self.notes.get(id) {
            _ if unavailable => (t("Non disponibile", "Unavailable").to_string(), th.overlay),
            Some(Note::Failed) => (t("Non riuscito", "Failed").to_string(), th.red),
            _ if mode == "off" => (mode_name, th.subtext),
            _ => (mode_name, th.text),
        };
        let on = !unavailable && mode != "off";
        let mut rows = vec![
            Node::row(vec![
                Node::symbol(symbol, 18.0, Weight::Semibold, color).key("icon"),
                Node::column(vec![Node::text(name, th.font(), if unavailable { th.overlay } else { th.text }).key("name"), Node::text(text, th.small_font(), text_color).key("state")]).gap(2.0).key("names"),
                Node::spacer(),
                Node::text(current, th.font_with(Weight::Medium, 15.0), if on { th.text } else { th.subtext }).key("current"),
            ])
            .gap(10.0)
            .key("top"),
        ];
        if on {
            let modes: Vec<String> = e
                .and_then(|e| e.attributes.get("hvac_modes"))
                .and_then(Value::as_array)
                .map(|m| m.iter().filter_map(Value::as_str).filter(|m| *m != "off").map(String::from).collect())
                .unwrap_or_default();
            let mut row: Vec<Node> = modes
                .iter()
                .take(6)
                .map(|m| {
                    let (symbol, c, _) = mode_look(m);
                    let c = th.color(c).unwrap_or(th.text);
                    let selected = *m == mode;
                    Node::row(vec![Node::symbol(symbol, 12.0, Weight::Semibold, if selected { c } else { th.subtext })])
                        .size(30.0, 24.0)
                        .justify(Justify::Center)
                        .radius(6.0)
                        .background(if selected { c.with_alpha(0.22) } else { th.surface.with_alpha(0.7) })
                        .click(rx.action(&format!("mode:{id}:{m}")))
                        .key(format!("mode-{m}"))
                })
                .collect();
            row.push(Node::spacer());
            let button = |symbol: &str, action: String| {
                Node::row(vec![Node::symbol(symbol, 11.0, Weight::Bold, th.text)]).size(26.0, 24.0).justify(Justify::Center).radius(6.0).background(th.surface.with_alpha(0.7)).click(rx.action(&action)).key(action)
            };
            row.push(button("minus", format!("temp:{id}:-")));
            let target = self.shown(id).map(degrees).unwrap_or_default();
            row.push(Node::row(vec![Node::text(target, th.font(), th.text)]).min_width(46.0).justify(Justify::Center).key("target"));
            row.push(button("plus", format!("temp:{id}:+")));
            rows.push(Node::row(row).gap(4.0).padding(6.0, 0.0, 0.0, 0.0).key("controls"));
        }
        let mut tile = Node::column(rows).gap(4.0).padding(10.0, 10.0, 10.0, 10.0).width(WIDE).radius(10.0).background(th.tile(color, on)).key(id);
        if !unavailable {
            tile = tile.click(rx.action(&format!("tap:{id}")));
        }
        tile
    }

    /// The printer's plug, two tiles wide, with the progress while it prints.
    fn printer(&self, rx: &Rx, def: &TileDef) -> Node {
        let th = rx.theme;
        let id = def.id();
        let e = self.states.get(id);
        let state = e.map(|e| e.state.as_str()).unwrap_or("unavailable");
        let name = custom_name(def).or_else(|| e.and_then(|e| e.name()).map(String::from)).unwrap_or_else(|| id.to_string());
        let unavailable = state == "unavailable" || !self.is_online();
        let on = state == "on";
        let printing = self.printing().filter(|_| on && !unavailable);
        let (progress, minutes) = self.printer_progress();
        let color = if unavailable { th.overlay } else if on { th.yellow } else { th.subtext };
        let (text, text_color) = match self.notes.get(id) {
            _ if unavailable => (t("Non disponibile", "Unavailable"), th.overlay),
            Some(Note::Failed) => (t("Non riuscito", "Failed"), th.red),
            Some(Note::ConfirmOff) => (t("Fai clic per spegnere", "Click to turn off"), th.red),
            Some(Note::TurningOff) => (t("Spegnimento…", "Turning off…"), th.yellow),
            Some(Note::Printing) => (t("In stampa", "Printing"), th.red),
            _ if printing.is_some() => (t("In stampa", "Printing"), th.text),
            _ if on => (t("Accesa", "On"), th.text),
            _ => (t("Spenta", "Off"), th.subtext),
        };
        let mut top = vec![
            Node::symbol("printer.fill", 18.0, Weight::Semibold, color).key("icon"),
            Node::column(vec![Node::text(name, th.font(), if unavailable { th.overlay } else { th.text }).key("name"), Node::text(text, th.small_font(), text_color).key("state")]).gap(2.0).key("names"),
            Node::spacer(),
        ];
        let mut rows = Vec::new();
        if let Some(status) = printing {
            let paused = status == "pause";
            top.push(Node::text(format!("{progress:.0}%"), th.font_with(Weight::Medium, 15.0), if paused { th.yellow } else { th.text }).key("percent"));
            let detail = match status {
                "prepare" | "slicing" => t("Si prepara a stampare…", "Getting ready to print…").to_string(),
                "pause" => t("In pausa", "Paused").to_string(),
                _ => match minutes {
                    Some(m) => t(&format!("ancora {}", crate::time::span(m * 60.0)), &format!("{} left", crate::time::span(m * 60.0))).to_string(),
                    None => String::new(),
                },
            };
            rows.push(Node::new(Kind::Progress { value: progress / 100.0, color: if paused { th.yellow } else { th.primary }, track: th.surface }).height(6.0).key("bar"));
            rows.push(Node::text(detail, th.small_font(), th.subtext).key("detail"));
        }
        let mut children = vec![Node::row(top).gap(10.0).key("top")];
        if !rows.is_empty() {
            children.push(Node::column(rows).gap(6.0).padding(6.0, 0.0, 0.0, 0.0).key("progress"));
        }
        let mut tile = Node::column(children).gap(4.0).padding(10.0, 10.0, 10.0, 10.0).width(WIDE).radius(10.0).background(th.tile(color, on && !unavailable)).key(id);
        if !unavailable {
            tile = tile.click(rx.action(&format!("tap:{id}")));
        }
        tile
    }

    /// A script or a scene.
    fn chip(&self, rx: &Rx, def: &TileDef) -> (Node, f64) {
        let th = rx.theme;
        let id = def.id();
        let e = self.states.get(id);
        let name = custom_name(def).or_else(|| e.and_then(|e| e.name()).map(String::from)).unwrap_or_else(|| id.to_string());
        let unavailable = e.is_none_or(|e| e.state == "unavailable") || !self.is_online();
        let running = e.is_some_and(|e| e.state == "on") || self.notes.get(id) == Some(&Note::Running);
        let (symbol, color) = match self.notes.get(id) {
            _ if unavailable => ("play.fill", th.overlay),
            Some(Note::Failed) => ("exclamationmark.circle.fill", th.red),
            _ if running => ("hourglass", th.yellow),
            _ => ("play.fill", th.primary),
        };
        let font = th.font_with(Weight::Bold, 12.0);
        let width = crate::gfx::measure_text(&name, &font).width + 12.0 + 6.0 + 22.0;
        let mut chip = Node::row(vec![Node::symbol(symbol, 10.0, Weight::Bold, color).key("icon"), Node::text(name, font, if unavailable { th.overlay } else { th.text }).key("name")])
            .gap(6.0)
            .padding(6.0, 12.0, 6.0, 10.0)
            .radius(14.0)
            .background(th.surface.with_alpha(0.7))
            .key(id);
        if !unavailable {
            chip = chip.click(rx.action(&format!("tap:{id}")));
        }
        (chip, width.min(WIDE))
    }
}

fn custom_name(def: &TileDef) -> Option<String> {
    match def {
        TileDef::Full { name, .. } => name.clone(),
        TileDef::Id(_) => None,
    }
}

impl Widget for Home {
    fn start(&mut self, cx: &mut Cx) {
        self.share();
        if self.mock || self.setup.server.trim().is_empty() {
            return;
        }
        self.connect(cx);
        cx.every::<Home>(PING, |w, cx| w.ping(cx));
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        if self.setup.server.trim().is_empty() && !self.mock {
            return None;
        }
        let th = rx.theme;
        let online = self.is_online();
        let brand = Rgba::parse("#18bcf2").unwrap_or(th.primary);
        let icon = match &self.logo {
            Some(path) => Node::image(path, 0.0, 15.0, Some(if online { brand } else { th.overlay })),
            None => Node::symbol("house.fill", th.icon_size, Weight::Semibold, if online { th.text } else { th.overlay }),
        };
        let mut home = vec![icon.key("logo")];
        let count = self.on_count();
        if online && count > 0 {
            home.push(Node::text(count.to_string(), th.font(), th.text).key("count"));
        }
        let mut pills = Vec::new();
        if let Some(status) = self.printing().filter(|_| online) {
            let color = if status == "pause" { th.yellow } else { th.text };
            let (progress, _) = self.printer_progress();
            pills.push(rx.pill(vec![Node::symbol("printer.fill", th.icon_size, Weight::Semibold, color).key("icon"), Node::text(format!("{progress:.0}%"), th.font(), color).key("percent")]).key("printer").click(rx.toggle_popup()));
        }
        pills.push(rx.pill(home).key("home").click(rx.toggle_popup()));
        Some(Node::row(pills).gap(th.item_gap).height(th.pill_height).key(rx.name))
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let mut rows: Vec<Node> = Vec::new();
        let message = |text: &str, key: &str| Node::row(vec![Node::text(text, th.font(), th.subtext)]).padding(6.0, 10.0, 6.0, 10.0).key(key);
        match self.online {
            Some(true) => {
                for (g, group) in self.setup.groups.iter().enumerate() {
                    if !group.title.is_empty() {
                        rows.push(Node::row(vec![Node::text(&group.title, th.font(), th.text)]).padding(if rows.is_empty() { 2.0 } else { 10.0 }, 10.0, 0.0, 10.0).key(format!("title-{g}")));
                    }
                    let mut pair: Vec<Node> = Vec::new();
                    let mut chips: Vec<(Node, f64)> = Vec::new();
                    let flush = |pair: &mut Vec<Node>, rows: &mut Vec<Node>| {
                        if !pair.is_empty() {
                            let key = format!("row-{}", pair[0].key.clone().unwrap_or_default());
                            rows.push(Node::row(std::mem::take(pair)).gap(GAP).align(Align::Stretch).key(key));
                        }
                    };
                    for def in &group.entities {
                        let id = def.id();
                        match domain(id) {
                            "script" | "scene" => chips.push(self.chip(rx, def)),
                            "climate" => {
                                flush(&mut pair, &mut rows);
                                rows.push(self.climate(rx, def));
                            }
                            _ if id == self.setup.printer.plug => {
                                flush(&mut pair, &mut rows);
                                rows.push(self.printer(rx, def));
                            }
                            _ => {
                                pair.push(self.tile(rx, def));
                                if pair.len() == 2 {
                                    flush(&mut pair, &mut rows);
                                }
                            }
                        }
                    }
                    flush(&mut pair, &mut rows);
                    // The chips fill rows as wide as two tiles
                    let mut line: Vec<Node> = Vec::new();
                    let mut used = 0.0;
                    for (chip, w) in chips {
                        if !line.is_empty() && used + GAP + w > WIDE {
                            let key = format!("chips-{}", line[0].key.clone().unwrap_or_default());
                            rows.push(Node::row(std::mem::take(&mut line)).gap(GAP).key(key));
                            used = 0.0;
                        }
                        used += if line.is_empty() { w } else { GAP + w };
                        line.push(chip);
                    }
                    if !line.is_empty() {
                        let key = format!("chips-{}", line[0].key.clone().unwrap_or_default());
                        rows.push(Node::row(line).gap(GAP).key(key));
                    }
                }
            }
            Some(false) if self.no_token => {
                rows.push(message(t("Nessun token di Home Assistant", "No Home Assistant token"), "message"));
                rows.push(Node::text("pbpaste | rustybar secret set home-assistant", th.small_font(), th.subtext).padding(0.0, 10.0, 6.0, 10.0).key("hint"));
            }
            Some(false) => rows.push(message(t("Home Assistant non raggiungibile", "Home Assistant can't be reached"), "message")),
            None => rows.push(message(t("Connessione a Home Assistant…", "Connecting to Home Assistant…"), "message")),
        }
        rows.push(Node::default().height(6.0).key("gap"));
        rows.push(rx.row_button(vec![Node::text(t("Apri Home Assistant", "Open Home Assistant"), th.font(), th.primary)], vec![], rx.action("dashboard")).key("dashboard"));
        Some(rx.popup(WIDE, rows).gap(GAP).padding(8.0, 8.0, 6.0, 8.0))
    }

    fn ready(&self) -> bool {
        self.mock || self.setup.server.trim().is_empty() || self.online.is_some()
    }

    fn action(&mut self, name: &str, value: Option<f64>, cx: &mut Cx) {
        if name == "dashboard" {
            cx.close_popup();
            if !self.mock {
                let d = self.setup.dashboard.trim();
                let url = if d.contains("://") { d.to_string() } else { format!("{}/{}", self.base(), d.trim_start_matches('/')) };
                crate::runtime::open(&url);
            }
            return;
        }
        if self.mock {
            return;
        }
        let (kind, rest) = name.split_once(':').unwrap_or((name, ""));
        match kind {
            "tap" => self.tap(rest, cx),
            // Never without a value: 0 would turn the light off
            "slider" => {
                if let Some(v) = value {
                    self.slide(rest, v, cx);
                }
            }
            "temp" => {
                if let Some((id, dir)) = rest.rsplit_once(':') {
                    self.nudge(id, dir == "+", cx);
                }
            }
            "mode" => {
                if let Some((id, mode)) = rest.rsplit_once(':') {
                    self.set_mode(id, mode, cx);
                }
            }
            _ => {}
        }
        cx.redraw();
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        if self.mock || self.setup.server.trim().is_empty() {
            return;
        }
        match event {
            // After sleep the connection is gone, though nothing says so until the next ping
            Event::Wake => {
                self.failures = 0;
                self.connect(cx);
            }
            Event::Trigger { name, .. } if name == "home_assistant_token" => {
                self.token = None;
                self.failures = 0;
                self.connect(cx);
            }
            Event::PopupClosed => self.notes.retain(|_, n| !matches!(n, Note::ConfirmOff | Note::Printing)),
            _ => {}
        }
    }

    crate::any_widget!();
}

// For Lua

/// `rb.home.state(id)`: `{state, attributes, name}`, or None.
pub fn state(id: &str) -> Option<Value> {
    SHARED.with(|s| {
        let s = s.borrow();
        let e = s.states.get(id)?;
        Some(json!({"state": e.state, "attributes": e.attributes, "name": e.name().unwrap_or(id)}))
    })
}

pub fn online() -> bool {
    SHARED.with(|s| s.borrow().online)
}

/// `rb.home.call(domain, service, data)`: nothing with made-up data.
pub fn call(domain: &str, service: &str, data: Value) {
    let (base, token) = SHARED.with(|s| {
        let s = s.borrow();
        (s.base.clone(), s.token.clone().filter(|_| !s.mock))
    });
    if let Some(token) = token {
        let what = format!("{domain}.{service}");
        post(&base, &token, domain, service, &data, move |ok| {
            if !ok {
                crate::log(&format!("home: {what} failed"));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_states() {
        let mut h = Home::new(&Options::default());
        let changed = h.apply(&json!({"a": {"light.desk": {"s": "on", "a": {"brightness": 128, "friendly_name": "Desk"}}}}));
        assert_eq!(changed, vec!["light.desk"]);
        assert_eq!(h.actual("light.desk"), Some(50.0));
        let changed = h.apply(&json!({"c": {"light.desk": {"+": {"s": "off", "a": {"color_mode": null}}, "-": {"a": ["brightness"]}}}}));
        assert_eq!(changed, vec!["light.desk"]);
        let e = &h.states["light.desk"];
        assert_eq!(e.state, "off");
        assert!(!e.attributes.contains_key("brightness"));
        assert!(h.apply(&json!({"c": {"light.desk": {"+": {"s": "off"}}}})).is_empty());
        assert_eq!(h.apply(&json!({"r": ["light.desk"]})), vec!["light.desk"]);
        assert!(h.states.is_empty());
    }

    #[test]
    fn options() {
        let table: toml::Table = toml::from_str(
            r#"
            server = "http://ha.local:8123/"
            [[groups]]
            title = "Lights"
            entities = ["light.a", { id = "switch.b", name = "B", icon = "lightbulb" }]
            [printer]
            plug = "switch.p"
            status = "sensor.s"
            "#,
        )
        .unwrap();
        let h = Home::new(&Options(table));
        assert_eq!(h.base(), "http://ha.local:8123");
        assert_eq!(h.entities(), vec!["light.a", "switch.b", "sensor.s"]);
        assert_eq!(degrees(24.0).trim_end_matches('°'), "24");
        assert!(degrees(26.46).starts_with("26"));
    }
}
