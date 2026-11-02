//! Docker: the containers, from the Engine API on its Unix socket, with no `docker` CLI, and a
//! dashboard to start, stop, restart and remove them and to clean up what they leave behind.
//!
//! - Bar: Docker Desktop's whale in Docker's blue (a box without Docker Desktop) with how many
//!   containers run, paused ones too as in `docker ps`: gray when none, red when one fails its
//!   health check or keeps restarting. Hidden while Docker isn't running.
//! - Popup, a dashboard of tiles as `home`'s:
//!   - The engine, two tiles wide: the whale, `Docker Desktop` (`OrbStack`, `Docker`) and
//!     `Engine 29.0.1 · 8 CPU · 7,7 GB`; on the right the CPU and memory of all the containers.
//!   - The containers, all of them as in `docker ps -a`: those of each Compose project under its
//!     name, with how many run and ▶ ■ ↻ for the whole project (each container at once, without
//!     Compose's order), then the others under `Container`; two tiles in a row, by name, without
//!     the project's prefix. A tile has an icon in the color of the state (green running, yellow
//!     paused or starting its health check, red failing it, restarting or exited with an error,
//!     gray stopped), the CPU and memory it uses, the name, the image, then the published ports,
//!     links to `http://localhost:<port>`, and how long it's been up (`da 2 h`), or its state
//!     (`In pausa`, `Fermo`, `Uscito con codice 1`…). Last its buttons, dimmed where they'd do
//!     nothing: ▶ starts it (resumes it when paused), ■ stops it, ↻ restarts it and the bin removes
//!     it, running too, at a second click (`Fai clic per eliminare` in red). While a request is out
//!     the state says so (`Si ferma…`), and `Non riuscito` in red if it fails (Docker's reason goes
//!     in the log). When there are more than fit on the screen, they scroll.
//!   - `Spazio su disco`: images, containers, volumes and build cache, each with its size and what
//!     a clean-up frees; a click, then a second one (`Fai clic per pulire`), cleans it up as
//!     `docker image prune -a`, `container prune`, `volume prune` (anonymous volumes only: named
//!     ones keep data) and `builder prune -a`, then says what it freed. Under them
//!     `Reti inutilizzate` and `Pulisci tutto` (as `docker system prune -a`: stopped containers,
//!     unused networks and images, build cache; not volumes) work the same.
//!   - `Apri Docker Desktop` opens the Dashboard.
//! - Data: `/info` and `/containers/json?all=1` (and each one up, for when it started), read again
//!   a second after the last of a burst of `/events` (create, start, die, pause, unpause, health,
//!   rename, destroy), so `docker compose up` updates the bar once, and at once when an action of
//!   the popup is done. While Docker is off, its socket is tried every 5 seconds, in-process. CPU
//!   and memory come from `/containers/<id>/stats` only while the popup is open, every few seconds
//!   (each takes a second or two: until then the last ones show); the disk from `/system/df` when
//!   it opens and after an action. Docker Desktop, OrbStack, colima, Rancher Desktop and
//!   `DOCKER_HOST=unix://…` all work.

use std::collections::HashMap;
use std::io::BufRead;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::{Cx, Event, Rx, Widget};
use crate::config::Options;
use crate::gfx::{Rgba, Weight};
use crate::i18n::t;
use crate::net;
use crate::runtime;
use crate::theme::Theme;
use crate::ui::{Align, Justify, Kind, Node};

const DESKTOP: &str = "/Applications/Docker.app";
const WHALE: &str = "/Applications/Docker.app#Whale";
const BLUE: &str = "#2496ed";
const PROJECT: &str = "com.docker.compose.project";
/// The running containers with their uptime and usage: the others show only their state.
const MAX: usize = 24;
const POLL: Duration = Duration::from_secs(5);
/// For an action: a stop waits for the container's own timeout, a clean-up for the disk.
const SLOW: Duration = Duration::from_secs(300);
const MB: f64 = 1_048_576.0;
const TILE: f64 = 160.0;
const GAP: f64 = 8.0;
const WIDE: f64 = TILE * 2.0 + GAP;
/// Inside a tile's padding.
const INNER: f64 = TILE - 20.0;
const BUTTON: f64 = 31.0;
/// The bar and the popup without its containers: they scroll in what's left of the screen.
const AROUND: f64 = 460.0;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Engine {
    os: String,
    version: String,
    cpus: u64,
    /// Bytes.
    memory: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Container {
    id: String,
    name: String,
    /// Its Compose project, or empty.
    project: String,
    image: String,
    /// created, running, paused, restarting, removing, exited or dead.
    state: String,
    /// healthy, unhealthy, starting, or empty without a health check.
    health: String,
    /// Of an exited one.
    exit: Option<i64>,
    ports: Vec<u16>,
    /// Seconds since 1970 (in made-up data, seconds ago).
    started: Option<f64>,
}

impl Container {
    /// Up, as in `docker ps`.
    fn up(&self) -> bool {
        matches!(self.state.as_str(), "running" | "paused" | "restarting")
    }

    /// Whether `op` does something to it.
    fn can(&self, op: Op) -> bool {
        match op {
            Op::Start => matches!(self.state.as_str(), "created" | "exited" | "paused"),
            Op::Stop | Op::Restart => self.up(),
            Op::Remove => self.state != "removing",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Snapshot {
    engine: Engine,
    /// All of them, in the order of the popup (see `sort`); only the first `MAX` up have `started`.
    containers: Vec<Container>,
}

/// Bytes used and bytes that a clean-up frees, of each kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Disk {
    images: (f64, f64),
    containers: (f64, f64),
    volumes: (f64, f64),
    build: (f64, f64),
}

/// Made-up data (see `mock`): the engine, the containers, what they use (by name) and the disk.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    engine: Option<Engine>,
    containers: Vec<Container>,
    /// Name → [CPU %, MB].
    usage: HashMap<String, (f64, f64)>,
    disk: Option<Disk>,
}

/// A button of a container.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Start,
    Stop,
    Restart,
    Remove,
}

/// A clean-up: a kind of the disk, the networks, or all of them.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Prune {
    Images,
    Containers,
    Volumes,
    Build,
    Networks,
    All,
}

impl Prune {
    const ALL: [Prune; 6] = [Prune::Images, Prune::Containers, Prune::Volumes, Prune::Build, Prune::Networks, Prune::All];

    fn key(self) -> &'static str {
        match self {
            Prune::Images => "images",
            Prune::Containers => "containers",
            Prune::Volumes => "volumes",
            Prune::Build => "build",
            Prune::Networks => "networks",
            Prune::All => "all",
        }
    }

    /// The requests, in order: containers first, so that their images and networks are unused
    /// by then, as in `docker system prune`.
    fn targets(self) -> Vec<String> {
        match self {
            Prune::Containers => vec!["/containers/prune".into()],
            // As `-a`: every image without a container, not only the untagged ones
            Prune::Images => vec![format!("/images/prune?filters={}", query(r#"{"dangling":["false"]}"#))],
            // Anonymous volumes only, since Engine 23: named ones keep data
            Prune::Volumes => vec!["/volumes/prune".into()],
            Prune::Build => vec!["/build/prune?all=true".into()],
            Prune::Networks => vec!["/networks/prune".into()],
            Prune::All => [Prune::Containers, Prune::Networks, Prune::Images, Prune::Build].into_iter().flat_map(Prune::targets).collect(),
        }
    }
}

/// What a tile or a button says for a moment, over its state.
#[derive(Clone, Debug, PartialEq)]
enum Note {
    /// A request is out: what it does.
    Busy(&'static str),
    /// The first click of a removal or a clean-up: the second one does it.
    Confirm,
    Failed,
    /// What a clean-up did.
    Done(String),
}

/// The stream of events: closed when the widget goes (a reload), which ends its thread.
struct Closer(UnixStream);

impl Drop for Closer {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

pub struct Docker {
    /// None while Docker isn't running.
    snapshot: Option<Snapshot>,
    /// CPU % and MB, by container name.
    usage: HashMap<String, (f64, f64)>,
    /// None until the popup first opens.
    disk: Option<Disk>,
    socket: Option<PathBuf>,
    checked: bool,
    events: Option<Closer>,
    /// Events in a burst: the last one reads again.
    burst: u64,
    measuring: bool,
    /// `/system/df` is being read; `again` reads it once more after, for an action done meanwhile.
    sizing: bool,
    again: bool,
    /// By container id, or `prune:<kind>`.
    notes: HashMap<String, Note>,
    mock: bool,
}

/// The sockets of the engines, the one of `DOCKER_HOST` first.
fn sockets() -> Vec<PathBuf> {
    let home = crate::config::home();
    let mut list: Vec<PathBuf> = std::env::var("DOCKER_HOST").ok().and_then(|h| h.strip_prefix("unix://").map(PathBuf::from)).into_iter().collect();
    list.extend([
        home.join(".docker/run/docker.sock"),
        PathBuf::from("/var/run/docker.sock"),
        home.join(".orbstack/run/docker.sock"),
        home.join(".colima/default/docker.sock"),
        home.join(".rd/docker.sock"),
    ]);
    list
}

fn get(path: &Path, target: &str, timeout: Duration) -> Option<Value> {
    let response = net::unix_get(path, target, Some(timeout)).ok()?;
    if response.status != 200 {
        return None;
    }
    serde_json::from_str(&response.text().ok()?).ok()
}

/// `method target` for an action: Docker's answer, or its reason when it refuses.
fn call(path: &Path, method: &str, target: &str) -> Result<Value, String> {
    let response = net::unix_request(path, method, target, Some(SLOW)).map_err(|e| e.to_string())?;
    let status = response.status;
    let body: Value = serde_json::from_str(&response.text().unwrap_or_default()).unwrap_or(Value::Null);
    // 304: started or stopped already
    if (200..300).contains(&status) || status == 304 {
        Ok(body)
    } else {
        Err(body["message"].as_str().map(String::from).unwrap_or_else(|| format!("HTTP {status}")))
    }
}

/// Percent-encoded, for a query string.
fn query(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b == b'_' { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// `ghcr.io/org/app:1.2@sha256:…` → `app:1.2`; an image without a tag left only its id.
fn short_image(image: &str) -> String {
    let image = image.split('@').next().unwrap_or(image);
    if let Some(id) = image.strip_prefix("sha256:") {
        return id.chars().take(12).collect();
    }
    image.rsplit('/').next().unwrap_or(image).to_string()
}

/// The health check in the status of `/containers/json`: `Up 2 hours (healthy)`.
fn health(status: &str) -> &'static str {
    if status.contains("(unhealthy)") {
        "unhealthy"
    } else if status.contains("(health: starting)") {
        "starting"
    } else if status.contains("(healthy)") {
        "healthy"
    } else {
        ""
    }
}

/// `Exited (137) 2 hours ago` → 137.
fn exit_code(status: &str) -> Option<i64> {
    status.strip_prefix("Exited (")?.split(')').next()?.parse().ok()
}

/// The order of the popup: by project, those without one last, then by name.
fn sort(containers: &mut [Container]) {
    containers.sort_by(|a, b| (a.project.is_empty(), &a.project, &a.name).cmp(&(b.project.is_empty(), &b.project, &b.name)));
}

/// The engine and all the containers.
fn snapshot(path: &Path) -> Option<Snapshot> {
    let timeout = Duration::from_secs(10);
    let info = get(path, "/info", timeout)?;
    let engine = Engine {
        os: info["OperatingSystem"].as_str().unwrap_or_default().to_string(),
        version: info["ServerVersion"].as_str().unwrap_or_default().to_string(),
        cpus: info["NCPU"].as_u64().unwrap_or(0),
        memory: info["MemTotal"].as_f64().unwrap_or(0.0),
    };
    let list = get(path, "/containers/json?all=1", timeout)?;
    let mut containers: Vec<Container> = list
        .as_array()?
        .iter()
        .map(|c| {
            let status = c["Status"].as_str().unwrap_or_default();
            let mut ports: Vec<u16> = c["Ports"].as_array().into_iter().flatten().filter_map(|p| p["PublicPort"].as_u64()).map(|p| p as u16).collect();
            ports.sort();
            ports.dedup();
            Container {
                id: c["Id"].as_str().unwrap_or_default().to_string(),
                name: c["Names"][0].as_str().unwrap_or_default().trim_start_matches('/').to_string(),
                project: c["Labels"][PROJECT].as_str().unwrap_or_default().to_string(),
                image: short_image(c["Image"].as_str().unwrap_or_default()),
                state: c["State"].as_str().unwrap_or_default().to_string(),
                health: health(status).to_string(),
                exit: exit_code(status),
                ports,
                started: None,
            }
        })
        .collect();
    sort(&mut containers);
    for c in containers.iter_mut().filter(|c| c.up()).take(MAX) {
        c.started = get(path, &format!("/containers/{}/json", c.id), timeout).and_then(|i| i["State"]["StartedAt"].as_str().and_then(crate::time::iso8601));
    }
    Some(Snapshot { engine, containers })
}

/// CPU % (of one core, as `docker stats`) and memory in MB of a container: the engine samples it
/// twice, a second apart.
fn stats(path: &Path, id: &str) -> Option<(f64, f64)> {
    let s = get(path, &format!("/containers/{id}/stats?stream=false"), Duration::from_secs(10))?;
    let num = |v: &Value| v.as_f64().unwrap_or(0.0);
    let cpu = num(&s["cpu_stats"]["cpu_usage"]["total_usage"]) - num(&s["precpu_stats"]["cpu_usage"]["total_usage"]);
    let system = num(&s["cpu_stats"]["system_cpu_usage"]) - num(&s["precpu_stats"]["system_cpu_usage"]);
    let cpus = s["cpu_stats"]["online_cpus"].as_f64().filter(|n| *n > 0.0).unwrap_or(1.0);
    let percent = if system > 0.0 && cpu > 0.0 { cpu / system * cpus * 100.0 } else { 0.0 };
    // Without the page cache, as `docker stats`: cgroup v2, then v1
    let m = &s["memory_stats"];
    let cache = m["stats"]["inactive_file"].as_f64().or(m["stats"]["total_inactive_file"].as_f64()).unwrap_or(0.0);
    let used = (num(&m["usage"]) - cache).max(0.0);
    Some((percent, used / MB))
}

/// A size of `/system/df`, which says -1 for what it didn't measure.
fn bytes_of(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0).max(0.0)
}

/// What `docker system df` says, from `/system/df`: what each kind uses and what a clean-up of it
/// (as `Prune::targets`) frees.
fn disk_usage(df: &Value) -> Disk {
    let items = |key: &str| df[key].as_array().cloned().unwrap_or_default();
    let split = |list: Vec<Value>, size: fn(&Value) -> f64, free: fn(&Value) -> bool| -> (f64, f64) {
        (list.iter().map(size).sum(), list.iter().filter(|i| free(i)).map(size).sum())
    };
    // As the CLI: the layers, less what the images that containers use don't share
    let layers = bytes_of(&df["LayersSize"]);
    let used: f64 = items("Images")
        .iter()
        .filter(|i| i["Containers"].as_i64().unwrap_or(0) > 0)
        .map(|i| (bytes_of(&i["Size"]) - bytes_of(&i["SharedSize"])).max(0.0))
        .sum();
    Disk {
        images: (layers, (layers - used).max(0.0)),
        containers: split(items("Containers"), |c| bytes_of(&c["SizeRw"]), |c| !matches!(c["State"].as_str(), Some("running" | "paused" | "restarting"))),
        volumes: split(
            items("Volumes"),
            |v| bytes_of(&v["UsageData"]["Size"]),
            |v| v["UsageData"]["RefCount"].as_i64() == Some(0) && v["Labels"].get("com.docker.volume.anonymous").is_some(),
        ),
        build: split(items("BuildCache"), |b| bytes_of(&b["Size"]), |b| b["InUse"] != Value::Bool(true)),
    }
}

/// Measuring volumes and the layers of containers walks their files: it can take a while.
fn disk(path: &Path) -> Option<Disk> {
    get(path, "/system/df", Duration::from_secs(60)).map(|df| disk_usage(&df))
}

/// Runs a clean-up: the bytes it freed and the networks it removed, or why it failed.
fn prune(path: &Path, kind: Prune) -> Result<(f64, usize), String> {
    let (mut freed, mut networks) = (0.0, 0);
    for target in kind.targets() {
        let body = call(path, "POST", &target)?;
        freed += bytes_of(&body["SpaceReclaimed"]);
        networks += body["NetworksDeleted"].as_array().map_or(0, Vec::len);
    }
    Ok((freed, networks))
}

fn decimal(v: f64) -> String {
    let s = format!("{v:.1}");
    if crate::i18n::italian() { s.replace('.', ",") } else { s }
}

/// `850 MB`, `3,2 GB`, `640 KB`.
fn size(mb: f64) -> String {
    if mb >= 1024.0 {
        format!("{} GB", decimal(mb / 1024.0))
    } else if mb >= 1.0 || mb == 0.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{:.0} KB", mb * 1024.0)
    }
}

/// `1 attivo`, `3 attivi`.
fn count(n: u64, one: (&str, &str), many: (&str, &str)) -> String {
    let (it, en) = if n == 1 { one } else { many };
    format!("{n} {}", t(it, en))
}

/// `da 2 h`: the largest unit only.
fn up_for(started: f64) -> String {
    let up = crate::time::now() - started;
    let span = if up >= 86_400.0 {
        format!("{:.0} {}", (up / 86_400.0).floor(), t("g", "d"))
    } else if up >= 3600.0 {
        format!("{:.0} h", (up / 3600.0).floor())
    } else if up >= 60.0 {
        format!("{:.0} min", (up / 60.0).floor())
    } else {
        return t("appena avviato", "just started").into();
    };
    t(&format!("da {span}"), &format!("up {span}")).to_string()
}

/// The look of a container: its icon, the icon's color, and what the tile says instead of the
/// ports and the uptime.
fn look(c: &Container, th: &Theme) -> (&'static str, Rgba, Option<(String, Rgba)>) {
    let says = |it: &str, en: &str, color: Rgba| Some((t(it, en).to_string(), color));
    match (c.state.as_str(), c.health.as_str()) {
        ("paused", _) => ("pause.circle.fill", th.yellow, says("In pausa", "Paused", th.yellow)),
        ("restarting", _) => ("arrow.clockwise.circle.fill", th.red, says("In riavvio", "Restarting", th.red)),
        ("running", "unhealthy") => ("exclamationmark.circle.fill", th.red, says("Health check fallito", "Health check failing", th.red)),
        ("running", "starting") => ("shippingbox.fill", th.yellow, says("In avvio", "Starting", th.yellow)),
        ("running", _) => ("shippingbox.fill", th.green, None),
        ("created", _) => ("shippingbox", th.subtext, says("Creato", "Created", th.subtext)),
        ("removing", _) => ("trash", th.subtext, says("Eliminazione…", "Removing…", th.subtext)),
        ("dead", _) => ("xmark.circle.fill", th.red, says("Guasto", "Dead", th.red)),
        // 130, 137 and 143: interrupted, killed or stopped by `docker stop`
        _ => match c.exit {
            Some(code) if ![0, 130, 137, 143].contains(&code) => {
                ("exclamationmark.circle.fill", th.red, Some((t(&format!("Uscito con codice {code}"), &format!("Exited with code {code}")).to_string(), th.red)))
            }
            _ => ("shippingbox", th.subtext, says("Fermo", "Stopped", th.subtext)),
        },
    }
}

/// What a clean-up did, for its note.
fn done_text(kind: Prune, freed: f64, networks: usize) -> String {
    match kind {
        Prune::Networks if networks == 0 => t("Niente da eliminare", "Nothing to remove").into(),
        Prune::Networks => count(networks as u64, ("rete eliminata", "network removed"), ("reti eliminate", "networks removed")),
        _ if freed == 0.0 => t("Niente da liberare", "Nothing to free").into(),
        _ => t(&format!("Liberati {}", size(freed / MB)), &format!("{} freed", size(freed / MB))).to_string(),
    }
}

impl Docker {
    pub fn new(_options: &Options) -> Docker {
        let mock = crate::mock::load::<Mock>("docker");
        let mut d = Docker {
            snapshot: None,
            usage: HashMap::new(),
            disk: None,
            socket: None,
            checked: false,
            events: None,
            burst: 0,
            measuring: false,
            sizing: false,
            again: false,
            notes: HashMap::new(),
            mock: mock.is_some(),
        };
        if let Some(m) = mock {
            let now = crate::time::now();
            let mut containers = m.containers;
            for c in &mut containers {
                c.started = c.started.map(|ago| now - ago);
                // The notes are by id
                if c.id.is_empty() {
                    c.id = c.name.clone();
                }
            }
            sort(&mut containers);
            d.snapshot = m.engine.map(|engine| Snapshot { engine, containers });
            d.usage = m.usage;
            d.disk = m.disk;
            d.checked = true;
        }
        d
    }

    /// Finds the engine, reads it, then follows its events until it stops; again and again, until
    /// a reload.
    fn watch(&mut self, cx: &mut Cx) {
        let handle = cx.handle::<Docker>();
        runtime::spawn("docker", move || {
            while handle.alive() {
                let since = crate::time::now() as i64;
                let found = sockets().into_iter().find_map(|path| snapshot(&path).map(|s| (path, s)));
                let Some((path, snap)) = found else {
                    handle.send(|w, cx| w.update(None, None, cx));
                    std::thread::sleep(POLL);
                    continue;
                };
                let socket = path.clone();
                handle.send(move |w, cx| w.update(Some(socket), Some(snap), cx));
                let filters = query(r#"{"type":["container"],"event":["create","start","die","pause","unpause","health_status","rename","destroy"]}"#);
                let Ok(response) = net::unix_get(&path, &format!("/events?since={since}&filters={filters}"), None) else {
                    std::thread::sleep(POLL);
                    continue;
                };
                let closer = response.stream.try_clone().ok();
                handle.send(move |w, _| w.events = closer.map(Closer));
                for line in response.body.lines() {
                    if line.is_err() || !handle.alive() {
                        break;
                    }
                    handle.send(|w, cx| w.changed(cx));
                }
                // Docker stopped, or a reload closed the stream
                handle.send(|w, cx| {
                    w.events = None;
                    w.changed(cx);
                });
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }

    fn update(&mut self, socket: Option<PathBuf>, snapshot: Option<Snapshot>, cx: &mut Cx) {
        let changed = !self.checked || self.snapshot != snapshot;
        self.checked = true;
        self.socket = socket;
        match &snapshot {
            // The notes of containers that are gone go with them
            Some(s) => self.notes.retain(|key, _| key.starts_with("prune:") || s.containers.iter().any(|c| c.id == *key)),
            None => {
                self.usage.clear();
                self.notes.clear();
                if cx.popup_open {
                    cx.close_popup();
                }
            }
        }
        self.snapshot = snapshot;
        if changed {
            cx.redraw();
        }
    }

    /// An event: read again a second after the last one of a burst.
    fn changed(&mut self, cx: &mut Cx) {
        self.burst += 1;
        let burst = self.burst;
        cx.after::<Docker>(1.0, move |w, cx| {
            if w.burst != burst {
                return;
            }
            let Some(path) = w.socket.clone() else { return };
            cx.background::<Docker, Option<Snapshot>>(move || snapshot(&path), |w, snap, cx| {
                let socket = snap.as_ref().and(w.socket.clone());
                w.update(socket, snap, cx);
            });
        });
    }

    /// CPU and memory of the running containers, while the popup is open.
    fn measure(&mut self, cx: &mut Cx) {
        if self.mock || self.measuring || !cx.popup_open {
            return;
        }
        let (Some(path), Some(snap)) = (self.socket.clone(), &self.snapshot) else { return };
        let shown: Vec<(String, String)> = snap.containers.iter().filter(|c| c.state == "running").take(MAX).map(|c| (c.id.clone(), c.name.clone())).collect();
        if shown.is_empty() {
            return;
        }
        self.measuring = true;
        cx.background::<Docker, HashMap<String, (f64, f64)>>(
            move || {
                std::thread::scope(|scope| {
                    let jobs: Vec<_> = shown.iter().map(|(id, name)| (name, scope.spawn(|| stats(&path, id)))).collect();
                    jobs.into_iter().filter_map(|(name, job)| Some((name.clone(), job.join().ok()??))).collect()
                })
            },
            |w, usage, cx| {
                w.measuring = false;
                w.usage.extend(usage);
                cx.redraw();
                if cx.popup_open {
                    cx.after::<Docker>(1.0, |w, cx| w.measure(cx));
                }
            },
        );
    }

    /// The disk, when the popup opens and after an action.
    fn size_up(&mut self, cx: &mut Cx) {
        if self.mock {
            return;
        }
        if self.sizing {
            self.again = true;
            return;
        }
        let Some(path) = self.socket.clone() else { return };
        self.sizing = true;
        cx.background::<Docker, Option<Disk>>(move || disk(&path), |w, disk, cx| {
            w.sizing = false;
            if disk.is_some() && w.disk != disk {
                w.disk = disk;
                cx.redraw();
            }
            if std::mem::take(&mut w.again) {
                w.size_up(cx);
            }
        });
    }

    /// A note that goes by itself after `secs`, unless replaced.
    fn note_for(&mut self, key: &str, note: Note, secs: f64, cx: &mut Cx) {
        self.notes.insert(key.to_string(), note.clone());
        let key = key.to_string();
        cx.after::<Docker>(secs, move |w, cx| {
            if w.notes.get(&key) == Some(&note) {
                w.notes.remove(&key);
                cx.redraw();
            }
        });
        cx.redraw();
    }

    /// A button of a container. Removing asks for a second click.
    fn act(&mut self, id: &str, op: Op, cx: &mut Cx) {
        let Some(c) = self.snapshot.as_ref().and_then(|s| s.containers.iter().find(|c| c.id == id)) else { return };
        if !c.can(op) || matches!(self.notes.get(id), Some(Note::Busy(_))) {
            return;
        }
        if op == Op::Remove && self.notes.get(id) != Some(&Note::Confirm) {
            return self.note_for(id, Note::Confirm, 4.0, cx);
        }
        let (method, target, busy) = match op {
            Op::Start if c.state == "paused" => ("POST", format!("/containers/{id}/unpause"), t("Riprende…", "Resuming…")),
            Op::Start => ("POST", format!("/containers/{id}/start"), t("Si avvia…", "Starting…")),
            Op::Stop => ("POST", format!("/containers/{id}/stop"), t("Si ferma…", "Stopping…")),
            Op::Restart => ("POST", format!("/containers/{id}/restart"), t("Si riavvia…", "Restarting…")),
            // Running too, as `docker rm -f`; its anonymous volumes stay, for a clean-up
            Op::Remove => ("DELETE", format!("/containers/{id}?force=true"), t("Eliminazione…", "Removing…")),
        };
        // Made-up data: nothing happens
        if self.mock {
            self.notes.remove(id);
            return cx.redraw();
        }
        let Some(path) = self.socket.clone() else { return };
        let (key, name) = (id.to_string(), c.name.clone());
        self.notes.insert(key.clone(), Note::Busy(busy));
        cx.background::<Docker, Result<Value, String>>(
            move || call(&path, method, &target),
            move |w, result, cx| w.finish(key, result.map(|_| None).map_err(|why| format!("{name}: {why}")), cx),
        );
        cx.redraw();
    }

    /// ▶ ■ ↻ of a Compose project: the same on each of its containers that it does something to.
    fn act_project(&mut self, project: &str, op: Op, cx: &mut Cx) {
        let ids: Vec<String> = self.snapshot.iter().flat_map(|s| &s.containers).filter(|c| c.project == project && c.can(op)).map(|c| c.id.clone()).collect();
        for id in ids {
            self.act(&id, op, cx);
        }
    }

    /// A tile of the disk or a clean-up button: the second click cleans up.
    fn clean(&mut self, kind: Prune, cx: &mut Cx) {
        let key = format!("prune:{}", kind.key());
        match self.notes.get(&key) {
            Some(Note::Busy(_)) => return,
            Some(Note::Confirm) => {}
            _ => return self.note_for(&key, Note::Confirm, 4.0, cx),
        }
        if self.mock {
            self.notes.remove(&key);
            return cx.redraw();
        }
        let Some(path) = self.socket.clone() else { return };
        self.notes.insert(key.clone(), Note::Busy(t("Pulizia…", "Cleaning up…")));
        cx.background::<Docker, Result<(f64, usize), String>>(
            move || prune(&path, kind),
            move |w, result, cx| {
                let result = result.map(|(freed, networks)| Some(Note::Done(done_text(kind, freed, networks))));
                w.finish(key, result.map_err(|why| format!("prune {}: {why}", kind.key())), cx);
            },
        );
        cx.redraw();
    }

    /// An action is done: `Ok` with what to say for a while, if anything. The containers are read
    /// again at once rather than after the events, and the busy note stays until then, so that the
    /// tile doesn't show the old state in between.
    fn finish(&mut self, key: String, result: Result<Option<Note>, String>, cx: &mut Cx) {
        match result {
            Err(why) => {
                crate::log(&format!("docker: {why}"));
                self.note_for(&key, Note::Failed, 5.0, cx);
            }
            Ok(note) => {
                if let Some(note) = note {
                    self.note_for(&key, note, 5.0, cx);
                }
                if let Some(path) = self.socket.clone() {
                    cx.background::<Docker, Option<Snapshot>>(move || snapshot(&path), move |w, snap, cx| {
                        if matches!(w.notes.get(&key), Some(Note::Busy(_))) {
                            w.notes.remove(&key);
                            cx.redraw();
                        }
                        let socket = snap.as_ref().and(w.socket.clone());
                        w.update(socket, snap, cx);
                    });
                }
                if cx.popup_open {
                    self.size_up(cx);
                }
            }
        }
    }

    // Drawing

    /// The engine, two tiles wide, with what all the containers use.
    fn engine(&self, rx: &Rx, snap: &Snapshot, title: &str) -> Node {
        let th = rx.theme;
        let e = &snap.engine;
        let blue = Rgba::parse(BLUE).unwrap_or(th.primary);
        let icon = if Path::new(DESKTOP).exists() { Node::image(WHALE, 0.0, 16.0, Some(blue)) } else { Node::symbol("shippingbox.fill", 18.0, Weight::Semibold, blue) };
        let detail = format!("Engine {} · {} · {}", e.version, count(e.cpus, ("CPU", "CPU"), ("CPU", "CPUs")), size(e.memory / MB));
        let mut row = vec![
            Node::row(vec![icon]).width(26.0).justify(Justify::Center).key("icon"),
            Node::column(vec![Node::text(title, th.font(), th.text).key("name"), Node::text(detail, th.small_font(), th.subtext).key("detail")]).gap(2.0).key("names"),
            Node::spacer(),
        ];
        let used: Vec<&(f64, f64)> = snap.containers.iter().filter(|c| c.state == "running").filter_map(|c| self.usage.get(&c.name)).collect();
        if !used.is_empty() {
            let (cpu, mb) = used.iter().fold((0.0, 0.0), |(cpu, mb), (c, m)| (cpu + c, mb + m));
            row.push(
                Node::column(vec![Node::text(format!("{}%", decimal(cpu)), th.font_with(Weight::Medium, 15.0), th.text).key("cpu"), Node::text(size(mb), th.small_font(), th.subtext).key("memory")])
                    .gap(2.0)
                    .align(Align::End)
                    .key("usage"),
            );
        }
        let up = snap.containers.iter().any(Container::up);
        Node::row(row).gap(10.0).padding(10.0, 10.0, 10.0, 10.0).width(WIDE).radius(10.0).background(th.tile(blue, up)).key("engine")
    }

    /// The title of a project, or of the containers without one: how many run, and ▶ ■ ↻ for a
    /// project.
    fn title(&self, rx: &Rx, group: &[Container], first: bool) -> Node {
        let th = rx.theme;
        let project = &group[0].project;
        let text = if project.is_empty() { t("Container", "Containers") } else { project.as_str() };
        let running = group.iter().filter(|c| c.up()).count() as u64;
        let mut row = vec![
            Node::text(text, th.font(), th.text).max_width(WIDE - 150.0).key("name"),
            Node::text(count(running, ("attivo", "running"), ("attivi", "running")), th.small_font(), th.subtext).key("count"),
            Node::spacer(),
        ];
        if !project.is_empty() {
            for (symbol, op, name) in [("play.fill", Op::Start, "start"), ("stop.fill", Op::Stop, "stop"), ("arrow.clockwise", Op::Restart, "restart")] {
                let enabled = group.iter().any(|c| c.can(op));
                let mut button = Node::row(vec![Node::symbol(symbol, 10.0, Weight::Bold, if enabled { th.text } else { th.overlay })])
                    .size(28.0, 22.0)
                    .justify(Justify::Center)
                    .radius(6.0)
                    .background(th.surface.faded(if enabled { 0.7 } else { 0.35 }))
                    .key(name);
                if enabled {
                    button = button.click(rx.action(&format!("project:{name}:{project}")));
                }
                row.push(button);
            }
        }
        Node::row(row).gap(6.0).padding(if first { 2.0 } else { 6.0 }, 0.0, 0.0, 10.0).key(format!("title-{project}"))
    }

    /// A container: its state, usage, name, image, ports and buttons.
    fn tile(&self, rx: &Rx, c: &Container, name: &str) -> Node {
        let th = rx.theme;
        let small = th.small_font();
        let (symbol, color, state) = look(c, th);
        let note = self.notes.get(&c.id);
        let busy = matches!(note, Some(Note::Busy(_)));
        let state = match note {
            Some(Note::Busy(text)) => Some((text.to_string(), th.yellow)),
            Some(Note::Confirm) => Some((t("Fai clic per eliminare", "Click to remove").to_string(), th.red)),
            Some(Note::Failed) => Some((t("Non riuscito", "Failed").to_string(), th.red)),
            Some(Note::Done(text)) => Some((text.clone(), th.green)),
            None => state,
        };
        let status = match state {
            Some((text, color)) => vec![Node::text(text, small.clone(), color).max_width(INNER).key("state")],
            None => {
                // The ports open in the browser
                let mut line: Vec<Node> = c
                    .ports
                    .iter()
                    .take(2)
                    .map(|p| {
                        Node::row(vec![Node::text(format!(":{p}"), small.clone(), th.primary)])
                            .padding(0.0, 4.0, 0.0, 4.0)
                            .height(16.0)
                            .radius(4.0)
                            .background(th.primary.faded(0.14))
                            .click(rx.action(&format!("port:{p}")))
                            .key(format!("port-{p}"))
                    })
                    .collect();
                if c.ports.len() > 2 {
                    line.push(Node::text(format!("+{}", c.ports.len() - 2), small.clone(), th.subtext).key("more"));
                }
                let up = c.started.map(up_for).unwrap_or_else(|| t("In esecuzione", "Running").to_string());
                line.push(Node::text(up, small.clone(), th.subtext).key("up"));
                line
            }
        };
        let usage = self.usage.get(&c.name).filter(|_| c.state == "running").map(|(cpu, mb)| format!("{}% · {}", decimal(*cpu), size(*mb))).unwrap_or_default();
        let button = |symbol: &str, op: Op, action: &str| {
            let enabled = c.can(op) && !busy;
            let confirm = op == Op::Remove && note == Some(&Note::Confirm);
            let color = if confirm { th.red } else if enabled { th.text } else { th.overlay };
            let background = if confirm { th.red.faded(0.22) } else { th.surface.faded(if enabled { 0.7 } else { 0.35 }) };
            let mut b = Node::row(vec![Node::symbol(symbol, 11.0, Weight::Bold, color)]).size(BUTTON, 24.0).justify(Justify::Center).radius(6.0).background(background).key(action);
            if enabled {
                b = b.click(rx.action(&format!("{action}:{}", c.id)));
            }
            b
        };
        Node::column(vec![
            Node::row(vec![Node::symbol(symbol, 18.0, Weight::Semibold, color).key("icon"), Node::spacer(), Node::text(usage, small.clone(), th.subtext).key("usage")]).height(22.0).key("top"),
            Node::text(name, th.font(), th.text).max_width(INNER).key("name"),
            Node::text(&c.image, small.clone(), th.subtext).max_width(INNER).key("image"),
            Node::row(status).gap(4.0).height(16.0).key("status"),
            Node::row(vec![button("play.fill", Op::Start, "start"), button("stop.fill", Op::Stop, "stop"), button("arrow.clockwise", Op::Restart, "restart"), button("trash", Op::Remove, "remove")])
                .justify(Justify::Between)
                .padding(6.0, 0.0, 0.0, 0.0)
                .key("buttons"),
        ])
        .gap(2.0)
        .padding(10.0, 10.0, 10.0, 10.0)
        .width(TILE)
        .radius(10.0)
        .background(th.tile(color, c.up()))
        .key(c.id.clone())
    }

    /// What a clean-up says over its state, if anything.
    fn prune_note(&self, rx: &Rx, key: &str, confirm: &str) -> Option<(String, Rgba)> {
        let th = rx.theme;
        Some(match self.notes.get(key)? {
            Note::Busy(text) => (text.to_string(), th.yellow),
            Note::Confirm => (confirm.to_string(), th.red),
            Note::Failed => (t("Non riuscito", "Failed").to_string(), th.red),
            Note::Done(text) => (text.clone(), th.green),
        })
    }

    /// A kind of the disk: its size and what a clean-up frees, which a click (and a second one)
    /// does.
    fn disk_tile(&self, rx: &Rx, kind: Prune) -> Node {
        let th = rx.theme;
        let disk = self.disk.as_ref();
        let (symbol, name, sizes) = match kind {
            Prune::Images => ("square.stack.3d.up.fill", t("Immagini", "Images"), disk.map(|d| d.images)),
            Prune::Containers => ("shippingbox.fill", t("Container", "Containers"), disk.map(|d| d.containers)),
            Prune::Volumes => ("externaldrive.fill", t("Volumi", "Volumes"), disk.map(|d| d.volumes)),
            _ => ("hammer.fill", t("Cache di build", "Build Cache"), disk.map(|d| d.build)),
        };
        let key = format!("prune:{}", kind.key());
        let (text, color) = self.prune_note(rx, &key, t("Fai clic per pulire", "Click to clean up")).unwrap_or_else(|| match sizes {
            None => (t("Calcolo…", "Measuring…").to_string(), th.subtext),
            Some((_, free)) if free > 0.0 => (t(&format!("{} liberabili", size(free / MB)), &format!("{} reclaimable", size(free / MB))).to_string(), th.text),
            Some(_) => (t("Niente da liberare", "Nothing to free").to_string(), th.subtext),
        });
        let free = sizes.is_some_and(|(_, free)| free > 0.0);
        let mut tile = Node::column(vec![
            Node::row(vec![
                Node::symbol(symbol, 18.0, Weight::Semibold, if free { th.primary } else { th.subtext }).key("icon"),
                Node::spacer(),
                Node::text(sizes.map(|(total, _)| size(total / MB)).unwrap_or_default(), th.font_with(Weight::Medium, 15.0), th.text).key("size"),
            ])
            .height(22.0)
            .key("top"),
            Node::text(name, th.font(), th.text).key("name"),
            Node::text(text, th.small_font(), color).max_width(INNER).key("state"),
        ])
        .gap(2.0)
        .padding(10.0, 10.0, 10.0, 10.0)
        .width(TILE)
        .radius(10.0)
        .background(th.tile(th.primary, false))
        .key(key.clone());
        if sizes.is_some() && !matches!(self.notes.get(&key), Some(Note::Busy(_))) {
            tile = tile.click(rx.action(&key));
        }
        tile
    }

    /// The clean-up of the networks, or of everything.
    fn prune_button(&self, rx: &Rx, kind: Prune) -> Node {
        let th = rx.theme;
        let key = format!("prune:{}", kind.key());
        let (symbol, label) = match kind {
            Prune::Networks => ("network", t("Reti inutilizzate", "Unused Networks")),
            _ => ("sparkles", t("Pulisci tutto", "Clean Up All")),
        };
        let busy = matches!(self.notes.get(&key), Some(Note::Busy(_)));
        let (symbol, color, label, text_color) = match self.prune_note(rx, &key, t("Fai clic di nuovo", "Click again")) {
            Some((text, color)) => {
                let symbol = match self.notes.get(&key) {
                    Some(Note::Busy(_)) => "hourglass",
                    Some(Note::Done(_)) => "checkmark",
                    Some(Note::Failed) => "exclamationmark.circle.fill",
                    _ => symbol,
                };
                (symbol, color, text, if color == th.green { th.text } else { color })
            }
            None => (symbol, th.primary, label.to_string(), th.text),
        };
        let mut button = Node::row(vec![Node::symbol(symbol, 11.0, Weight::Bold, color).key("icon"), Node::text(label, th.font_with(Weight::Bold, 12.0), text_color).max_width(INNER - 10.0).key("label")])
            .gap(6.0)
            .justify(Justify::Center)
            .size(TILE, 30.0)
            .radius(15.0)
            .background(th.surface.faded(0.7))
            .key(key.clone());
        if !busy {
            button = button.click(rx.action(&key));
        }
        button
    }
}

impl Widget for Docker {
    fn start(&mut self, cx: &mut Cx) {
        if !self.mock {
            self.watch(cx);
        }
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let snap = self.snapshot.as_ref()?;
        let up: Vec<&Container> = snap.containers.iter().filter(|c| c.up()).collect();
        let failing = up.iter().any(|c| c.state == "restarting" || c.health == "unhealthy");
        let color = if failing { th.red } else if up.is_empty() { th.subtext } else { th.text };
        let icon = if Path::new(DESKTOP).exists() {
            Node::image(WHALE, 0.0, 12.0, Some(Rgba::parse(BLUE).unwrap_or(th.primary)))
        } else {
            Node::symbol("shippingbox.fill", th.icon_size, Weight::Semibold, th.text)
        };
        Some(rx.pill(vec![icon.key("icon"), Node::text(up.len().to_string(), th.font(), color).key("count")]).click(rx.toggle_popup()))
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let snap = self.snapshot.as_ref()?;
        let title = match snap.engine.os.as_str() {
            os @ ("Docker Desktop" | "OrbStack") => os,
            _ => "Docker",
        };
        let mut rows = vec![self.engine(rx, snap, title)];

        // The containers, by project, two in a row
        let mut list: Vec<Node> = Vec::new();
        for group in snap.containers.chunk_by(|a, b| a.project == b.project) {
            list.push(self.title(rx, group, list.is_empty()));
            let prefix = format!("{}-", group[0].project);
            for pair in group.chunks(2) {
                let tiles = pair.iter().map(|c| self.tile(rx, c, c.name.strip_prefix(&prefix).filter(|n| !n.is_empty()).unwrap_or(&c.name))).collect();
                list.push(Node::row(tiles).gap(GAP).align(Align::Stretch).key(format!("row-{}", pair[0].id)));
            }
        }
        if list.is_empty() {
            list.push(Node::row(vec![Node::text(t("Nessun container", "No containers"), th.font(), th.subtext)]).padding(6.0, 10.0, 6.0, 10.0).key("none"));
        }
        let height = (rx.display.frame.size.height - AROUND).max(TILE * 1.5);
        rows.push(Node::new(Kind::Scroll { max_height: height, start: None }).width(WIDE).key("containers").push(Node::column(list).gap(GAP)));

        // The disk and the clean-ups
        rows.push(Node::row(vec![Node::text(t("Spazio su disco", "Disk Usage"), th.font(), th.text)]).height(24.0).padding(6.0, 0.0, 0.0, 10.0).key("disk-title"));
        rows.push(Node::row(vec![self.disk_tile(rx, Prune::Images), self.disk_tile(rx, Prune::Containers)]).gap(GAP).align(Align::Stretch).key("disk-1"));
        rows.push(Node::row(vec![self.disk_tile(rx, Prune::Volumes), self.disk_tile(rx, Prune::Build)]).gap(GAP).align(Align::Stretch).key("disk-2"));
        rows.push(Node::row(vec![self.prune_button(rx, Prune::Networks), self.prune_button(rx, Prune::All)]).gap(GAP).key("prune"));

        if title == "Docker Desktop" && Path::new(DESKTOP).exists() {
            rows.push(rx.row_button(vec![Node::text(t("Apri Docker Desktop", "Open Docker Desktop"), th.font(), th.primary)], vec![], rx.action("open")).key("open"));
        }
        Some(rx.popup(WIDE, rows).gap(GAP).padding(8.0, 8.0, 6.0, 8.0))
    }

    fn ready(&self) -> bool {
        self.checked
    }

    fn action(&mut self, name: &str, _value: Option<f64>, cx: &mut Cx) {
        let (kind, rest) = name.split_once(':').unwrap_or((name, ""));
        let op = |s: &str| match s {
            "start" => Some(Op::Start),
            "stop" => Some(Op::Stop),
            "restart" => Some(Op::Restart),
            "remove" => Some(Op::Remove),
            _ => None,
        };
        match kind {
            "open" | "port" => {
                cx.close_popup();
                if self.mock {
                    return;
                }
                if kind == "open" {
                    runtime::open_app("com.docker.docker");
                } else {
                    runtime::open(&format!("http://localhost:{rest}"));
                }
            }
            "prune" => {
                if let Some(p) = Prune::ALL.into_iter().find(|p| p.key() == rest) {
                    self.clean(p, cx);
                }
            }
            "project" => {
                if let Some((o, project)) = rest.split_once(':')
                    && let Some(o) = op(o)
                {
                    self.act_project(project, o, cx);
                }
            }
            _ => {
                if let Some(o) = op(kind) {
                    self.act(rest, o, cx);
                }
            }
        }
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        match event {
            Event::PopupOpened => {
                self.measure(cx);
                self.size_up(cx);
            }
            Event::PopupClosed => self.notes.retain(|_, n| *n != Note::Confirm),
            Event::Wake => self.changed(cx),
            Event::Trigger { name, .. } if name == "docker_update" => self.changed(cx),
            _ => {}
        }
    }

    crate::any_widget!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn images() {
        assert_eq!(short_image("ghcr.io/org/app:1.2@sha256:abc"), "app:1.2");
        assert_eq!(short_image("postgres:16"), "postgres:16");
        assert_eq!(short_image("sha256:0123456789abcdef0123"), "0123456789ab");
        assert_eq!(size(512.0), "512 MB");
        assert_eq!(size(0.5), "512 KB");
        assert!(size(3276.8).starts_with('3'));
    }

    #[test]
    fn states() {
        assert_eq!(health("Up 2 hours (healthy)"), "healthy");
        assert_eq!(health("Up 5 seconds (health: starting)"), "starting");
        assert_eq!(exit_code("Exited (137) 2 hours ago"), Some(137));
        assert_eq!(exit_code("Up 2 hours"), None);
        let c = |state: &str| Container { state: state.into(), ..Default::default() };
        assert!(c("exited").can(Op::Start) && !c("exited").can(Op::Stop));
        assert!(c("paused").can(Op::Start) && c("paused").can(Op::Stop));
        assert!(!c("running").can(Op::Start) && c("running").can(Op::Restart) && c("running").can(Op::Remove));
    }

    #[test]
    fn order() {
        let c = |name: &str, project: &str| Container { name: name.into(), project: project.into(), ..Default::default() };
        let mut list = vec![c("mailpit", ""), c("shop-web-1", "shop"), c("api-1", "api"), c("shop-db-1", "shop")];
        sort(&mut list);
        let names: Vec<&str> = list.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["api-1", "shop-db-1", "shop-web-1", "mailpit"]);
    }

    #[test]
    fn clean_ups() {
        assert_eq!(Prune::All.targets()[0], "/containers/prune");
        assert_eq!(Prune::All.targets().len(), 4);
        assert_eq!(Prune::Images.targets()[0], "/images/prune?filters=%7B%22dangling%22%3A%5B%22false%22%5D%7D");
        let df = json!({
            "LayersSize": 1000,
            "Images": [{"Size": 600, "SharedSize": 100, "Containers": 1}, {"Size": 400, "SharedSize": 100, "Containers": 0}],
            "Containers": [{"SizeRw": 10, "State": "running"}, {"SizeRw": 5, "State": "exited"}],
            "Volumes": [
                {"UsageData": {"Size": 50, "RefCount": 0}, "Labels": {"com.docker.volume.anonymous": ""}},
                {"UsageData": {"Size": 70, "RefCount": 0}, "Labels": null},
                {"UsageData": {"Size": -1, "RefCount": 1}}
            ],
            "BuildCache": [{"Size": 30, "InUse": false}, {"Size": 20, "InUse": true}]
        });
        let d = disk_usage(&df);
        assert_eq!(d.images, (1000.0, 500.0));
        assert_eq!(d.containers, (15.0, 5.0));
        assert_eq!(d.volumes, (120.0, 50.0));
        assert_eq!(d.build, (50.0, 30.0));
    }
}
